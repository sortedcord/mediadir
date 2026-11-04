//! analytics.db: schema, the single writer thread (batched inserts, session persistence) and
//! retention. Separate SQLite file so the main database and its backups stay lean.

use super::tracker::{FileMeta, PlayEvent, PlayRow, ReqEvent, Tracker};
use crate::db::Db;
use crate::error::AppResult;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS requests (
    id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, grp TEXT NOT NULL, route TEXT NOT NULL, method TEXT NOT NULL,
    status INTEGER NOT NULL, bytes INTEGER NOT NULL, dur_ms INTEGER NOT NULL,
    device TEXT NOT NULL, user TEXT, item_id TEXT, file_id TEXT
);
CREATE INDEX IF NOT EXISTS requests_ts ON requests(ts);
CREATE TABLE IF NOT EXISTS requests_hourly (
    hour INTEGER NOT NULL, grp TEXT NOT NULL, route TEXT NOT NULL, status_class TEXT NOT NULL,
    device TEXT NOT NULL, user TEXT NOT NULL DEFAULT '',
    requests INTEGER NOT NULL, bytes INTEGER NOT NULL, dur_ms_sum INTEGER NOT NULL,
    PRIMARY KEY (hour, grp, route, status_class, device, user)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS snapshots (ts INTEGER PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS devices (
    id TEXT PRIMARY KEY, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
    requests INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0,
    user_agent TEXT, ip TEXT, last_user TEXT
);
CREATE TABLE IF NOT EXISTS device_users (
    device TEXT NOT NULL, user TEXT NOT NULL, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
    requests INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (device, user)
);
CREATE INDEX IF NOT EXISTS device_users_user ON device_users(user);
CREATE TABLE IF NOT EXISTS playback_sessions (
    id TEXT PRIMARY KEY, device TEXT NOT NULL, user TEXT, file_id TEXT NOT NULL, item_id TEXT NOT NULL,
    item_kind TEXT NOT NULL DEFAULT '', started_at INTEGER NOT NULL, last_at INTEGER NOT NULL, ended_at INTEGER,
    source TEXT NOT NULL, state TEXT NOT NULL, bytes INTEGER NOT NULL DEFAULT 0, requests INTEGER NOT NULL DEFAULT 0,
    watched_s REAL NOT NULL DEFAULT 0, inferred_watched_s REAL NOT NULL DEFAULT 0,
    max_position_s REAL NOT NULL DEFAULT 0, position_s REAL NOT NULL DEFAULT 0,
    duration_s REAL, file_size INTEGER NOT NULL DEFAULT 0, ranges TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS playback_started ON playback_sessions(started_at);
CREATE INDEX IF NOT EXISTS playback_active ON playback_sessions(ended_at);
CREATE INDEX IF NOT EXISTS playback_device ON playback_sessions(device);
CREATE TABLE IF NOT EXISTS device_sessions (
    id TEXT PRIMARY KEY, device TEXT NOT NULL, user TEXT, started_at INTEGER NOT NULL, last_at INTEGER NOT NULL,
    ended_at INTEGER, requests INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS dsessions_started ON device_sessions(started_at);
CREATE INDEX IF NOT EXISTS dsessions_active ON device_sessions(ended_at);
CREATE INDEX IF NOT EXISTS dsessions_device ON device_sessions(device);
"#;

#[derive(Clone)]
pub struct AnalyticsDb {
    pool: r2d2::Pool<SqliteConnectionManager>,
}

impl AnalyticsDb {
    pub fn open(path: &Path) -> AppResult<Self> {
        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.pragma_update(None, "synchronous", "NORMAL")?;
            c.busy_timeout(Duration::from_secs(10))?;
            Ok(())
        });
        let pool = r2d2::Pool::builder().max_size(4).build(manager)?;
        pool.get()?.execute_batch(SCHEMA)?;
        Ok(AnalyticsDb { pool })
    }

    pub fn conn(&self) -> AppResult<r2d2::PooledConnection<SqliteConnectionManager>> {
        Ok(self.pool.get()?)
    }

    pub async fn call<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&Connection) -> AppResult<R> + Send + 'static,
        R: Send + 'static,
    {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            f(&conn)
        })
        .await?
    }
}

pub fn meta_get(c: &Connection, key: &str) -> Option<String> {
    c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).ok()
}

pub fn meta_set(c: &Connection, key: &str, v: &str) -> rusqlite::Result<()> {
    c.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", [key, v]).map(|_| ())
}

pub enum Msg {
    StreamStart { ts: i64, device: String, user: Option<String>, file_id: String },
    Req { ev: ReqEvent, ua: String, ip: String },
    Play {
        ts: i64,
        device: String,
        user: Option<String>,
        ua: String,
        ip: String,
        file_id: String,
        meta: FileMeta,
        ev: PlayEvent,
        pos: f64,
        dur: Option<f64>,
        reply: tokio::sync::oneshot::Sender<PlayRow>,
    },
}

#[derive(Default)]
struct DevAgg {
    first: i64,
    last: i64,
    requests: u64,
    bytes: u64,
    ua: String,
    ip: String,
    user: Option<String>,
}

type HourKey = (i64, String, String, String, String, String);

pub struct Writer {
    db: AnalyticsDb,
    main: Db,
    tracker: Tracker,
    metas: HashMap<String, FileMeta>,
    pending: Vec<ReqEvent>,
    hourly: HashMap<HourKey, (u64, u64, u64)>,
    devices: HashMap<String, DevAgg>,
    device_users: HashMap<(String, String), DevAgg>,
    force: bool,
}

pub fn status_class(status: u16) -> String {
    format!("{}xx", status / 100)
}

impl Writer {
    pub fn new(db: AnalyticsDb, main: Db, play_idle: i64, dev_idle: i64) -> Self {
        Writer {
            db,
            main,
            tracker: Tracker::new(play_idle, dev_idle),
            metas: HashMap::new(),
            pending: vec![],
            hourly: HashMap::new(),
            devices: HashMap::new(),
            device_users: HashMap::new(),
            force: false,
        }
    }

    fn meta_for(&mut self, file_id: &str) -> Option<FileMeta> {
        if let Some(m) = self.metas.get(file_id) {
            return Some(m.clone());
        }
        let conn = self.main.conn().ok()?;
        let repo = crate::db::Repo::new(&conn);
        let f = repo.find_file(file_id).ok().flatten()?;
        let kind = repo.find_item(&f.item_id).ok().flatten().map(|i| i.kind.as_str().to_string()).unwrap_or_default();
        let m = FileMeta { item_id: f.item_id.clone(), item_kind: kind, size: f.size, duration: f.duration };
        if self.metas.len() > 20_000 {
            self.metas.clear();
        }
        self.metas.insert(file_id.to_string(), m.clone());
        Some(m)
    }

    fn touch_device(&mut self, device: &str, user: &Option<String>, ts: i64, ua: &str, ip: &str, requests: u64, bytes: u64) {
        let d = self.devices.entry(device.to_string()).or_default();
        if d.first == 0 {
            d.first = ts;
        }
        d.last = d.last.max(ts);
        d.requests += requests;
        d.bytes += bytes;
        if !ua.is_empty() {
            d.ua = ua.to_string();
        }
        if !ip.is_empty() {
            d.ip = ip.to_string();
        }
        if user.is_some() {
            d.user = user.clone();
        }
        if let Some(u) = user {
            let du = self.device_users.entry((device.to_string(), u.clone())).or_default();
            if du.first == 0 {
                du.first = ts;
            }
            du.last = du.last.max(ts);
            du.requests += requests;
            du.bytes += bytes;
        }
    }

    pub fn handle(&mut self, m: Msg) {
        match m {
            Msg::StreamStart { ts, device, user, file_id } => {
                let meta = self.meta_for(&file_id);
                self.tracker.stream_start(ts, &device, &user, &file_id, meta.as_ref());
            }
            Msg::Req { mut ev, ua, ip } => {
                let mut meta = None;
                if let Some(f) = ev.file_id.clone() {
                    meta = self.meta_for(&f);
                    if ev.item_id.is_none() {
                        ev.item_id = meta.as_ref().map(|m| m.item_id.clone());
                    }
                }
                self.touch_device(&ev.device.clone(), &ev.user.clone(), ev.ts, &ua, &ip, 1, ev.bytes);
                self.tracker.request(&ev, meta.as_ref());
                let k = (ev.ts.div_euclid(3600) * 3600, ev.group.clone(), ev.route.clone(), status_class(ev.status), ev.device.clone(), ev.user.clone().unwrap_or_default());
                let h = self.hourly.entry(k).or_default();
                h.0 += 1;
                h.1 += ev.bytes;
                h.2 += ev.dur_ms;
                self.pending.push(ev);
            }
            Msg::Play { ts, device, user, ua, ip, file_id, meta, ev, pos, dur, reply } => {
                self.touch_device(&device, &user, ts, &ua, &ip, 0, 0);
                let row = self.tracker.playback_event(ts, &device, &user, &file_id, &meta, ev, pos, dur);
                let _ = reply.send(row);
                self.force = true;
            }
        }
    }

    /// Persist everything buffered, in one transaction.
    pub fn flush(&mut self) {
        let (plays, devs) = self.tracker.take_dirty();
        if self.pending.is_empty() && self.hourly.is_empty() && self.devices.is_empty() && plays.is_empty() && devs.is_empty() {
            return;
        }
        let res = (|| -> Result<(), rusqlite::Error> {
            let conn = self.db.conn().map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;
            conn.execute_batch("BEGIN")?;
            let r = (|| -> Result<(), rusqlite::Error> {
                {
                    let mut st = conn.prepare_cached(
                        "INSERT INTO requests (ts, grp, route, method, status, bytes, dur_ms, device, user, item_id, file_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    )?;
                    for e in &self.pending {
                        st.execute(params![e.ts, e.group, e.route, e.method, e.status, e.bytes as i64, e.dur_ms as i64, e.device, e.user, e.item_id, e.file_id])?;
                    }
                }
                {
                    let mut st = conn.prepare_cached(
                        "INSERT INTO requests_hourly (hour, grp, route, status_class, device, user, requests, bytes, dur_ms_sum) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                         ON CONFLICT (hour, grp, route, status_class, device, user) DO UPDATE SET
                           requests = requests + excluded.requests, bytes = bytes + excluded.bytes, dur_ms_sum = dur_ms_sum + excluded.dur_ms_sum",
                    )?;
                    for ((h, g, r, c, d, u), (n, b, ms)) in &self.hourly {
                        st.execute(params![h, g, r, c, d, u, *n as i64, *b as i64, *ms as i64])?;
                    }
                }
                {
                    let mut st = conn.prepare_cached(
                        "INSERT INTO devices (id, first_seen, last_seen, requests, bytes, user_agent, ip, last_user) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                         ON CONFLICT (id) DO UPDATE SET last_seen = max(last_seen, excluded.last_seen),
                           requests = requests + excluded.requests, bytes = bytes + excluded.bytes,
                           user_agent = CASE WHEN excluded.user_agent = '' THEN user_agent ELSE excluded.user_agent END,
                           ip = CASE WHEN excluded.ip = '' THEN ip ELSE excluded.ip END,
                           last_user = COALESCE(excluded.last_user, last_user)",
                    )?;
                    for (id, d) in &self.devices {
                        st.execute(params![id, d.first, d.last, d.requests as i64, d.bytes as i64, d.ua, d.ip, d.user])?;
                    }
                    let mut st = conn.prepare_cached(
                        "INSERT INTO device_users (device, user, first_seen, last_seen, requests, bytes) VALUES (?1,?2,?3,?4,?5,?6)
                         ON CONFLICT (device, user) DO UPDATE SET last_seen = max(last_seen, excluded.last_seen),
                           requests = requests + excluded.requests, bytes = bytes + excluded.bytes",
                    )?;
                    for ((dv, u), d) in &self.device_users {
                        st.execute(params![dv, u, d.first, d.last, d.requests as i64, d.bytes as i64])?;
                    }
                }
                {
                    let mut st = conn.prepare_cached(
                        "INSERT OR REPLACE INTO playback_sessions (id, device, user, file_id, item_id, item_kind, started_at, last_at, ended_at, source, state, bytes, requests,
                           watched_s, inferred_watched_s, max_position_s, position_s, duration_s, file_size, ranges)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
                    )?;
                    for p in &plays {
                        st.execute(params![
                            p.id, p.device, p.user, p.file_id, p.item_id, p.item_kind, p.started_at, p.last_at, p.ended_at, p.source, p.state,
                            p.bytes as i64, p.requests as i64, p.watched_s, p.inferred_watched_s, p.max_position_s, p.position_s, p.duration_s,
                            p.file_size as i64, serde_json::to_string(&p.ranges).unwrap_or_else(|_| "[]".into())
                        ])?;
                    }
                    let mut st = conn.prepare_cached(
                        "INSERT OR REPLACE INTO device_sessions (id, device, user, started_at, last_at, ended_at, requests, bytes) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    )?;
                    for d in &devs {
                        st.execute(params![d.id, d.device, d.user, d.started_at, d.last_at, d.ended_at, d.requests as i64, d.bytes as i64])?;
                    }
                }
                Ok(())
            })();
            match r {
                Ok(()) => conn.execute_batch("COMMIT"),
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })();
        match res {
            Ok(()) => {
                self.pending.clear();
                self.hourly.clear();
                self.devices.clear();
                self.device_users.clear();
            }
            Err(e) => {
                eprintln!("analytics: flush failed: {e}");
                // keep buffers (bounded) and retry; sessions are re-marked dirty by their next update
                if self.pending.len() > 100_000 {
                    self.pending.clear();
                    self.hourly.clear();
                }
                self.tracker.restore_dirty(plays, devs);
            }
        }
        self.force = false;
    }

    pub fn close_dangling(&self) {
        if let Ok(conn) = self.db.conn() {
            let _ = conn.execute("UPDATE playback_sessions SET ended_at = last_at, state = 'stopped' WHERE ended_at IS NULL", []);
            let _ = conn.execute("UPDATE device_sessions SET ended_at = last_at WHERE ended_at IS NULL", []);
        }
    }
}

/// Delete raw request rows older than `raw_days` (aggregates are kept forever). Returns how many.
pub fn prune_raw(db: &AnalyticsDb, raw_days: u64, now: i64) -> AppResult<usize> {
    let cutoff = (now - raw_days as i64 * 86400).div_euclid(3600) * 3600;
    let conn = db.conn()?;
    let prev: i64 = meta_get(&conn, "raw_pruned_until").and_then(|v| v.parse().ok()).unwrap_or(0);
    let cutoff = cutoff.max(prev);
    let n = conn.execute("DELETE FROM requests WHERE ts < ?1", [cutoff])?;
    meta_set(&conn, "raw_pruned_until", &cutoff.to_string())?;
    if n > 0 {
        println!("analytics: pruned {n} raw request rows older than {raw_days} day(s)");
    }
    Ok(n)
}

pub fn run(rx: Receiver<Msg>, mut w: Writer) {
    let now = crate::db::now;
    w.close_dangling();
    let mut last_flush = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(m) => {
                w.handle(m);
                let mut n = 0;
                while let Ok(m) = rx.try_recv() {
                    w.handle(m);
                    n += 1;
                    if n >= 4000 {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                w.flush();
                return;
            }
        }
        if w.force || last_flush.elapsed() >= Duration::from_secs(1) {
            w.tracker.expire(now());
            w.flush();
            last_flush = Instant::now();
        }
    }
}
