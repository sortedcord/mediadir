//! Still frames: one high-quality frame at a timestamp, for the paused-player view.
//! Decodes a single frame with ffmpeg (frame-accurate input seek); never transcodes media.

use crate::error::{AppError, AppResult};
use crate::model::{fnv1a, MediaFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    Jpg,
    Webp,
    /// Lossless PNG of the exact decoded frame.
    Png,
}

impl FrameFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "jpg" | "jpeg" => Some(FrameFormat::Jpg),
            "webp" => Some(FrameFormat::Webp),
            "png" | "raw" | "lossless" => Some(FrameFormat::Png),
            _ => None,
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            FrameFormat::Jpg => "jpg",
            FrameFormat::Webp => "webp",
            FrameFormat::Png => "png",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            FrameFormat::Jpg => "image/jpeg",
            FrameFormat::Webp => "image/webp",
            FrameFormat::Png => "image/png",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub pts: f64,
}

pub struct Frame {
    pub path: PathBuf,
    pub info: FrameInfo,
    pub cached: bool,
}

pub struct FrameService {
    dir: PathBuf,
    cache_bytes: u64,
    threads: u32,
    tonemap: bool,
    /// One extraction at a time.
    slot: Semaphore,
    /// Per-file request generation; a newer request obsoletes older queued/running ones.
    generations: Mutex<HashMap<String, Arc<AtomicU64>>>,
}

fn ffmpeg_has_tonemap() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-filters"])
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.contains(" zscale ") && s.contains(" tonemap ")
        })
        .unwrap_or(false)
}

impl FrameService {
    pub fn new(data_dir: &Path) -> Self {
        let env_num = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let tonemap = ffmpeg_has_tonemap();
        if !tonemap {
            eprintln!("frames: ffmpeg lacks zscale/tonemap; HDR frames will not be tone-mapped");
        }
        FrameService {
            dir: data_dir.join("frames"),
            cache_bytes: env_num("FRAME_CACHE_MB", 2048) * 1024 * 1024,
            threads: env_num("FRAME_THREADS", 4).clamp(1, 64) as u32,
            tonemap,
            slot: Semaphore::new(1),
            generations: Mutex::new(HashMap::new()),
        }
    }

    fn key(file_id: &str, t: f64, max_w: Option<u32>, fmt: FrameFormat) -> String {
        let ms = (t * 1000.0).round() as i64;
        fnv1a(&format!("{file_id}\u{0}{ms}\u{0}{}\u{0}{}", max_w.unwrap_or(0), fmt.ext()))
    }

    fn cached(&self, key: &str, fmt: FrameFormat) -> Option<Frame> {
        let path = self.dir.join(format!("{key}.{}", fmt.ext()));
        let meta = std::fs::read(self.dir.join(format!("{key}.json"))).ok()?;
        let info: FrameInfo = serde_json::from_slice(&meta).ok()?;
        if !path.is_file() {
            return None;
        }
        // LRU: touch on hit
        if let Ok(f) = std::fs::File::options().append(true).open(&path) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        Some(Frame { path, info, cached: true })
    }

    /// Get (or extract) a frame. `src` is the absolute media path.
    pub async fn frame(&self, file: &MediaFile, src: &Path, t: f64, max_w: Option<u32>, fmt: FrameFormat) -> AppResult<Frame> {
        let key = Self::key(&file.id, t, max_w, fmt);
        if let Some(f) = self.cached(&key, fmt) {
            return Ok(f);
        }
        let generation = {
            let mut g = self.generations.lock().unwrap();
            g.entry(file.id.clone()).or_insert_with(|| Arc::new(AtomicU64::new(0))).clone()
        };
        let mine = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let superseded = || AppError::Conflict("superseded by a newer frame request for this file".into());
        let _permit = self.slot.acquire().await.map_err(|_| AppError::Internal("frame queue closed".into()))?;
        if generation.load(Ordering::SeqCst) != mine {
            return Err(superseded());
        }
        if let Some(f) = self.cached(&key, fmt) {
            return Ok(f);
        }
        std::fs::create_dir_all(&self.dir)?;
        let out = self.dir.join(format!("{key}.tmp.{}", fmt.ext()));
        let hdr = file.video.as_ref().and_then(|v| v.hdr.as_ref()).is_some();
        let mut filters: Vec<String> = vec![];
        if hdr && self.tonemap {
            filters.push(
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv"
                    .into(),
            );
        }
        if let Some(w) = max_w {
            filters.push(format!("scale='min({w},iw)':-2:flags=lanczos"));
        }
        filters.push(match fmt {
            FrameFormat::Jpg => "format=yuvj420p".into(),
            FrameFormat::Webp => "format=yuv420p".into(),
            // Tone-mapped HDR is already 8-bit SDR; other >8-bit sources keep their depth (16-bit PNG).
            FrameFormat::Png => {
                let deep = !(hdr && self.tonemap) && file.video.as_ref().and_then(|v| v.bit_depth).unwrap_or(8) > 8;
                format!("scale=in_range=auto:out_range=full:flags=accurate_rnd+full_chroma_int+full_chroma_inp,format={}", if deep { "rgb48be" } else { "rgb24" })
            }
        });
        filters.push("showinfo".into());
        let threads = self.threads.to_string();
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostdin", "-y", "-threads", &threads, "-ss", &format!("{t:.3}"), "-copyts", "-i"])
            .arg(src)
            .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-frames:v", "1", "-filter_threads", &threads, "-vf", &filters.join(",")]);
        match fmt {
            FrameFormat::Jpg => cmd.args(["-c:v", "mjpeg", "-q:v", "2"]),
            FrameFormat::Webp => cmd.args(["-c:v", "libwebp", "-quality", "92", "-compression_level", "1"]),
            FrameFormat::Png => cmd.args(["-c:v", "png", "-compression_level", "1"]),
        };
        cmd.args(["-f", "image2"]).arg(&out).kill_on_drop(true);
        cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
        let child = cmd.spawn().map_err(|e| AppError::Internal(format!("ffmpeg: {e}")))?;
        let wait = child.wait_with_output();
        tokio::pin!(wait);
        let output = loop {
            tokio::select! {
                r = &mut wait => break r?,
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if generation.load(Ordering::SeqCst) != mine {
                        // dropping `wait` kills ffmpeg (kill_on_drop)
                        let _ = std::fs::remove_file(&out);
                        return Err(superseded());
                    }
                }
            }
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() || !out.is_file() {
            let _ = std::fs::remove_file(&out);
            let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
            return Err(AppError::Internal(format!("frame extraction failed: {tail}")));
        }
        let info = parse_showinfo(&stderr).unwrap_or(FrameInfo { width: 0, height: 0, pts: t });
        let path = self.dir.join(format!("{key}.{}", fmt.ext()));
        std::fs::rename(&out, &path)?;
        std::fs::write(self.dir.join(format!("{key}.json")), serde_json::to_vec(&info)?)?;
        let (dir, cap) = (self.dir.clone(), self.cache_bytes);
        tokio::task::spawn_blocking(move || evict(&dir, cap));
        Ok(Frame { path, info, cached: false })
    }
}

/// `[Parsed_showinfo_N ...] n:   0 pts:... pts_time:12.345 ... s:7680x4320 ...`
fn parse_showinfo(stderr: &str) -> Option<FrameInfo> {
    let line = stderr.lines().find(|l| l.contains("showinfo") && l.contains("pts_time:"))?;
    let field = |name: &str| -> Option<&str> {
        let i = line.find(name)? + name.len();
        line[i..].split_whitespace().next()
    };
    let pts: f64 = field("pts_time:")?.parse().ok()?;
    let (w, h) = field(" s:")?.split_once('x')?;
    Some(FrameInfo { width: w.parse().ok()?, height: h.parse().ok()?, pts })
}

/// Delete least-recently-used frames until the cache is below 90% of its cap.
fn evict(dir: &Path, cap: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let p = e.path();
            let modified = m.modified().ok()?;
            (p.extension()? != "json").then_some((modified, m.len(), p))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= cap {
        return;
    }
    files.sort();
    for (_, len, p) in files {
        if total <= cap * 9 / 10 {
            break;
        }
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("json"));
        total = total.saturating_sub(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_showinfo() {
        let s = "[Parsed_showinfo_2 @ 0x1] n:   0 pts:1234 pts_time:12.345  duration:1 fmt:yuvj420p cl:left sar:1/1 s:7680x4320 i:P";
        let i = parse_showinfo(s).unwrap();
        assert_eq!((i.width, i.height), (7680, 4320));
        assert!((i.pts - 12.345).abs() < 1e-9);
    }
}
