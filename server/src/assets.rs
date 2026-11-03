//! Image assets: content-addressed originals under ASSET_DIR plus an on-demand, disk-cached
//! pre-processor (resize / re-encode with ffmpeg). Originals are never modified.

use crate::db::{Asset, Db};
use crate::error::{AppError, AppResult};
use crate::ingest::store::Store;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tokio::sync::Semaphore;

pub const MAX_UPLOAD: u64 = 100 * 1024 * 1024;
const MAX_PIXELS: u64 = 400_000_000;
const MAX_TRANSFORMS: usize = 4;
pub const DEFAULT_QUALITY: u8 = 80;

pub struct AssetService {
    pub store: Store,
    cache: PathBuf,
    pub default_width: u32,
    sem: Semaphore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Cover,
    Contain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    Webp,
    Jpg,
    Png,
}

impl Fmt {
    pub fn parse(s: &str) -> Option<Fmt> {
        match s.to_ascii_lowercase().as_str() {
            "webp" => Some(Fmt::Webp),
            "jpg" | "jpeg" => Some(Fmt::Jpg),
            "png" => Some(Fmt::Png),
            _ => None,
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            Fmt::Webp => "webp",
            Fmt::Jpg => "jpg",
            Fmt::Png => "png",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Fmt::Webp => "image/webp",
            Fmt::Jpg => "image/jpeg",
            Fmt::Png => "image/png",
        }
    }
}

/// Raw transform request (already syntactically valid).
#[derive(Debug, Clone, Copy, Default)]
pub struct Request {
    pub w: Option<u32>,
    pub h: Option<u32>,
    pub fit: Option<Fit>,
    pub fmt: Option<Fmt>,
    pub q: Option<u8>,
}

impl Request {
    pub fn is_transform(&self) -> bool {
        self.w.is_some() || self.h.is_some() || self.fit.is_some() || self.fmt.is_some() || self.q.is_some()
    }
}

/// A normalized transform: the exact output size and encoding. Two requests with the same plan
/// produce the same bytes and share one cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub w: u32,
    pub h: u32,
    /// true: scale to cover `w`x`h` and center-crop; false: plain scale to `w`x`h`.
    pub crop: bool,
    pub fmt: Fmt,
    /// 1..=100; 0 for png (lossless, not part of the key).
    pub q: u8,
}

impl Plan {
    pub fn key(&self) -> String {
        format!("{}x{}_{}_{}_q{}", self.w, self.h, if self.crop { "cover" } else { "scale" }, self.fmt.ext(), self.q)
    }
}

/// Resolve a request against the original's size. Never upscales.
pub fn plan(orig_w: u32, orig_h: u32, r: &Request, default_width: u32) -> Plan {
    let (ow, oh) = (orig_w.max(1) as f64, orig_h.max(1) as f64);
    let px = |v: f64| (v.round() as u32).max(1);
    let (w, h, crop) = match (r.w, r.h) {
        (Some(w), Some(h)) => {
            let (w, h) = (w as f64, h as f64);
            if r.fit == Some(Fit::Contain) {
                let s = (w / ow).min(h / oh).min(1.0);
                (px(ow * s), px(oh * s), false)
            } else {
                // cover: the output box itself may not exceed the original
                let s = (ow / w).min(oh / h).min(1.0);
                (px(w * s), px(h * s), true)
            }
        }
        (Some(w), None) => scale_to_width(ow, oh, w as f64),
        (None, Some(h)) => {
            let h = (h as f64).min(oh);
            (px(ow * h / oh), px(h), false)
        }
        (None, None) => scale_to_width(ow, oh, default_width as f64),
    };
    let fmt = r.fmt.unwrap_or(Fmt::Webp);
    let q = if fmt == Fmt::Png { 0 } else { r.q.unwrap_or(DEFAULT_QUALITY).clamp(1, 100) };
    Plan { w, h, crop, fmt, q }
}

fn scale_to_width(ow: f64, oh: f64, w: f64) -> (u32, u32, bool) {
    let w = w.min(ow);
    (w.round().max(1.0) as u32, ((oh * w / ow).round() as u32).max(1), false)
}

/// Detect a supported image by magic bytes: (mime, extension).
pub fn sniff(head: &[u8]) -> Option<(&'static str, &'static str)> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("image/jpeg", "jpg"))
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else {
        None
    }
}

/// Pixel size after applying the EXIF orientation (what ffmpeg's autorotate produces).
fn probe_dimensions(path: &Path) -> Result<(u32, u32), String> {
    use image::{metadata::Orientation, ImageDecoder};
    let reader = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut dec = reader.into_decoder().map_err(|e| e.to_string())?;
    let (w, h) = dec.dimensions();
    let o = dec.orientation().unwrap_or(Orientation::NoTransforms);
    Ok(match o {
        Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH => (h, w),
        _ => (w, h),
    })
}

impl AssetService {
    pub fn new(asset_dir: &Path, data_dir: &Path, default_width: u32) -> std::io::Result<Self> {
        let store = Store::open(asset_dir)?;
        if let Ok(rd) = std::fs::read_dir(store.staging_dir()) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let cache = data_dir.join("asset-cache");
        std::fs::create_dir_all(&cache)?;
        Ok(AssetService { store, cache, default_width, sem: Semaphore::new(MAX_TRANSFORMS) })
    }

    pub fn rel(a: &Asset) -> String {
        let ext = match a.mime.as_str() {
            "image/jpeg" => "jpg",
            "image/png" => "png",
            "image/webp" => "webp",
            "image/gif" => "gif",
            _ => "bin",
        };
        Store::rel_path(&a.hash, ext)
    }

    pub fn path(&self, a: &Asset) -> PathBuf {
        self.store.abs(&Self::rel(a))
    }

    fn cache_dir(&self, hash: &str) -> PathBuf {
        self.cache.join(&hash[0..2]).join(&hash[2..4])
    }

    /// Stream the body into staging while hashing, validate it is a decodable image, place it
    /// content-addressed and record it. An existing hash is returned as is (its `created_at`
    /// is refreshed, which restarts the GC grace period). The bool is "already existed".
    pub async fn upload(&self, db: &Db, body: axum::body::Body) -> AppResult<(Asset, bool)> {
        use http_body_util::BodyExt;
        use tokio::io::AsyncWriteExt;
        let staging = self.store.staging_dir().join(format!(
            "{}-{}.upload",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        let res: AppResult<(String, u64)> = async {
            let file = tokio::fs::File::create(&staging).await?;
            let mut out = tokio::io::BufWriter::with_capacity(1 << 20, file);
            let mut hasher = blake3::Hasher::new();
            let mut n = 0u64;
            let mut body = body;
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|e| AppError::bad(format!("upload interrupted: {e}")))?;
                if let Some(d) = frame.data_ref() {
                    n += d.len() as u64;
                    if n > MAX_UPLOAD {
                        return Err(AppError::TooLarge(format!("image exceeds {} MB", MAX_UPLOAD >> 20)));
                    }
                    hasher.update(d);
                    out.write_all(d).await?;
                }
            }
            out.flush().await?;
            Ok((hasher.finalize().to_hex().to_string(), n))
        }
        .await;
        let (hash, size) = match res {
            Ok(v) => v,
            Err(e) => {
                let _ = tokio::fs::remove_file(&staging).await;
                return Err(e);
            }
        };
        let store = self.store.clone();
        let db = db.clone();
        let st2 = staging.clone();
        let out = tokio::task::spawn_blocking(move || -> AppResult<(Asset, bool)> {
            use std::io::Read;
            let mut head = [0u8; 16];
            let n = std::fs::File::open(&st2)?.read(&mut head)?;
            let (mime, ext) = sniff(&head[..n]).ok_or_else(|| AppError::bad("body must be a JPEG, PNG, WebP or GIF image"))?;
            let (width, height) = probe_dimensions(&st2).map_err(|e| AppError::bad(format!("not a decodable image: {e}")))?;
            if (width as u64) * (height as u64) > MAX_PIXELS {
                return Err(AppError::bad("image has too many pixels"));
            }
            let rel = Store::rel_path(&hash, ext);
            let dst = store.abs(&rel);
            if !dst.exists() {
                store.ensure_parent(&rel)?;
                std::fs::set_permissions(&st2, std::fs::Permissions::from_mode(0o444))?;
                std::fs::rename(&st2, &dst)?;
            }
            let asset = Asset { hash, mime: mime.into(), width, height, size, created_at: 0 };
            let conn = db.conn()?;
            crate::db::Repo::new(&conn).upsert_asset(&asset)
        })
        .await?;
        let _ = tokio::fs::remove_file(&staging).await;
        out
    }

    /// Render (or fetch from the disk cache) the variant described by `plan`.
    /// Returns the file and whether it was a cache hit.
    pub async fn render(&self, a: &Asset, plan: &Plan) -> AppResult<(PathBuf, bool)> {
        let dir = self.cache_dir(&a.hash);
        let dst = dir.join(format!("{}_{}.{}", a.hash, plan.key(), plan.fmt.ext()));
        if dst.is_file() {
            return Ok((dst, true));
        }
        let _permit = self.sem.acquire().await.map_err(|_| AppError::Internal("asset renderer closed".into()))?;
        if dst.is_file() {
            return Ok((dst, true));
        }
        tokio::fs::create_dir_all(&dir).await?;
        let tmp = dir.join(format!(".tmp-{}-{}.{}", std::process::id(), plan.key(), plan.fmt.ext()));
        let vf = if plan.crop {
            format!("scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,crop={w}:{h}", w = plan.w, h = plan.h)
        } else {
            format!("scale={}:{}:flags=lanczos", plan.w, plan.h)
        };
        let mut cmd = tokio::process::Command::new("ffmpeg");
        cmd.args(["-v", "error", "-y", "-nostdin", "-i"]).arg(self.path(a)).args(["-frames:v", "1", "-vf", &vf]);
        match plan.fmt {
            Fmt::Webp => {
                cmd.args(["-c:v", "libwebp", "-quality", &plan.q.to_string(), "-f", "webp"]);
            }
            Fmt::Jpg => {
                // q 1..100 -> mjpeg qscale 31..1
                let qs = (31.0 - (plan.q as f64 - 1.0) * 30.0 / 99.0).round().clamp(1.0, 31.0) as u32;
                cmd.args(["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v", &qs.to_string(), "-f", "image2", "-update", "1"]);
            }
            Fmt::Png => {
                cmd.args(["-c:v", "png", "-f", "image2", "-update", "1"]);
            }
        }
        cmd.arg(&tmp).kill_on_drop(true).stdin(std::process::Stdio::null());
        let out = cmd.output().await.map_err(|e| AppError::Internal(format!("cannot run ffmpeg: {e}")))?;
        if !out.status.success() || !tmp.is_file() {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(AppError::Internal(format!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
        }
        tokio::fs::rename(&tmp, &dst).await?;
        Ok((dst, false))
    }

    /// Remove an asset's original and all of its cached variants.
    pub fn remove(&self, a: &Asset) {
        let _ = self.store.remove_blob(&Self::rel(a));
        if let Ok(rd) = std::fs::read_dir(self.cache_dir(&a.hash)) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&a.hash) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(w: Option<u32>, h: Option<u32>, fit: Option<Fit>) -> Request {
        Request { w, h, fit, ..Default::default() }
    }

    #[test]
    fn plans_never_upscale() {
        // width only: aspect kept, clamped to the original
        let p = plan(2000, 1000, &req(Some(500), None, None), 1280);
        assert_eq!((p.w, p.h, p.crop, p.fmt), (500, 250, false, Fmt::Webp));
        let p = plan(2000, 1000, &req(Some(9000), None, None), 1280);
        assert_eq!((p.w, p.h), (2000, 1000));
        // height only
        let p = plan(2000, 1000, &req(None, Some(100), None), 1280);
        assert_eq!((p.w, p.h), (200, 100));
        // default width when only other params are given
        let p = plan(4000, 2000, &Request { fmt: Some(Fmt::Jpg), ..Default::default() }, 1280);
        assert_eq!((p.w, p.h, p.fmt, p.q), (1280, 640, Fmt::Jpg, 80));
        let p = plan(800, 600, &Request { q: Some(50), ..Default::default() }, 1280);
        assert_eq!((p.w, p.h), (800, 600));
        // both: cover keeps the box (shrunk if the original is too small), contain fits inside
        let p = plan(2000, 1000, &req(Some(300), Some(300), Some(Fit::Cover)), 1280);
        assert_eq!((p.w, p.h, p.crop), (300, 300, true));
        let p = plan(2000, 1000, &req(Some(300), Some(300), Some(Fit::Contain)), 1280);
        assert_eq!((p.w, p.h, p.crop), (300, 150, false));
        let p = plan(400, 200, &req(Some(1000), Some(1000), None), 1280);
        assert_eq!((p.w, p.h, p.crop), (200, 200, true));
        let p = plan(400, 200, &req(Some(1000), Some(1000), Some(Fit::Contain)), 1280);
        assert_eq!((p.w, p.h), (400, 200));
        // png ignores q in the key
        let p = plan(400, 200, &Request { fmt: Some(Fmt::Png), q: Some(10), ..Default::default() }, 1280);
        assert_eq!(p.q, 0);
    }

    #[test]
    fn sniffs() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]).map(|s| s.1), Some("jpg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ").map(|s| s.1), Some("webp"));
        assert_eq!(sniff(b"GIF89a....").map(|s| s.1), Some("gif"));
        assert!(sniff(b"<html>").is_none());
    }
}
