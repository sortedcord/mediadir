//! Byte serving: stored media (Range/HEAD via ServeFile) and still frames.

use super::Shared;
use crate::error::{AppError, AppResult};
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::path::PathBuf;
use tower::ServiceExt;
use tower_http::services::ServeFile;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/stream/{file_id}", get(stream).head(stream))
        .route("/frame/{file_id}", get(frame))
}

pub(super) async fn serve(path: PathBuf, mime: &str, req: Request) -> AppResult<Response> {
    let mut r = ServeFile::new(path)
        .oneshot(req)
        .await
        .map_err(|e| AppError::Internal(format!("serve: {e}")))?
        .map(Body::new)
        .into_response();
    if r.status() == axum::http::StatusCode::NOT_FOUND {
        return Err(AppError::NotFound("stored file is missing".into()));
    }
    if let Ok(h) = HeaderValue::from_str(mime) {
        r.headers_mut().insert(header::CONTENT_TYPE, h);
    }
    Ok(r)
}

async fn stream(State(st): State<Shared>, Path(file_id): Path<String>, req: Request) -> AppResult<Response> {
    let file = st.db.call(move |r| r.get_file(&file_id)).await?;
    let mut resp = serve(st.ingest.store.abs(&file.store_path), &file.mime, req).await?;
    resp.headers_mut().insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    Ok(resp)
}

#[derive(serde::Deserialize)]
struct FrameParams {
    t: Option<f64>,
    max_w: Option<u32>,
    fmt: Option<String>,
}

/// One high-quality still frame at `t` (frame-accurate), cached on disk.
async fn frame(
    State(st): State<Shared>,
    Path(file_id): Path<String>,
    super::QueryParams(p): super::QueryParams<FrameParams>,
    req: Request,
) -> AppResult<Response> {
    let fmt = crate::frames::FrameFormat::parse(p.fmt.as_deref().unwrap_or("jpg"))
        .ok_or_else(|| AppError::bad("fmt must be jpg or webp"))?;
    let t = p.t.ok_or_else(|| AppError::bad("t is required"))?;
    let file = st.db.call(move |r| r.get_file(&file_id)).await?;
    if file.video.is_none() {
        return Err(AppError::bad("file has no video stream"));
    }
    let max_t = file.duration.unwrap_or(f64::MAX);
    if !t.is_finite() || t < 0.0 || t > max_t {
        return Err(AppError::bad(format!("t must be within 0..{max_t:.3}")));
    }
    let max_w = p.max_w.filter(|w| *w > 0);
    let src = st.ingest.store.abs(&file.store_path);
    if !src.is_file() {
        return Err(AppError::NotFound("stored file is missing".into()));
    }
    let f = st.frames.frame(&file, &src, t.min((max_t - 0.001).max(0.0)), max_w, fmt).await?;
    let mut r = serve(f.path, fmt.mime(), req).await?;
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000"));
    for (k, v) in [
        ("x-frame-width", f.info.width.to_string()),
        ("x-frame-height", f.info.height.to_string()),
        ("x-frame-pts", format!("{:.3}", f.info.pts)),
        ("x-frame-cache", if f.cached { "hit".into() } else { "miss".into() }),
    ] {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    }
    Ok(r)
}
