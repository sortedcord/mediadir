//! Virtual libraries: one per item kind that has items. Creating, renaming and deleting
//! libraries (and scanning them) no longer exists; media enters through the ingest pipeline.

use super::Shared;
use crate::error::{AppError, AppResult};
use crate::model::{Library, LibraryKind};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;

#[derive(Serialize)]
pub struct LibraryDto {
    #[serde(flatten)]
    pub library: Library,
    pub item_count: i64,
    pub scanning: bool,
    pub available: bool,
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/libraries", get(list).post(gone).patch(gone).delete(gone))
        .route("/api/libraries/{id}", get(get_one).post(gone).patch(gone).delete(gone))
}

async fn gone() -> Response {
    let body = Json(serde_json::json!({
        "error": "libraries are virtual (one per item kind); add media through the ingest dir or POST /api/ingest/upload"
    }));
    (StatusCode::METHOD_NOT_ALLOWED, [("allow", "GET")], body).into_response()
}

async fn all(st: &Shared) -> AppResult<Vec<LibraryDto>> {
    let counts = st.db.call(|r| r.top_level_counts()).await?;
    Ok(LibraryKind::ALL
        .into_iter()
        .filter_map(|k| {
            let n = counts.get(&k.id()).copied().unwrap_or(0);
            (n > 0).then(|| LibraryDto { library: Library::virtual_for(k), item_count: n, scanning: false, available: true })
        })
        .collect())
}

async fn list(State(st): State<Shared>) -> AppResult<Json<Vec<LibraryDto>>> {
    Ok(Json(all(&st).await?))
}

async fn get_one(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<LibraryDto>> {
    let kind = LibraryKind::from_id(&id).ok_or_else(|| AppError::not_found("library"))?;
    let lib = all(&st).await?.into_iter().find(|l| l.library.kind == kind);
    Ok(Json(lib.unwrap_or(LibraryDto { library: Library::virtual_for(kind), item_count: 0, scanning: false, available: true })))
}
