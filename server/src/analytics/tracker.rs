//! Session inference (pure, clock passed in): playback sessions from `/stream` requests and
//! explicit playback events, device sessions from any request. No I/O; the writer persists the
//! dirty rows this returns.

use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// What the writer knows about a file (looked up in the main database).
#[derive(Debug, Clone)]
pub struct FileMeta {
    pub item_id: String,
    pub item_kind: String,
    pub size: u64,
    pub duration: Option<f64>,
}

/// A finished (or aborted) HTTP request.
#[derive(Debug, Clone)]
pub struct ReqEvent {
    pub ts: i64,
    pub end_ts: i64,
    pub group: String,
    pub route: String,
    pub method: String,
    pub status: u16,
    pub bytes: u64,
    pub dur_ms: u64,
    pub device: String,
    pub user: Option<String>,
    pub item_id: Option<String>,
    pub file_id: Option<String>,
    /// First byte offset served (`/stream` GET 200/206 only).
    pub range_start: Option<u64>,
    /// A `StreamStart` was registered for this request (so it is in flight until now).
    pub started: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayRow {
    pub id: String,
    pub device: String,
    pub user: Option<String>,
    pub file_id: String,
    pub item_id: String,
    pub item_kind: String,
    pub started_at: i64,
    pub last_at: i64,
    pub ended_at: Option<i64>,
    /// `inferred` (from /stream requests only) or `explicit` (client sent playback events).
    pub source: String,
    /// `playing` | `paused` | `stopped`
    pub state: String,
    pub bytes: u64,
    pub requests: u64,
    pub watched_s: f64,
    pub inferred_watched_s: f64,
    pub max_position_s: f64,
    pub position_s: f64,
    pub duration_s: Option<f64>,
    pub file_size: u64,
    /// Merged served byte ranges `[start, end)`.
    pub ranges: Vec<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevRow {
    pub id: String,
    pub device: String,
    pub user: Option<String>,
    pub started_at: i64,
    pub last_at: i64,
    pub ended_at: Option<i64>,
    pub requests: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayEvent {
    Start,
    Progress,
    Pause,
    Resume,
    Stop,
}

impl PlayEvent {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "start" => Self::Start,
            "progress" => Self::Progress,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "stop" => Self::Stop,
            _ => return None,
        })
    }
}

struct PlaySess {
    row: PlayRow,
    in_flight: u32,
    explicit: bool,
    ex_watched: f64,
    ex_max: f64,
    last_ev_ts: i64,
    last_ev_pos: f64,
    /// Furthest playback position implied by served ranges (excluding index probes).
    inf_max: f64,
    inf_pos: f64,
}

struct DevSess {
    row: DevRow,
    in_flight: u32,
}

pub struct Tracker {
    pub play_idle: i64,
    pub dev_idle: i64,
    play: HashMap<(String, String), PlaySess>,
    /// Explicitly stopped sessions still absorbing trailing buffered requests.
    grace: HashMap<(String, String), PlaySess>,
    dev: HashMap<String, DevSess>,
    dirty_play: HashMap<String, PlayRow>,
    dirty_dev: HashMap<String, DevRow>,
    counter: u64,
}

const GRACE_SECS: i64 = 15;
/// A leaked in-flight counter must not keep a session open forever.
const MAX_IN_FLIGHT_AGE: i64 = 6 * 3600;

/// Add `[s, e)` to a sorted, merged range list.
pub fn add_range(ranges: &mut Vec<(u64, u64)>, s: u64, e: u64) {
    if e <= s {
        return;
    }
    ranges.push((s, e));
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for &(a, b) in ranges.iter() {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    *ranges = out;
}

pub fn range_total(r: &[(u64, u64)]) -> u64 {
    r.iter().map(|(a, b)| b - a).sum()
}

/// Short reads at the very end of a file are container index probes (mp4 moov, mkv cues), not playback.
fn is_probe(s: u64, e: u64, size: u64) -> bool {
    size > 0 && s >= size / 10 * 9 && (e - s) <= size / 20
}

impl Tracker {
    pub fn new(play_idle: i64, dev_idle: i64) -> Self {
        Tracker {
            play_idle,
            dev_idle,
            play: HashMap::new(),
            grace: HashMap::new(),
            dev: HashMap::new(),
            dirty_play: HashMap::new(),
            dirty_dev: HashMap::new(),
            counter: 0,
        }
    }

    fn new_id(&mut self, prefix: &str, ts: i64) -> String {
        self.counter += 1;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let h = blake3::hash(format!("{prefix}{ts}{nanos}{}", self.counter).as_bytes());
        format!("{prefix}{}", &h.to_hex()[..16])
    }

    pub fn take_dirty(&mut self) -> (Vec<PlayRow>, Vec<DevRow>) {
        (self.dirty_play.drain().map(|(_, v)| v).collect(), self.dirty_dev.drain().map(|(_, v)| v).collect())
    }

    /// A failed flush puts its rows back (newer updates to the same session win).
    pub fn restore_dirty(&mut self, plays: Vec<PlayRow>, devs: Vec<DevRow>) {
        for p in plays {
            self.dirty_play.entry(p.id.clone()).or_insert(p);
        }
        for d in devs {
            self.dirty_dev.entry(d.id.clone()).or_insert(d);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn active_counts(&self) -> (usize, usize) {
        (self.play.len(), self.dev.len())
    }

    fn mark_play(&mut self, key: &(String, String), grace: bool) {
        let s = if grace { self.grace.get(key) } else { self.play.get(key) };
        if let Some(s) = s {
            self.dirty_play.insert(s.row.id.clone(), s.row.clone());
        }
    }

    // ---- device sessions -------------------------------------------------------------------

    fn dev_touch(&mut self, device: &str, user: &Option<String>, ts: i64, end_ts: i64, requests: u64, bytes: u64, in_flight: i32) {
        if let Some(s) = self.dev.get(device) {
            if s.in_flight == 0 && ts - s.row.last_at > self.dev_idle {
                self.close_dev(device);
            }
        }
        if !self.dev.contains_key(device) {
            let id = self.new_id("dv_", ts);
            let row = DevRow { id, device: device.into(), user: user.clone(), started_at: ts, last_at: ts, ended_at: None, requests: 0, bytes: 0 };
            self.dev.insert(device.into(), DevSess { row, in_flight: 0 });
        }
        let s = self.dev.get_mut(device).unwrap();
        s.row.requests += requests;
        s.row.bytes += bytes;
        s.row.last_at = s.row.last_at.max(end_ts);
        if user.is_some() {
            s.row.user = user.clone();
        }
        s.in_flight = (s.in_flight as i32 + in_flight).max(0) as u32;
        self.dirty_dev.insert(s.row.id.clone(), s.row.clone());
    }

    fn close_dev(&mut self, device: &str) {
        if let Some(mut s) = self.dev.remove(device) {
            s.row.ended_at = Some(s.row.last_at);
            self.dirty_dev.insert(s.row.id.clone(), s.row);
        }
    }

    // ---- playback sessions -----------------------------------------------------------------

    /// Find the session a request/event belongs to, expiring a stale one. Returns (key, in_grace).
    fn play_slot(&mut self, device: &str, file_id: &str, ts: i64) -> Option<((String, String), bool)> {
        let key = (device.to_string(), file_id.to_string());
        if let Some(s) = self.play.get(&key) {
            if s.in_flight == 0 && ts - s.row.last_at > self.play_idle {
                self.close_play(&key, None);
            } else {
                return Some((key, false));
            }
        }
        if let Some(g) = self.grace.get(&key) {
            if ts - g.row.ended_at.unwrap_or(0) <= GRACE_SECS {
                return Some((key, true));
            }
        }
        None
    }

    fn open_play(&mut self, device: &str, user: &Option<String>, file_id: &str, meta: &FileMeta, ts: i64, source: &str) -> (String, String) {
        let key = (device.to_string(), file_id.to_string());
        let id = self.new_id("pb_", ts);
        let row = PlayRow {
            id,
            device: device.into(),
            user: user.clone(),
            file_id: file_id.into(),
            item_id: meta.item_id.clone(),
            item_kind: meta.item_kind.clone(),
            started_at: ts,
            last_at: ts,
            ended_at: None,
            source: source.into(),
            state: "playing".into(),
            bytes: 0,
            requests: 0,
            watched_s: 0.0,
            inferred_watched_s: 0.0,
            max_position_s: 0.0,
            position_s: 0.0,
            duration_s: meta.duration,
            file_size: meta.size,
            ranges: vec![],
        };
        self.grace.remove(&key);
        self.play.insert(
            key.clone(),
            PlaySess { row, in_flight: 0, explicit: false, ex_watched: 0.0, ex_max: 0.0, last_ev_ts: ts, last_ev_pos: 0.0, inf_max: 0.0, inf_pos: 0.0 },
        );
        key
    }

    fn close_play(&mut self, key: &(String, String), at: Option<i64>) {
        if let Some(mut s) = self.play.remove(key) {
            s.row.ended_at = Some(at.unwrap_or(s.row.last_at));
            s.row.state = "stopped".into();
            self.dirty_play.insert(s.row.id.clone(), s.row.clone());
            if s.explicit {
                self.grace.insert(key.clone(), s);
            }
        }
    }

    fn recompute(s: &mut PlaySess) {
        let inferred = if s.row.file_size > 0 { s.row.duration_s.unwrap_or(0.0) * range_total(&s.row.ranges) as f64 / s.row.file_size as f64 } else { 0.0 };
        s.row.inferred_watched_s = inferred;
        if s.explicit {
            s.row.watched_s = s.ex_watched;
            s.row.max_position_s = s.ex_max;
        } else {
            s.row.watched_s = inferred;
            s.row.max_position_s = s.inf_max;
            s.row.position_s = s.inf_pos;
        }
    }

    /// `/stream` response headers were produced (before any body byte): the file is being played now.
    pub fn stream_start(&mut self, ts: i64, device: &str, user: &Option<String>, file_id: &str, meta: Option<&FileMeta>) {
        self.dev_touch(device, user, ts, ts, 0, 0, 1);
        let Some(meta) = meta else { return };
        let (key, grace) = match self.play_slot(device, file_id, ts) {
            Some(x) => x,
            None => (self.open_play(device, user, file_id, meta, ts, "inferred"), false),
        };
        if grace {
            return;
        }
        let s = self.play.get_mut(&key).unwrap();
        s.in_flight += 1;
        s.row.last_at = s.row.last_at.max(ts);
        if user.is_some() {
            s.row.user = user.clone();
        }
        self.mark_play(&key, false);
    }

    /// A request finished. Updates device sessions and (for `/stream`) playback sessions.
    pub fn request(&mut self, e: &ReqEvent, meta: Option<&FileMeta>) {
        self.dev_touch(&e.device, &e.user, e.ts, e.end_ts, 1, e.bytes, if e.started { -1 } else { 0 });
        if e.group != "stream" || e.method != "GET" || !(e.status == 200 || e.status == 206) {
            if e.started {
                // header-time start registered but the request did not count (cannot happen: start only for 200/206 GET)
                self.release(&e.device, e.file_id.as_deref().unwrap_or(""));
            }
            return;
        }
        let (Some(file_id), Some(meta)) = (e.file_id.as_deref(), meta) else { return };
        let (key, grace) = match self.play_slot(&e.device, file_id, e.ts) {
            Some(x) => x,
            None => (self.open_play(&e.device, &e.user, file_id, meta, e.ts, "inferred"), false),
        };
        let s = if grace { self.grace.get_mut(&key) } else { self.play.get_mut(&key) }.unwrap();
        if e.started && s.in_flight > 0 && !grace {
            s.in_flight -= 1;
        }
        s.row.requests += 1;
        s.row.bytes += e.bytes;
        s.row.last_at = s.row.last_at.max(e.end_ts);
        if e.user.is_some() {
            s.row.user = e.user.clone();
        }
        if e.bytes > 0 && s.row.file_size > 0 {
            let start = e.range_start.unwrap_or(0).min(s.row.file_size);
            let end = (start + e.bytes).min(s.row.file_size);
            if !is_probe(start, end, s.row.file_size) {
                add_range(&mut s.row.ranges, start, end);
                if let Some(d) = s.row.duration_s {
                    let pos = end as f64 / s.row.file_size as f64 * d;
                    if pos > s.inf_max {
                        s.inf_max = pos;
                    }
                    s.inf_pos = pos;
                }
            }
        }
        Self::recompute(s);
        self.mark_play(&key, grace);
    }

    fn release(&mut self, device: &str, file_id: &str) {
        if let Some(s) = self.play.get_mut(&(device.to_string(), file_id.to_string())) {
            s.in_flight = s.in_flight.saturating_sub(1);
        }
    }

    /// Explicit playback event from a client. Returns the session id.
    #[allow(clippy::too_many_arguments)]
    pub fn playback_event(
        &mut self,
        ts: i64,
        device: &str,
        user: &Option<String>,
        file_id: &str,
        meta: &FileMeta,
        ev: PlayEvent,
        pos: f64,
        duration: Option<f64>,
    ) -> PlayRow {
        self.dev_touch(device, user, ts, ts, 0, 0, 0);
        let (key, grace) = match self.play_slot(device, file_id, ts) {
            Some(x) => x,
            None => (self.open_play(device, user, file_id, meta, ts, "explicit"), false),
        };
        // an event on a grace session (stop already received): reopen as a fresh session for start, else absorb
        let key = if grace && ev == PlayEvent::Start {
            self.grace.remove(&key);
            self.open_play(device, user, file_id, meta, ts, "explicit")
        } else {
            key
        };
        if grace && ev != PlayEvent::Start {
            let s = self.grace.get(&key).unwrap();
            return s.row.clone();
        }
        let s = self.play.get_mut(&key).unwrap();
        if !s.explicit {
            s.explicit = true;
            s.row.source = "explicit".into();
            s.last_ev_ts = ts;
            s.last_ev_pos = pos;
            s.ex_max = 0.0;
        }
        if user.is_some() {
            s.row.user = user.clone();
        }
        if let Some(d) = duration.filter(|d| *d > 0.0) {
            s.row.duration_s = Some(d);
        }
        // accrue continuous playback since the previous event (a jump is a seek, not watching)
        if s.row.state == "playing" && ev != PlayEvent::Start && ev != PlayEvent::Resume {
            let wall = (ts - s.last_ev_ts).max(0) as f64;
            let dpos = pos - s.last_ev_pos;
            if dpos >= 0.0 && dpos <= wall * 2.0 + 2.0 {
                s.ex_watched += dpos;
            }
        }
        s.last_ev_ts = ts;
        s.last_ev_pos = pos;
        s.ex_max = s.ex_max.max(pos);
        s.row.position_s = pos;
        s.row.last_at = s.row.last_at.max(ts);
        s.row.state = match ev {
            PlayEvent::Pause => "paused",
            PlayEvent::Stop => "stopped",
            _ => "playing",
        }
        .into();
        Self::recompute(s);
        let out_id = s.row.id.clone();
        self.mark_play(&key, false);
        if ev == PlayEvent::Stop {
            self.close_play(&key, Some(ts));
        }
        let row = self.dirty_play.get(&out_id).cloned();
        row.unwrap_or_else(|| self.grace.get(&key).map(|g| g.row.clone()).unwrap())
    }

    /// Close idle sessions. Call about once a second.
    pub fn expire(&mut self, now: i64) {
        let stale: Vec<_> = self
            .play
            .iter()
            .filter(|(_, s)| {
                let idle = now - s.row.last_at;
                (s.in_flight == 0 && idle > self.play_idle) || idle > MAX_IN_FLIGHT_AGE
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            self.close_play(&k, None);
        }
        // a connection that is still open keeps its session alive
        let live: Vec<_> = self.play.iter().filter(|(_, s)| s.in_flight > 0).map(|(k, _)| k.clone()).collect();
        for k in live {
            let s = self.play.get_mut(&k).unwrap();
            s.row.last_at = s.row.last_at.max(now);
            self.mark_play(&k, false);
        }
        let stale: Vec<_> = self
            .dev
            .iter()
            .filter(|(_, s)| {
                let idle = now - s.row.last_at;
                (s.in_flight == 0 && idle > self.dev_idle) || idle > MAX_IN_FLIGHT_AGE
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            self.close_dev(&k);
        }
        let live: Vec<_> = self.dev.iter().filter(|(_, s)| s.in_flight > 0).map(|(k, _)| k.clone()).collect();
        for k in live {
            let s = self.dev.get_mut(&k).unwrap();
            s.row.last_at = s.row.last_at.max(now);
            self.dirty_dev.insert(s.row.id.clone(), s.row.clone());
        }
        self.grace.retain(|_, g| now - g.row.ended_at.unwrap_or(0) <= GRACE_SECS);
    }

    /// Close everything (shutdown / test).
    #[allow(dead_code)]
    pub fn close_all(&mut self) {
        let keys: Vec<_> = self.play.keys().cloned().collect();
        for k in keys {
            self.close_play(&k, None);
        }
        let ds: HashSet<_> = self.dev.keys().cloned().collect();
        for d in ds {
            self.close_dev(&d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> FileMeta {
        FileMeta { item_id: "it".into(), item_kind: "movie".into(), size: 1000, duration: Some(100.0) }
    }
    fn req(ts: i64, start: u64, bytes: u64) -> ReqEvent {
        ReqEvent {
            ts,
            end_ts: ts,
            group: "stream".into(),
            route: "/stream/{file_id}".into(),
            method: "GET".into(),
            status: 206,
            bytes,
            dur_ms: 5,
            device: "d1".into(),
            user: Some("u".into()),
            item_id: None,
            file_id: Some("f".into()),
            range_start: Some(start),
            started: false,
        }
    }

    #[test]
    fn ranges_merge() {
        let mut r = vec![];
        add_range(&mut r, 0, 100);
        add_range(&mut r, 200, 300);
        add_range(&mut r, 50, 220);
        assert_eq!(r, vec![(0, 300)]);
        assert_eq!(range_total(&r), 300);
    }

    #[test]
    fn inferred_session_extends_and_closes() {
        let mut t = Tracker::new(300, 1800);
        let m = meta();
        t.request(&req(1000, 0, 100), Some(&m));
        t.request(&req(1100, 100, 100), Some(&m)); // contiguous
        t.request(&req(1200, 0, 100), Some(&m)); // re-read same bytes: union unchanged
        let (p, d) = t.take_dirty();
        assert_eq!(p.len(), 1);
        assert_eq!(d.len(), 1);
        let s = &p[0];
        assert_eq!(s.requests, 3);
        assert_eq!(s.bytes, 300);
        assert!((s.watched_s - 20.0).abs() < 1e-9, "{}", s.watched_s);
        assert!((s.max_position_s - 20.0).abs() < 1e-9);
        assert_eq!(s.source, "inferred");
        // within the idle window: same session
        t.expire(1400);
        assert_eq!(t.active_counts().0, 1);
        t.expire(1501); // 300s after last_at(1200)
        assert_eq!(t.active_counts().0, 0);
        let (p, _) = t.take_dirty();
        assert_eq!(p[0].ended_at, Some(1200));
        // a later request starts a new session
        t.request(&req(2000, 0, 100), Some(&m));
        let (p, _) = t.take_dirty();
        assert_ne!(p[0].id, s.id);
    }

    #[test]
    fn stale_session_replaced_on_request_even_without_expire() {
        let mut t = Tracker::new(300, 1800);
        let m = meta();
        t.request(&req(1000, 0, 100), Some(&m));
        t.request(&req(1400, 0, 100), Some(&m));
        let (p, _) = t.take_dirty();
        assert_eq!(p.len(), 2);
        assert!(p.iter().any(|r| r.ended_at == Some(1000)));
    }

    #[test]
    fn index_probe_ignored_for_position() {
        let mut t = Tracker::new(300, 1800);
        let m = meta();
        t.request(&req(1000, 950, 50), Some(&m)); // tail read (moov): small, in last 10%
        t.request(&req(1001, 0, 200), Some(&m));
        let (p, _) = t.take_dirty();
        assert!((p[0].max_position_s - 20.0).abs() < 1e-9);
        assert_eq!(p[0].bytes, 250);
    }

    #[test]
    fn in_flight_stream_keeps_session_open() {
        let mut t = Tracker::new(300, 1800);
        let m = meta();
        t.stream_start(1000, "d1", &None, "f", Some(&m));
        t.expire(5000);
        assert_eq!(t.active_counts().0, 1);
        let mut e = req(1000, 0, 500);
        e.started = true;
        e.end_ts = 5000;
        t.request(&e, Some(&m));
        t.expire(5100);
        assert_eq!(t.active_counts().0, 1);
        t.expire(5301);
        assert_eq!(t.active_counts().0, 0);
    }

    #[test]
    fn explicit_events_take_precedence() {
        let mut t = Tracker::new(300, 1800);
        let m = meta();
        let u = Some("u".to_string());
        t.request(&req(1000, 0, 900), Some(&m)); // inferred says 90 s
        let r = t.playback_event(1000, "d1", &u, "f", &m, PlayEvent::Start, 0.0, None);
        assert_eq!(r.source, "explicit");
        t.playback_event(1010, "d1", &u, "f", &m, PlayEvent::Progress, 10.0, None);
        t.playback_event(1015, "d1", &u, "f", &m, PlayEvent::Pause, 15.0, None);
        t.playback_event(1100, "d1", &u, "f", &m, PlayEvent::Resume, 15.0, None);
        t.playback_event(1110, "d1", &u, "f", &m, PlayEvent::Progress, 60.0, None); // seek, not watched
        let r = t.playback_event(1115, "d1", &u, "f", &m, PlayEvent::Stop, 65.0, Some(100.0));
        assert_eq!(r.state, "stopped");
        assert!((r.watched_s - 20.0).abs() < 1e-9, "{}", r.watched_s);
        assert!((r.max_position_s - 65.0).abs() < 1e-9);
        assert!((r.inferred_watched_s - 90.0).abs() < 1e-9);
        assert_eq!(t.active_counts().0, 0);
        // trailing buffered request lands in the stopped session
        t.request(&req(1118, 900, 100), Some(&m));
        let (p, _) = t.take_dirty();
        let last = p.iter().find(|r| r.id == r.id && r.ended_at.is_some()).unwrap();
        assert_eq!(last.requests, 2);
        assert!((last.watched_s - 20.0).abs() < 1e-9);
    }

    #[test]
    fn device_session_idle_split() {
        let mut t = Tracker::new(300, 1800);
        let mut e = req(1000, 0, 10);
        e.group = "api".into();
        e.route = "/api/items".into();
        t.request(&e, None);
        e.ts = 2000;
        e.end_ts = 2000;
        t.request(&e, None);
        e.ts = 4000;
        e.end_ts = 4000; // > 1800 after 2000
        t.request(&e, None);
        let (_, d) = t.take_dirty();
        assert_eq!(d.len(), 2);
        let closed = d.iter().find(|r| r.ended_at.is_some()).unwrap();
        assert_eq!((closed.requests, closed.bytes), (2, 20));
    }
}
