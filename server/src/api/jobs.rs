//! Jobs and schedules (docs/API.md "Jobs").

use super::{JsonBody, QueryParams, Shared};
use crate::error::{AppError, AppResult};
use crate::jobs::store::JobFilter;
use crate::jobs::{Job, NewJob, Schedule, SchedulePatch, STATES};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::time::Duration;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/jobs", get(list).post(create))
        .route("/api/jobs/stats", get(stats))
        .route("/api/jobs/{id}", get(get_one).delete(delete_one))
        .route("/api/jobs/{id}/cancel", post(cancel))
        .route("/api/jobs/{id}/pause", post(pause))
        .route("/api/jobs/{id}/resume", post(resume))
        .route("/api/jobs/{id}/retry", post(retry))
        .route("/api/schedules", get(schedules))
        .route("/api/schedules/{id}", put(update_schedule).get(get_schedule))
        .route("/api/schedules/{id}/run", post(run_schedule))
}

/// Outside the compression layer (an event stream must not be buffered).
pub fn sse_routes() -> Router<Shared> {
    Router::new().route("/api/jobs/events", get(events))
}

#[derive(Deserialize)]
struct ListParams {
    state: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list(State(st): State<Shared>, QueryParams(p): QueryParams<ListParams>) -> AppResult<Json<Value>> {
    if let Some(s) = &p.state {
        if !STATES.contains(&s.as_str()) {
            return Err(AppError::bad(format!("state must be one of {}", STATES.join(", "))));
        }
    }
    if let Some(k) = &p.kind {
        if !st.jobs.types().contains(k) {
            return Err(AppError::bad(format!("type must be one of {}", st.jobs.types().join(", "))));
        }
    }
    let f = JobFilter { state: p.state, kind: p.kind, limit: p.limit.unwrap_or(50).clamp(1, 1000), offset: p.offset.unwrap_or(0).max(0) };
    let (limit, offset) = (f.limit, f.offset);
    let (jobs, total) = st.db.call(move |r| r.list_jobs(&f)).await?;
    Ok(Json(json!({ "jobs": jobs, "total": total, "limit": limit, "offset": offset })))
}

fn with_logs(job: Job, logs: Vec<Value>) -> Value {
    let mut v = serde_json::to_value(job).unwrap_or_default();
    v["logs"] = json!(logs);
    v
}

async fn get_one(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let (job, logs) = st.db.call(move |r| Ok((r.get_job(&id)?, r.job_logs(&id)?))).await?;
    Ok(Json(with_logs(job, logs)))
}

async fn create(State(st): State<Shared>, JsonBody(req): JsonBody<NewJob>) -> AppResult<(StatusCode, Json<Job>)> {
    let jobs = st.jobs.clone();
    let job = tokio::task::spawn_blocking(move || jobs.enqueue(req)).await??;
    Ok((StatusCode::CREATED, Json(job)))
}

async fn op(st: Shared, id: String, f: fn(&crate::jobs::Jobs, &str) -> AppResult<Job>) -> AppResult<Json<Job>> {
    let jobs = st.jobs.clone();
    Ok(Json(tokio::task::spawn_blocking(move || f(&jobs, &id)).await??))
}

async fn cancel(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Job>> {
    op(st, id, |j, id| j.cancel(id)).await
}
async fn pause(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Job>> {
    op(st, id, |j, id| j.pause(id)).await
}
async fn resume(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Job>> {
    op(st, id, |j, id| j.resume(id)).await
}
async fn retry(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Job>> {
    op(st, id, |j, id| j.retry(id)).await
}

async fn delete_one(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let jobs = st.jobs.clone();
    tokio::task::spawn_blocking(move || jobs.delete(&id)).await??;
    Ok(super::ok())
}

async fn stats(State(st): State<Shared>) -> AppResult<Json<Value>> {
    let jobs = st.jobs.clone();
    Ok(Json(tokio::task::spawn_blocking(move || jobs.stats()).await??))
}

async fn schedules(State(st): State<Shared>) -> AppResult<Json<Vec<Schedule>>> {
    let jobs = st.jobs.clone();
    Ok(Json(tokio::task::spawn_blocking(move || jobs.schedules()).await??))
}

async fn get_schedule(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Schedule>> {
    Ok(Json(st.db.call(move |r| r.get_schedule(&id)).await?))
}

async fn update_schedule(State(st): State<Shared>, Path(id): Path<String>, JsonBody(p): JsonBody<SchedulePatch>) -> AppResult<Json<Schedule>> {
    let jobs = st.jobs.clone();
    Ok(Json(tokio::task::spawn_blocking(move || jobs.update_schedule(&id, p)).await??))
}

async fn run_schedule(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<(StatusCode, Json<Job>)> {
    let jobs = st.jobs.clone();
    Ok((StatusCode::CREATED, Json(tokio::task::spawn_blocking(move || jobs.run_schedule(&id)).await??)))
}

#[derive(Deserialize)]
struct EventParams {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
}

/// Server-sent events: one `job` event (JSON [`crate::jobs::JobEvent`]) per state change /
/// progress update; `lagged` when the client fell behind (re-read `GET /api/jobs` then).
async fn events(
    State(st): State<Shared>,
    QueryParams(p): QueryParams<EventParams>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = st.jobs.events.subscribe();
    let hello = Event::default().event("hello").data(json!({ "ok": true }).to_string());
    let stream = futures_util::stream::unfold((rx, Some(hello), p), |(mut rx, first, p)| async move {
        if let Some(h) = first {
            return Some((Ok(h), (rx, None, p)));
        }
        loop {
            match rx.recv().await {
                Ok(e) => {
                    if p.kind.as_ref().is_some_and(|k| *k != e.kind) || p.id.as_ref().is_some_and(|i| *i != e.id) {
                        continue;
                    }
                    let ev = Event::default().event("job").data(serde_json::to_string(&e).unwrap_or_default());
                    return Some((Ok(ev), (rx, None, p)));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    let ev = Event::default().event("lagged").data(json!({ "missed": n }).to_string());
                    return Some((Ok(ev), (rx, None, p)));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
