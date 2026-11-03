//! Image assets: upload and the pre-processing delivery endpoint (docs/STORAGE.md#image-assets).

use super::{QueryParams, Shared};
use crate::assets::{plan, Fit, Fmt, Request as Transform};
use crate::error::{AppError, AppResult};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/assets", post(upload).layer(DefaultBodyLimit::disable()))
        .route("/asset/{hash}", get(deliver))
}

async fn upload(State(st): State<Shared>, body: Body) -> AppResult<(StatusCode, Json<crate::db::Asset>)> {
    let (asset, existed) = st.assets.upload(&st.db, body).await?;
    Ok((if existed { StatusCode::OK } else { StatusCode::CREATED }, Json(asset)))
}

#[derive(Deserialize, Default)]
struct Params {
    w: Option<u32>,
    h: Option<u32>,
    fit: Option<String>,
    fmt: Option<String>,
    q: Option<u32>,
    original: Option<String>,
}

fn truthy(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

fn immutable(r: &mut Response, etag: &str) {
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    if let Ok(v) = HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
}

fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim().trim_start_matches("W/") == etag || t.trim() == "*"))
}

async fn deliver(
    State(st): State<Shared>,
    Path(hash): Path<String>,
    QueryParams(p): QueryParams<Params>,
    headers: HeaderMap,
    req: Request,
) -> AppResult<Response> {
    if p.w == Some(0) || p.h == Some(0) {
        return Err(AppError::bad("w and h must be positive"));
    }
    let fit = match p.fit.as_deref() {
        None => None,
        Some("cover") => Some(Fit::Cover),
        Some("contain") => Some(Fit::Contain),
        Some(_) => return Err(AppError::bad("fit must be cover or contain")),
    };
    let fmt = match p.fmt.as_deref() {
        None => None,
        Some(f) => Some(Fmt::parse(f).ok_or_else(|| AppError::bad("fmt must be webp, jpg or png"))?),
    };
    if p.q.is_some_and(|q| !(1..=100).contains(&q)) {
        return Err(AppError::bad("q must be 1..100"));
    }
    let t = Transform { w: p.w, h: p.h, fit, fmt, q: p.q.map(|q| q as u8) };

    let hash = hash.to_ascii_lowercase();
    let h2 = hash.clone();
    let asset = st
        .db
        .call(move |r| r.find_asset(&h2))
        .await?
        .filter(|_| hash.len() == 64)
        .ok_or_else(|| AppError::not_found("asset"))?;

    if p.original.as_deref().is_some_and(truthy) || !t.is_transform() {
        let etag = format!("\"{}\"", asset.hash);
        let mut r = if not_modified(&headers, &etag) {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            super::media::serve(st.assets.path(&asset), &asset.mime, req).await?
        };
        immutable(&mut r, &etag);
        r.headers_mut().insert("x-asset-cache", HeaderValue::from_static("original"));
        return Ok(r);
    }

    let pl = plan(asset.width, asset.height, &t, st.assets.default_width);
    let etag = format!("\"{}-{}\"", asset.hash, pl.key());
    let (mut r, cache) = if not_modified(&headers, &etag) {
        (StatusCode::NOT_MODIFIED.into_response(), "hit")
    } else {
        if !st.assets.path(&asset).is_file() {
            return Err(AppError::NotFound("asset file is missing".into()));
        }
        let (path, hit) = st.assets.render(&asset, &pl).await?;
        (super::media::serve(path, pl.fmt.mime(), req).await?, if hit { "hit" } else { "miss" })
    };
    immutable(&mut r, &etag);
    let h = r.headers_mut();
    h.insert("x-asset-cache", HeaderValue::from_static(cache));
    h.insert("x-asset-width", HeaderValue::from(pl.w));
    h.insert("x-asset-height", HeaderValue::from(pl.h));
    Ok(r)
}
