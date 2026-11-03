//! Read side of analytics.db: time bucketing, traffic aggregation (raw rows where they still
//! exist, hourly aggregates for older data), snapshot series, sessions, top lists.

use super::store::meta_get;
use crate::error::{AppError, AppResult};
use rusqlite::{types::Value as Sql, Connection};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Bucket {
    Hour,
    Day,
    Week,
    Month,
}

impl Bucket {
    pub fn parse(s: &str) -> AppResult<Self> {
        Ok(match s {
            "hour" => Self::Hour,
            "day" => Self::Day,
            "week" => Self::Week,
            "month" => Self::Month,
            _ => return Err(AppError::bad("bucket must be hour, day, week or month")),
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }
}

// Howard Hinnant's civil-date algorithms.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Start (UTC, unix seconds) of the bucket containing `ts`. Weeks start on Monday.
pub fn bucket_start(ts: i64, b: Bucket) -> i64 {
    match b {
        Bucket::Hour => ts.div_euclid(3600) * 3600,
        Bucket::Day => ts.div_euclid(86400) * 86400,
        Bucket::Week => {
            let day = ts.div_euclid(86400);
            (day - (day + 3).rem_euclid(7)) * 86400
        }
        Bucket::Month => {
            let (y, m, _) = civil_from_days(ts.div_euclid(86400));
            days_from_civil(y, m, 1) * 86400
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GroupBy {
    Route,
    Group,
    Device,
    User,
    Status,
    None,
}

impl GroupBy {
    pub fn parse(s: &str) -> AppResult<Self> {
        Ok(match s {
            "route" => Self::Route,
            "group" => Self::Group,
            "device" => Self::Device,
            "user" => Self::User,
            "status" => Self::Status,
            "none" => Self::None,
            _ => return Err(AppError::bad("group_by must be route, group, device, user, status or none")),
        })
    }
    fn names(self) -> &'static [&'static str] {
        match self {
            Self::Route => &["group", "route"],
            Self::Group => &["group"],
            Self::Device => &["device"],
            Self::User => &["user"],
            Self::Status => &["status"],
            Self::None => &[],
        }
    }
    fn key(self, grp: &str, route: &str, device: &str, user: &str, class: &str) -> Vec<String> {
        match self {
            Self::Route => vec![grp.into(), route.into()],
            Self::Group => vec![grp.into()],
            Self::Device => vec![device.into()],
            Self::User => vec![user.into()],
            Self::Status => vec![class.into()],
            Self::None => vec![],
        }
    }
}

#[derive(Default)]
struct Acc {
    requests: u64,
    bytes: u64,
    dur_sum: u64,
    durs: Vec<u64>,
    raw: bool,
    agg: bool,
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    // nearest rank
    let n = sorted.len();
    let rank = ((p * n as f64).ceil() as usize).clamp(1, n);
    sorted[rank - 1]
}

/// Traffic rows. `bucket = None` collapses the whole range into one row per key (`t = from`).
/// Raw rows are used for `ts >= raw_pruned_until`; hourly aggregates (hour granularity, no
/// percentiles) for everything before.
pub fn traffic(c: &Connection, from: i64, to: i64, bucket: Option<Bucket>, group_by: GroupBy) -> AppResult<Vec<Value>> {
    let pruned: i64 = meta_get(c, "raw_pruned_until").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut acc: BTreeMap<(i64, Vec<String>), Acc> = BTreeMap::new();
    let tb = |ts: i64| bucket.map(|b| bucket_start(ts, b)).unwrap_or(from);
    let agg_to = to.min(pruned);
    if agg_to > from {
        let mut st = c.prepare_cached(
            "SELECT hour, grp, route, status_class, device, user, requests, bytes, dur_ms_sum FROM requests_hourly WHERE hour >= ?1 AND hour < ?2",
        )?;
        let mut rows = st.query([from.div_euclid(3600) * 3600, agg_to])?;
        while let Some(r) = rows.next()? {
            let (hour, g, route, class, dev, user): (i64, String, String, String, String, String) =
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?);
            let a = acc.entry((tb(hour), group_by.key(&g, &route, &dev, &user, &class))).or_default();
            a.requests += r.get::<_, i64>(6)? as u64;
            a.bytes += r.get::<_, i64>(7)? as u64;
            a.dur_sum += r.get::<_, i64>(8)? as u64;
            a.agg = true;
        }
    }
    let raw_from = from.max(pruned);
    if to > raw_from {
        let mut st = c.prepare_cached("SELECT ts, grp, route, status, device, user, bytes, dur_ms FROM requests WHERE ts >= ?1 AND ts < ?2")?;
        let mut rows = st.query([raw_from, to])?;
        while let Some(r) = rows.next()? {
            let ts: i64 = r.get(0)?;
            let (g, route): (String, String) = (r.get(1)?, r.get(2)?);
            let class = format!("{}xx", r.get::<_, i64>(3)? / 100);
            let (dev, user): (String, Option<String>) = (r.get(4)?, r.get(5)?);
            let dur = r.get::<_, i64>(7)? as u64;
            let a = acc.entry((tb(ts), group_by.key(&g, &route, &dev, &user.unwrap_or_default(), &class))).or_default();
            a.requests += 1;
            a.bytes += r.get::<_, i64>(6)? as u64;
            a.dur_sum += dur;
            a.durs.push(dur);
            a.raw = true;
        }
    }
    let names = group_by.names();
    Ok(acc
        .into_iter()
        .map(|((t, key), mut a)| {
            let mut k = Map::new();
            for (n, v) in names.iter().zip(key) {
                k.insert((*n).into(), if v.is_empty() && *n == "user" { Value::Null } else { Value::String(v) });
            }
            let (p50, p95) = if !a.agg && !a.durs.is_empty() {
                a.durs.sort_unstable();
                (json!(pct(&a.durs, 0.5)), json!(pct(&a.durs, 0.95)))
            } else {
                (Value::Null, Value::Null)
            };
            json!({
                "t": t, "key": if names.is_empty() { Value::Null } else { Value::Object(k) },
                "requests": a.requests, "bytes": a.bytes,
                "avg_ms": if a.requests > 0 { json!((a.dur_sum as f64 / a.requests as f64 * 10.0).round() / 10.0) } else { Value::Null },
                "p50_ms": p50, "p95_ms": p95,
                "source": match (a.raw, a.agg) { (true, false) => "raw", (false, true) => "aggregate", _ => "mixed" },
            })
        })
        .collect())
}

/// Last snapshot of each bucket in `[from, to]`.
pub fn library(c: &Connection, from: i64, to: i64, bucket: Bucket) -> AppResult<Vec<Value>> {
    let mut st = c.prepare("SELECT ts, data FROM snapshots WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts")?;
    let mut last: BTreeMap<i64, Value> = BTreeMap::new();
    let mut rows = st.query([from, to])?;
    while let Some(r) = rows.next()? {
        let (ts, data): (i64, String) = (r.get(0)?, r.get(1)?);
        let mut v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
        if let Value::Object(m) = &mut v {
            m.insert("t".into(), json!(bucket_start(ts, bucket)));
            m.insert("ts".into(), json!(ts));
        }
        last.insert(bucket_start(ts, bucket), v);
    }
    Ok(last.into_values().collect())
}

pub fn latest_snapshot(c: &Connection) -> Option<Value> {
    c.query_row("SELECT data FROM snapshots ORDER BY ts DESC LIMIT 1", [], |r| r.get::<_, String>(0)).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn play_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let ended: Option<i64> = r.get("ended_at")?;
    Ok(json!({
        "id": r.get::<_, String>("id")?, "type": "playback",
        "device": r.get::<_, String>("device")?, "user": r.get::<_, Option<String>>("user")?,
        "file_id": r.get::<_, String>("file_id")?, "item_id": r.get::<_, String>("item_id")?,
        "item_kind": r.get::<_, String>("item_kind")?,
        "started_at": r.get::<_, i64>("started_at")?, "last_at": r.get::<_, i64>("last_at")?,
        "ended_at": ended, "active": ended.is_none(),
        "source": r.get::<_, String>("source")?, "state": r.get::<_, String>("state")?,
        "bytes": r.get::<_, i64>("bytes")?, "requests": r.get::<_, i64>("requests")?,
        "watched_s": round1(r.get::<_, f64>("watched_s")?), "inferred_watched_s": round1(r.get::<_, f64>("inferred_watched_s")?),
        "max_position_s": round1(r.get::<_, f64>("max_position_s")?), "position_s": round1(r.get::<_, f64>("position_s")?),
        "duration_s": r.get::<_, Option<f64>>("duration_s")?, "file_size": r.get::<_, i64>("file_size")?,
    }))
}

fn dev_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let ended: Option<i64> = r.get("ended_at")?;
    Ok(json!({
        "id": r.get::<_, String>("id")?, "type": "device",
        "device": r.get::<_, String>("device")?, "user": r.get::<_, Option<String>>("user")?,
        "started_at": r.get::<_, i64>("started_at")?, "last_at": r.get::<_, i64>("last_at")?,
        "ended_at": ended, "active": ended.is_none(),
        "requests": r.get::<_, i64>("requests")?, "bytes": r.get::<_, i64>("bytes")?,
    }))
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

pub struct SessionQuery {
    pub kind: String,
    pub active: Option<bool>,
    pub device: Option<String>,
    pub user: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

pub fn sessions(c: &Connection, q: &SessionQuery) -> AppResult<(i64, Vec<Value>)> {
    let (table, mapper): (&str, fn(&rusqlite::Row) -> rusqlite::Result<Value>) = match q.kind.as_str() {
        "playback" => ("playback_sessions", play_json),
        "device" => ("device_sessions", dev_json),
        _ => return Err(AppError::bad("type must be playback or device")),
    };
    let mut wh = vec!["1=1".to_string()];
    let mut args: Vec<Sql> = vec![];
    if let Some(a) = q.active {
        wh.push(if a { "ended_at IS NULL" } else { "ended_at IS NOT NULL" }.into());
    }
    if let Some(d) = &q.device {
        args.push(Sql::Text(d.clone()));
        wh.push(format!("device = ?{}", args.len()));
    }
    if let Some(u) = &q.user {
        args.push(Sql::Text(u.clone()));
        wh.push(format!("user = ?{}", args.len()));
    }
    if let Some(f) = q.from {
        args.push(Sql::Integer(f));
        wh.push(format!("last_at >= ?{}", args.len()));
    }
    if let Some(t) = q.to {
        args.push(Sql::Integer(t));
        wh.push(format!("started_at < ?{}", args.len()));
    }
    let wh = wh.join(" AND ");
    let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE {wh}"), rusqlite::params_from_iter(args.iter()), |r| r.get(0))?;
    args.push(Sql::Integer(q.limit));
    args.push(Sql::Integer(q.offset));
    let n = args.len();
    let mut st = c.prepare(&format!("SELECT * FROM {table} WHERE {wh} ORDER BY started_at DESC, id LIMIT ?{} OFFSET ?{n}", n - 1))?;
    let rows = st.query_map(rusqlite::params_from_iter(args.iter()), |r| mapper(r))?.collect::<Result<Vec<_>, _>>()?;
    Ok((total, rows))
}

pub fn session(c: &Connection, id: &str) -> AppResult<Value> {
    let (table, mapper): (&str, fn(&rusqlite::Row) -> rusqlite::Result<Value>) = if id.starts_with("pb_") {
        ("playback_sessions", play_json)
    } else if id.starts_with("dv_") {
        ("device_sessions", dev_json)
    } else {
        return Err(AppError::not_found("session"));
    };
    let mut v = c.query_row(&format!("SELECT * FROM {table} WHERE id = ?1"), [id], |r| mapper(r)).map_err(|_| AppError::not_found("session"))?;
    if table == "playback_sessions" {
        let ranges: String = c.query_row("SELECT ranges FROM playback_sessions WHERE id = ?1", [id], |r| r.get(0))?;
        v["ranges"] = serde_json::from_str(&ranges).unwrap_or(json!([]));
    }
    Ok(v)
}

pub struct TopQuery {
    pub metric: String,
    pub by: String,
    pub from: i64,
    pub to: i64,
    pub limit: i64,
}

pub fn top(c: &Connection, q: &TopQuery) -> AppResult<Vec<Value>> {
    let col = match q.by.as_str() {
        "item" => "item_id",
        "user" => "user",
        "device" => "device",
        "kind" => "item_kind",
        _ => return Err(AppError::bad("by must be item, user, device or kind")),
    };
    let order = match q.metric.as_str() {
        "plays" => "plays",
        "bytes" => "bytes",
        "watch_time" => "watch_time_s",
        _ => return Err(AppError::bad("metric must be plays, bytes or watch_time")),
    };
    let sql = format!(
        "SELECT {col}, COUNT(*) AS plays, COALESCE(SUM(bytes),0) AS bytes, COALESCE(SUM(watched_s),0) AS watch_time_s
         FROM playback_sessions WHERE started_at >= ?1 AND started_at < ?2 AND {col} IS NOT NULL AND {col} != ''
         GROUP BY {col} ORDER BY {order} DESC, plays DESC LIMIT ?3"
    );
    let mut st = c.prepare(&sql)?;
    let rows = st
        .query_map([q.from, q.to, q.limit], |r| {
            let metric = match order {
                "plays" => json!(r.get::<_, i64>(1)?),
                "bytes" => json!(r.get::<_, i64>(2)?),
                _ => json!(round1(r.get::<_, f64>(3)?)),
            };
            Ok(json!({ "key": r.get::<_, String>(0)?, "value": metric, "plays": r.get::<_, i64>(1)?, "bytes": r.get::<_, i64>(2)?, "watch_time_s": round1(r.get::<_, f64>(3)?) }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn devices(c: &Connection) -> AppResult<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT d.id, d.first_seen, d.last_seen, d.requests, d.bytes, d.user_agent, d.ip, d.last_user,
           (SELECT COUNT(*) FROM playback_sessions p WHERE p.device = d.id) AS plays,
           (SELECT COALESCE(SUM(watched_s),0) FROM playback_sessions p WHERE p.device = d.id) AS watch,
           (SELECT COUNT(*) FROM device_sessions s WHERE s.device = d.id) AS sessions,
           EXISTS (SELECT 1 FROM device_sessions s WHERE s.device = d.id AND s.ended_at IS NULL) AS active
         FROM devices d ORDER BY d.last_seen DESC",
    )?;
    let mut users = c.prepare("SELECT user FROM device_users WHERE device = ?1 ORDER BY last_seen DESC")?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                json!({
                    "first_seen": r.get::<_, i64>(1)?, "last_seen": r.get::<_, i64>(2)?,
                    "requests": r.get::<_, i64>(3)?, "bytes": r.get::<_, i64>(4)?,
                    "user_agent": r.get::<_, Option<String>>(5)?, "ip": r.get::<_, Option<String>>(6)?,
                    "last_user": r.get::<_, Option<String>>(7)?,
                    "playback_sessions": r.get::<_, i64>(8)?, "watch_time_s": round1(r.get::<_, f64>(9)?),
                    "device_sessions": r.get::<_, i64>(10)?, "active": r.get::<_, i64>(11)? != 0,
                }),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = vec![];
    for (id, mut v) in rows {
        let us = users.query_map([&id], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        v["id"] = json!(id);
        v["users"] = json!(us);
        out.push(v);
    }
    Ok(out)
}

pub fn users(c: &Connection) -> AppResult<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT u.user, MIN(u.first_seen), MAX(u.last_seen), SUM(u.requests), SUM(u.bytes),
           (SELECT COUNT(*) FROM playback_sessions p WHERE p.user = u.user),
           (SELECT COALESCE(SUM(watched_s),0) FROM playback_sessions p WHERE p.user = u.user)
         FROM device_users u GROUP BY u.user ORDER BY MAX(u.last_seen) DESC",
    )?;
    let mut devs = c.prepare("SELECT device FROM device_users WHERE user = ?1 ORDER BY last_seen DESC")?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                json!({
                    "first_seen": r.get::<_, i64>(1)?, "last_seen": r.get::<_, i64>(2)?,
                    "requests": r.get::<_, i64>(3)?, "bytes": r.get::<_, i64>(4)?,
                    "playback_sessions": r.get::<_, i64>(5)?, "watch_time_s": round1(r.get::<_, f64>(6)?),
                }),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = vec![];
    for (name, mut v) in rows {
        let ds = devs.query_map([&name], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        v["name"] = json!(name);
        v["devices"] = json!(ds);
        out.push(v);
    }
    Ok(out)
}

pub fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::store::SCHEMA;

    #[test]
    fn buckets() {
        // 2024-03-14 15:09:26 UTC (a Thursday)
        let ts = 1710428966;
        assert_eq!(bucket_start(ts, Bucket::Hour), 1710428400);
        assert_eq!(bucket_start(ts, Bucket::Day), 1710374400);
        assert_eq!(bucket_start(ts, Bucket::Week), 1710115200); // Monday 2024-03-11
        assert_eq!(bucket_start(ts, Bucket::Month), 1709251200); // 2024-03-01
        assert_eq!(bucket_start(0, Bucket::Week), -3 * 86400); // epoch is a Thursday
    }

    /// Raw rows are authoritative from `raw_pruned_until`; hourly aggregates before it. Totals match.
    #[test]
    fn traffic_rollup_hybrid() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        // hour 0 (aggregate only, raw pruned) and hour 3600*5 (raw + aggregate both written at insert time)
        c.execute("INSERT INTO requests_hourly VALUES (0,'api','/api/items','2xx','d','u',10,1000,500)", []).unwrap();
        c.execute("INSERT INTO requests_hourly VALUES (18000,'api','/api/items','2xx','d','u',2,300,30)", []).unwrap();
        for (ts, b, d) in [(18010, 100, 10), (18020, 200, 20)] {
            c.execute("INSERT INTO requests (ts,grp,route,method,status,bytes,dur_ms,device,user) VALUES (?1,'api','/api/items','GET',200,?2,?3,'d','u')", rusqlite::params![ts, b, d]).unwrap();
        }
        super::super::store::meta_set(&c, "raw_pruned_until", "3600").unwrap();
        let rows = traffic(&c, 0, 20000, Some(Bucket::Hour), GroupBy::None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0]["requests"].as_i64(), rows[0]["bytes"].as_i64(), rows[0]["source"].as_str()), (Some(10), Some(1000), Some("aggregate")));
        assert!(rows[0]["p95_ms"].is_null());
        assert_eq!((rows[1]["requests"].as_i64(), rows[1]["p50_ms"].as_i64(), rows[1]["source"].as_str()), (Some(2), Some(10), Some("raw")));
        let one = traffic(&c, 0, 20000, None, GroupBy::Device).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0]["requests"], 12);
        assert_eq!(one[0]["source"], "mixed");
    }
}
