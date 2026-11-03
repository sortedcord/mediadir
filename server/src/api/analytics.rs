//! Analytics data endpoints (docs/API.md "Analytics"). JSON only; no visualization.

use super::{JsonBody, QueryParams, Shared};
use crate::analytics::middleware::Identity;
use crate::analytics::query::{self, Bucket, GroupBy, SessionQuery, TopQuery};
use crate::analytics::store::Msg;
use crate::analytics::tracker::{FileMeta, PlayEvent};
use crate::db::{now, Repo};
use crate::error::{AppError, AppResult};
use axum::{
    extract::{Extension, Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/analytics/overview", get(overview))
        .route("/api/analytics/library", get(library))
        .route("/api/analytics/traffic", get(traffic))
        .route("/api/analytics/sessions", get(sessions))
        .route("/api/analytics/sessions/{id}", get(session))
        .route("/api/analytics/now-playing", get(now_playing))
        .route("/api/analytics/top", get(top))
        .route("/api/analytics/users", get(users))
        .route("/api/analytics/devices", get(devices))
        .route("/api/analytics/playback", post(playback))
}

fn range(from: Option<i64>, to: Option<i64>, default_span: i64) -> AppResult<(i64, i64)> {
    let to = to.unwrap_or_else(now);
    let from = from.unwrap_or(to - default_span);
    if from < 0 || to < 0 {
        return Err(AppError::bad("from/to must be unix seconds"));
    }
    if from >= to {
        return Err(AppError::bad("from must be before to"));
    }
    Ok((from, to))
}

fn flag(v: &Option<String>, name: &str) -> AppResult<Option<bool>> {
    match v.as_deref() {
        None | Some("") => Ok(None),
        Some("true") | Some("1") => Ok(Some(true)),
        Some("false") | Some("0") => Ok(Some(false)),
        Some(_) => Err(AppError::bad(format!("{name} must be true or false"))),
    }
}

/// Compact item description for session/top rows; `None` when the item no longer exists.
fn item_brief(r: &Repo, id: &str) -> Option<Value> {
    let it = r.find_item(id).ok().flatten()?;
    let mut parents = vec![];
    let mut cur = it.parent_id.clone();
    while let Some(pid) = cur {
        if parents.len() >= 3 {
            break;
        }
        let Some(p) = r.find_item(&pid).ok().flatten() else { break };
        parents.push(json!({ "id": p.id, "kind": p.kind.as_str(), "title": p.title }));
        cur = p.parent_id.clone();
    }
    Some(json!({
        "id": it.id, "kind": it.kind.as_str(), "title": it.title, "subtitle": it.subtitle, "year": it.year,
        "index": it.index, "parent_index": it.parent_index, "parent_id": it.parent_id, "parents": parents,
    }))
}

async fn attach_items(st: &Shared, mut rows: Vec<Value>, id_field: &str) -> AppResult<Vec<Value>> {
    let ids: Vec<String> = rows.iter().filter_map(|v| v[id_field].as_str().map(String::from)).collect();
    let briefs = st.db.call(move |r| Ok(ids.into_iter().map(|i| (i.clone(), item_brief(r, &i))).collect::<Vec<_>>())).await?;
    let map: std::collections::HashMap<_, _> = briefs.into_iter().collect();
    for v in &mut rows {
        let item = v[id_field].as_str().and_then(|i| map.get(i)).cloned().flatten();
        v["item"] = item.unwrap_or(Value::Null);
    }
    Ok(rows)
}

async fn overview(State(st): State<Shared>) -> AppResult<Json<Value>> {
    let (db, ingest, tasks, paths_an) = (st.db.clone(), st.ingest.clone(), st.tasks.clone(), st.analytics.clone());
    let library = {
        let an = paths_an.clone();
        tokio::task::spawn_blocking(move || crate::analytics::snapshot::collect(&db, &ingest, &tasks, &an.snapshot_paths)).await??
    };
    let dropped = st.analytics.dropped.load(std::sync::atomic::Ordering::Relaxed);
    let t = now();
    let mut v = st
        .analytics
        .db
        .call(move |c| {
            let mut traffic = serde_json::Map::new();
            for (name, span) in [("last_24h", 86400), ("last_7d", 7 * 86400), ("last_30d", 30 * 86400)] {
                let rows = query::traffic(c, t - span, t + 1, None, GroupBy::Group)?;
                let (mut req, mut bytes) = (0, 0);
                let mut by = serde_json::Map::new();
                for r in &rows {
                    req += r["requests"].as_u64().unwrap_or(0);
                    bytes += r["bytes"].as_u64().unwrap_or(0);
                    by.insert(r["key"]["group"].as_str().unwrap_or("").into(), json!({ "requests": r["requests"], "bytes": r["bytes"] }));
                }
                traffic.insert(name.into(), json!({ "requests": req, "bytes": bytes, "by_group": by }));
            }
            let plays_24h = query::count(c, &format!("SELECT COUNT(*) FROM playback_sessions WHERE started_at >= {}", t - 86400));
            let watch_24h: f64 = c
                .query_row(&format!("SELECT COALESCE(SUM(watched_s),0) FROM playback_sessions WHERE started_at >= {}", t - 86400), [], |r| r.get(0))
                .unwrap_or(0.0);
            Ok(json!({
                "ts": t,
                "traffic": traffic,
                "playback": { "plays_24h": plays_24h, "watch_time_s_24h": (watch_24h * 10.0).round() / 10.0 },
                "active": {
                    "playback_sessions": query::count(c, "SELECT COUNT(*) FROM playback_sessions WHERE ended_at IS NULL"),
                    "device_sessions": query::count(c, "SELECT COUNT(*) FROM device_sessions WHERE ended_at IS NULL"),
                },
                "devices": query::count(c, "SELECT COUNT(*) FROM devices"),
                "users": query::count(c, "SELECT COUNT(DISTINCT user) FROM device_users"),
                "last_snapshot_at": query::latest_snapshot(c).and_then(|_| c.query_row("SELECT MAX(ts) FROM snapshots", [], |r| r.get::<_, i64>(0)).ok()),
            }))
        })
        .await?;
    v["library"] = library;
    v["dropped_events"] = json!(dropped);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct LibraryParams {
    from: Option<i64>,
    to: Option<i64>,
    bucket: Option<String>,
}

async fn library(State(st): State<Shared>, QueryParams(p): QueryParams<LibraryParams>) -> AppResult<Json<Value>> {
    let bucket = Bucket::parse(p.bucket.as_deref().unwrap_or("day"))?;
    let (from, to) = range(p.from, p.to, 30 * 86400)?;
    let series = st.analytics.db.call(move |c| query::library(c, from, to, bucket)).await?;
    Ok(Json(json!({ "from": from, "to": to, "bucket": bucket.name(), "series": series })))
}

#[derive(Deserialize)]
struct TrafficParams {
    from: Option<i64>,
    to: Option<i64>,
    bucket: Option<String>,
    group_by: Option<String>,
}

async fn traffic(State(st): State<Shared>, QueryParams(p): QueryParams<TrafficParams>) -> AppResult<Json<Value>> {
    let bucket = Bucket::parse(p.bucket.as_deref().unwrap_or("hour"))?;
    let group_by = GroupBy::parse(p.group_by.as_deref().unwrap_or("none"))?;
    let (from, to) = range(p.from, p.to, 7 * 86400)?;
    let rows = st.analytics.db.call(move |c| query::traffic(c, from, to, Some(bucket), group_by)).await?;
    let (req, bytes) = rows.iter().fold((0, 0), |a, r| (a.0 + r["requests"].as_u64().unwrap_or(0), a.1 + r["bytes"].as_u64().unwrap_or(0)));
    let gb = p.group_by.unwrap_or_else(|| "none".into());
    Ok(Json(json!({ "from": from, "to": to, "bucket": bucket.name(), "group_by": gb, "totals": { "requests": req, "bytes": bytes }, "rows": rows })))
}

#[derive(Deserialize)]
struct SessionParams {
    #[serde(rename = "type")]
    kind: Option<String>,
    active: Option<String>,
    device: Option<String>,
    user: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn sessions(State(st): State<Shared>, QueryParams(p): QueryParams<SessionParams>) -> AppResult<Json<Value>> {
    let q = SessionQuery {
        kind: p.kind.unwrap_or_else(|| "playback".into()),
        active: flag(&p.active, "active")?,
        device: p.device,
        user: p.user,
        from: p.from,
        to: p.to,
        limit: p.limit.unwrap_or(50).clamp(1, 500),
        offset: p.offset.unwrap_or(0).max(0),
    };
    let (kind, limit, offset) = (q.kind.clone(), q.limit, q.offset);
    let (total, rows) = st.analytics.db.call(move |c| query::sessions(c, &q)).await?;
    let rows = if kind == "playback" { attach_items(&st, rows, "item_id").await? } else { rows };
    Ok(Json(json!({ "type": kind, "total": total, "limit": limit, "offset": offset, "sessions": rows })))
}

async fn session(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let v = st.analytics.db.call(move |c| query::session(c, &id)).await?;
    let v = if v["type"] == "playback" { attach_items(&st, vec![v], "item_id").await?.remove(0) } else { v };
    Ok(Json(v))
}

async fn now_playing(State(st): State<Shared>) -> AppResult<Json<Value>> {
    let q = SessionQuery { kind: "playback".into(), active: Some(true), device: None, user: None, from: None, to: None, limit: 500, offset: 0 };
    let (total, rows) = st.analytics.db.call(move |c| query::sessions(c, &q)).await?;
    let rows = attach_items(&st, rows, "item_id").await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|s| {
            json!({
                "session_id": s["id"], "user": s["user"], "device": s["device"], "item": s["item"], "item_id": s["item_id"], "file_id": s["file_id"],
                "state": s["state"], "source": s["source"], "position_s": s["position_s"], "duration_s": s["duration_s"],
                "started_at": s["started_at"], "last_at": s["last_at"], "watched_s": s["watched_s"], "bytes": s["bytes"],
            })
        })
        .collect();
    Ok(Json(json!({ "ts": now(), "total": total, "sessions": out })))
}

#[derive(Deserialize)]
struct TopParams {
    metric: Option<String>,
    by: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<i64>,
}

async fn top(State(st): State<Shared>, QueryParams(p): QueryParams<TopParams>) -> AppResult<Json<Value>> {
    let (from, to) = range(p.from, p.to, 30 * 86400)?;
    let q = TopQuery {
        metric: p.metric.unwrap_or_else(|| "plays".into()),
        by: p.by.unwrap_or_else(|| "item".into()),
        from,
        to,
        limit: p.limit.unwrap_or(10).clamp(1, 200),
    };
    let (metric, by) = (q.metric.clone(), q.by.clone());
    let mut rows = st.analytics.db.call(move |c| query::top(c, &q)).await?;
    if by == "item" {
        for r in &mut rows {
            r["item_id"] = r["key"].clone();
        }
        rows = attach_items(&st, rows, "item_id").await?;
    }
    Ok(Json(json!({ "metric": metric, "by": by, "from": from, "to": to, "rows": rows })))
}

async fn users(State(st): State<Shared>) -> AppResult<Json<Value>> {
    let rows = st.analytics.db.call(query::users).await?;
    Ok(Json(json!({ "total": rows.len(), "users": rows })))
}

async fn devices(State(st): State<Shared>) -> AppResult<Json<Value>> {
    let rows = st.analytics.db.call(query::devices).await?;
    Ok(Json(json!({ "total": rows.len(), "devices": rows })))
}

#[derive(Deserialize)]
struct PlaybackBody {
    file_id: Option<String>,
    item_id: Option<String>,
    event: String,
    position_s: f64,
    duration_s: Option<f64>,
}

async fn playback(State(st): State<Shared>, Extension(id): Extension<Identity>, JsonBody(b): JsonBody<PlaybackBody>) -> AppResult<Json<Value>> {
    let ev = PlayEvent::parse(&b.event).ok_or_else(|| AppError::bad("event must be start, progress, pause, resume or stop"))?;
    if !b.position_s.is_finite() || b.position_s < 0.0 {
        return Err(AppError::bad("position_s must be a number >= 0"));
    }
    if b.duration_s.is_some_and(|d| !d.is_finite() || d < 0.0) {
        return Err(AppError::bad("duration_s must be a number >= 0"));
    }
    if b.file_id.is_none() && b.item_id.is_none() {
        return Err(AppError::bad("file_id or item_id is required"));
    }
    let (fid, iid) = (b.file_id.clone(), b.item_id.clone());
    let (file_id, meta) = st
        .db
        .call(move |r| {
            let f = match (&fid, &iid) {
                (Some(f), _) => r.get_file(f)?,
                (None, Some(i)) => r.files_for_item(i)?.into_iter().next().ok_or_else(|| AppError::not_found("item (or it has no files)"))?,
                _ => unreachable!(),
            };
            let kind = r.find_item(&f.item_id)?.map(|i| i.kind.as_str().to_string()).unwrap_or_default();
            Ok((f.id.clone(), FileMeta { item_id: f.item_id.clone(), item_kind: kind, size: f.size, duration: f.duration }))
        })
        .await?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let queued = st.analytics.send(Msg::Play {
        ts: now(),
        device: id.device,
        user: id.user,
        ua: id.user_agent,
        ip: id.ip,
        file_id,
        meta,
        ev,
        pos: b.position_s,
        dur: b.duration_s,
        reply: tx,
    });
    if !queued {
        return Err(AppError::Internal("analytics queue is full".into()));
    }
    let row = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .map_err(|_| AppError::Internal("analytics writer timed out".into()))?
        .map_err(|_| AppError::Internal("analytics writer stopped".into()))?;
    Ok(Json(json!({
        "ok": true, "session_id": row.id, "source": row.source, "state": row.state, "active": row.ended_at.is_none(),
        "file_id": row.file_id, "item_id": row.item_id, "position_s": row.position_s, "watched_s": (row.watched_s * 10.0).round() / 10.0,
    })))
}
