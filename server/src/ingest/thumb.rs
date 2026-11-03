//! One generated thumbnail per video item (a single decoded frame; media is never transcoded).

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn thumb_path(data_dir: &Path, item_id: &str) -> PathBuf {
    data_dir.join("thumbs").join(format!("{item_id}.jpg"))
}

/// Grab one frame at ~30% of the duration (falling back to the first frame).
pub fn make_video_thumb(data_dir: &Path, item_id: &str, src: &Path, duration: Option<f64>) -> Option<String> {
    let dst = thumb_path(data_dir, item_id);
    if dst.is_file() {
        return Some(dst.to_string_lossy().to_string());
    }
    std::fs::create_dir_all(dst.parent()?).ok()?;
    let ts = duration.filter(|d| *d > 0.0).map(|d| d * 0.3).unwrap_or(5.0);
    let attempt = |ss: f64| {
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-ss", &format!("{ss:.2}"), "-i"])
            .arg(src)
            .args(["-frames:v", "1", "-vf", "scale=640:-2", "-q:v", "4"])
            .arg(&dst)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            && dst.is_file()
    };
    (attempt(ts) || attempt(0.0)).then(|| dst.to_string_lossy().to_string())
}
