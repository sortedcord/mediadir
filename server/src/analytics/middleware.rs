//! Request-logging middleware: identity, route template, bytes actually streamed, duration.
//! The body is wrapped so bytes are counted as they leave (Range partials and client aborts
//! included); the event is queued to the writer thread when the body completes or is dropped.

use super::store::Msg;
use super::tracker::ReqEvent;
use super::Analytics;
use crate::db::now;
use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, MatchedPath, Request, State},
    http::{header, HeaderMap},
    middleware::Next,
    response::Response,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

pub const DEVICE_HEADER: &str = "x-mediadir-device";
pub const USER_HEADER: &str = "x-mediadir-user";

/// Who is calling. Stored in request extensions for handlers.
#[derive(Clone, Debug)]
pub struct Identity {
    pub device: String,
    pub user: Option<String>,
    pub user_agent: String,
    pub ip: String,
}

fn clean(s: &str, max: usize) -> Option<String> {
    let t: String = s.trim().chars().filter(|c| !c.is_control()).take(max).collect();
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    let fwd = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()))
        .and_then(|v| clean(v, 64));
    fwd.or_else(|| peer.map(|p| p.ip().to_string())).unwrap_or_default()
}

/// Header first, then `?device=` / `?user=`, else `anon:<ip>`.
pub fn identity(headers: &HeaderMap, query: Option<&str>, peer: Option<SocketAddr>) -> Identity {
    let q: Vec<(String, String)> = query.and_then(|q| serde_urlencoded::from_str(q).ok()).unwrap_or_default();
    let qp = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let hv = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let ip = client_ip(headers, peer);
    let device = hv(DEVICE_HEADER).and_then(|v| clean(v, 128)).or_else(|| qp("device").and_then(|v| clean(v, 128))).unwrap_or_else(|| format!("anon:{ip}"));
    let user = hv(USER_HEADER).and_then(|v| clean(v, 128)).or_else(|| qp("user").and_then(|v| clean(v, 128)));
    let user_agent = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).and_then(|v| clean(v, 300)).unwrap_or_default();
    Identity { device, user, user_agent, ip }
}

fn group_of(path: &str) -> &'static str {
    if path.starts_with("/stream/") {
        "stream"
    } else if path.starts_with("/asset/") {
        "asset"
    } else if path.starts_with("/frame/") {
        "frame"
    } else if path.starts_with("/api/") || path == "/api" {
        "api"
    } else {
        "other"
    }
}

/// Pull `item_id` / `file_id` out of the concrete path using the route template.
fn ids(template: &str, path: &str) -> (Option<String>, Option<String>) {
    let (mut item, mut file) = (None, None);
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    for (i, seg) in t.iter().enumerate() {
        let Some(actual) = p.get(i) else { break };
        match *seg {
            "{file_id}" => file = Some(actual.to_string()),
            "{id}" if i > 0 && t[i - 1] == "items" => item = Some(actual.to_string()),
            "{id}" if i > 0 && t[i - 1] == "files" => file = Some(actual.to_string()),
            _ => {}
        }
    }
    (item, file)
}

struct Guard {
    an: Arc<Analytics>,
    ev: ReqEvent,
    ua: String,
    ip: String,
    start: Instant,
    bytes: u64,
    done: Option<Instant>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let end = self.done.unwrap_or_else(Instant::now);
        let mut ev = self.ev.clone();
        ev.bytes = self.bytes;
        ev.dur_ms = end.duration_since(self.start).as_millis() as u64;
        ev.end_ts = now();
        let (ua, ip) = (std::mem::take(&mut self.ua), std::mem::take(&mut self.ip));
        self.an.send(Msg::Req { ev, ua, ip });
    }
}

struct Counted {
    inner: Body,
    guard: Guard,
}

impl HttpBody for Counted {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let r = Pin::new(&mut self.inner).poll_frame(cx);
        match &r {
            Poll::Ready(Some(Ok(f))) => {
                if let Some(d) = f.data_ref() {
                    self.guard.bytes += d.len() as u64;
                }
            }
            Poll::Ready(None) | Poll::Ready(Some(Err(_))) => {
                if self.guard.done.is_none() {
                    self.guard.done = Some(Instant::now());
                }
            }
            Poll::Pending => {}
        }
        r
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

fn content_range_start(headers: &HeaderMap) -> Option<u64> {
    let v = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    v.strip_prefix("bytes ")?.split('-').next()?.trim().parse().ok()
}

pub async fn log(State(an): State<Arc<Analytics>>, mut req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    let id = identity(req.headers(), req.uri().query(), peer);
    let path = req.uri().path().to_string();
    req.extensions_mut().insert(id.clone());
    if path.starts_with("/api/analytics") {
        return next.run(req).await; // not logged: the observer does not observe itself
    }
    let template = req.extensions().get::<MatchedPath>().map(|m| m.as_str().to_string());
    let method = req.method().to_string();
    let start = Instant::now();
    let ts = now();
    let resp = next.run(req).await;
    // unrouted requests (404 fallback) share one label so scanners cannot blow up cardinality
    let template = template.unwrap_or_else(|| "(unmatched)".to_string());
    let group = group_of(&path);
    let (item_id, file_id) = ids(&template, &path);
    let status = resp.status().as_u16();
    let is_stream_play = group == "stream" && method == "GET" && (status == 200 || status == 206);
    let range_start = is_stream_play.then(|| content_range_start(resp.headers()).unwrap_or(0));
    let mut started = false;
    if is_stream_play {
        if let Some(f) = &file_id {
            started = an.send(Msg::StreamStart { ts, device: id.device.clone(), user: id.user.clone(), file_id: f.clone() });
        }
    }
    let ev = ReqEvent {
        ts,
        end_ts: ts,
        group: group.into(),
        route: template,
        method,
        status,
        bytes: 0,
        dur_ms: 0,
        device: id.device,
        user: id.user,
        item_id,
        file_id,
        range_start,
        started,
    };
    let guard = Guard { an, ev, ua: id.user_agent, ip: id.ip, start, bytes: 0, done: None };
    let (parts, body) = resp.into_parts();
    Response::from_parts(parts, Body::new(Counted { inner: body, guard }))
}
