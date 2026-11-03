//! Ingest endpoints (docs/STORAGE.md): records, status, forced poll, upload; plus deletes.

use super::{QueryParams, Shared};
use crate::db::IngestRecord;
use crate::error::{AppError, AppResult};
use crate::ingest::{Deleted, QueueStatus};
use crate::tasks::TaskStatus;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/ingest", get(list))
        .route("/api/ingest/status", get(status))
        .route("/api/ingest/scan", post(scan))
        .route("/api/ingest/upload", post(upload).layer(DefaultBodyLimit::disable()))
        .route("/api/ingest/{id}", get(get_one))
        .route("/api/files/{id}", delete(delete_file))
}

const STATES: &[&str] = &["waiting", "hashing", "placing", "probing", "done", "duplicate", "failed", "ignored"];

#[derive(Deserialize)]
struct ListParams {
    state: Option<String>,
    limit: Option<i64>,
}

async fn list(State(st): State<Shared>, QueryParams(p): QueryParams<ListParams>) -> AppResult<Json<Vec<IngestRecord>>> {
    if let Some(s) = &p.state {
        if !STATES.contains(&s.as_str()) {
            return Err(AppError::bad(format!("state must be one of {}", STATES.join(", "))));
        }
    }
    let limit = p.limit.unwrap_or(100);
    Ok(Json(st.db.call(move |r| r.list_ingests(p.state.as_deref(), limit)).await?))
}

async fn get_one(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<IngestRecord>> {
    Ok(Json(st.db.call(move |r| r.get_ingest(&id)).await?))
}

#[derive(Serialize)]
struct Status {
    #[serde(flatten)]
    queue: QueueStatus,
    ingest_dir: String,
    media_dir: String,
    asset_dir: String,
    data_dir: String,
    /// EXTERNAL_ROOT (null = referenced media is off).
    external_root: Option<String>,
    /// Referenced files whose full hash is still being computed in the background.
    pending_hashes: i64,
    #[serde(flatten)]
    tasks: TaskStatus,
}

async fn status(State(st): State<Shared>) -> AppResult<Json<Status>> {
    let tasks = st.tasks.clone();
    let (task_status, pending_hashes) = st.db.call(move |r| Ok((tasks.status(r)?, r.count_pending_hashes()?))).await?;
    Ok(Json(Status {
        queue: st.ingest.status(),
        ingest_dir: st.ingest.ingest_dir.to_string_lossy().to_string(),
        media_dir: st.ingest.store.root().to_string_lossy().to_string(),
        asset_dir: st.assets.store.root().to_string_lossy().to_string(),
        data_dir: st.data_dir.to_string_lossy().to_string(),
        external_root: st.ingest.external_root.as_ref().map(|p| p.to_string_lossy().to_string()),
        pending_hashes,
        tasks: task_status,
    }))
}

/// Force a poll of the ingest dir; files whose ingest failed are retried.
async fn scan(State(st): State<Shared>) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    let ing = st.ingest.clone();
    let retried = tokio::task::spawn_blocking(move || -> AppResult<usize> {
        let n = ing.forget_failed()?;
        ing.scan_dir(&ing.ingest_dir.clone());
        Ok(n)
    })
    .await??;
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({ "ok": true, "retrying_failed": retried }))))
}

/// `%XX` decoding for `X-Filename`.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Raw file as the body, original name (URL-encoded) in `X-Filename`. Returns the ingest record:
/// `201` when it was ingested, `200` otherwise (`duplicate`, `ignored`, `failed`).
async fn upload(State(st): State<Shared>, headers: HeaderMap, body: Body) -> AppResult<(StatusCode, Json<IngestRecord>)> {
    let raw = headers
        .get("x-filename")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::bad("X-Filename header (URL-encoded file name) is required"))?;
    let name = percent_decode(raw).ok_or_else(|| AppError::bad("X-Filename is not valid URL-encoded UTF-8"))?;
    let rec = st.ingest.upload(&name, body).await?;
    let code = if rec.state == "done" { StatusCode::CREATED } else { StatusCode::OK };
    Ok((code, Json(rec)))
}

fn deleted(d: Deleted) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "items": d.items, "files": d.files }))
}

/// `DELETE /api/items/{id}`: the item, its children and every stored file they referenced.
pub(super) async fn delete_item(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<serde_json::Value>> {
    let ing = st.ingest.clone();
    Ok(deleted(tokio::task::spawn_blocking(move || ing.delete_item(&id)).await??))
}

/// `DELETE /api/files/{id}`: one version (the item too when it was the last one).
async fn delete_file(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<serde_json::Value>> {
    let ing = st.ingest.clone();
    Ok(deleted(tokio::task::spawn_blocking(move || ing.delete_file(&id)).await??))
}

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_filenames() {
        assert_eq!(super::percent_decode("Big%20Buck%20(2008).mkv").as_deref(), Some("Big Buck (2008).mkv"));
        assert_eq!(super::percent_decode("%EF%BD%9C").as_deref(), Some("｜"));
        assert_eq!(super::percent_decode("bad%2"), None);
    }
}
