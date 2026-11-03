//! HTTP API: one router per resource, composed here.

mod fields;
mod ingest;
mod items;
mod libraries;
mod media;
mod people;
mod search;
pub mod views;

use crate::db::Db;
use crate::ingest::Ingest;
use crate::tasks::Maintenance;
use axum::{routing::get, Json, Router};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;

pub struct AppState {
    pub db: Db,
    pub data_dir: PathBuf,
    pub frames: Arc<crate::frames::FrameService>,
    pub ingest: Arc<Ingest>,
    pub tasks: Arc<Maintenance>,
}

pub type Shared = Arc<AppState>;

pub fn router(state: Shared) -> Router {
    let api = Router::new()
        .route("/api/health", get(|| async { Json(serde_json::json!({ "ok": true, "version": 3 })) }))
        .merge(libraries::routes())
        .merge(ingest::routes())
        .merge(items::routes())
        .merge(people::routes())
        .merge(fields::routes())
        .merge(search::routes())
        .layer(CompressionLayer::new());
    Router::new()
        .merge(api)
        .merge(media::routes())
        .fallback(|| async { crate::error::AppError::NotFound("no such route".into()) })
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// JSON body extractor whose rejections use the `{"error": ...}` shape.
pub struct JsonBody<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for JsonBody<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = crate::error::AppError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(JsonBody(v)),
            Err(e) => Err(crate::error::AppError::bad(e.body_text())),
        }
    }
}

/// Query extractor whose rejections use the `{"error": ...}` shape.
pub struct QueryParams<T>(pub T);

impl<S, T> axum::extract::FromRequestParts<S> for QueryParams<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = crate::error::AppError;

    async fn from_request_parts(parts: &mut axum::http::request::Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(v)) => Ok(QueryParams(v)),
            Err(e) => Err(crate::error::AppError::bad(e.body_text())),
        }
    }
}

pub fn ok() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}
