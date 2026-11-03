use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VideoStream {
    pub codec: String,
    pub profile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub hdr: Option<String>,
    pub bit_depth: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AudioStream {
    pub index: u32,
    pub codec: String,
    pub channels: Option<u32>,
    pub language: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubtitleStream {
    pub index: u32,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub forced: bool,
}

pub const LOC_STORE: &str = "store";
pub const LOC_EXTERNAL: &str = "external";

/// One stored file (a version of an item). Its bytes live at `MEDIA_DIR/<store_path>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFile {
    pub id: String,
    pub item_id: String,
    /// BLAKE3 of the content (hex): the file's identity.
    pub hash: String,
    /// Relative to MEDIA_DIR (`<h[0..2]>/<h[2..4]>/<hash>.<ext>`), or to EXTERNAL_ROOT when `location` is external.
    pub store_path: String,
    /// File name it arrived with (ingest dir or upload).
    pub original_name: String,
    pub label: String,
    pub container: String,
    pub mime: String,
    pub size: u64,
    pub duration: Option<f64>,
    pub bitrate: Option<u64>,
    pub video: Option<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub subtitles: Vec<SubtitleStream>,
    pub added_at: i64,
    /// `store` (private blob under MEDIA_DIR) or `external` (referenced in place under EXTERNAL_ROOT).
    pub location: String,
    /// `full` (`hash` is the BLAKE3 of the content) or `pending` (`hash` is `fp:<fingerprint>`).
    pub hash_state: String,
    /// Quick fingerprint (external files only).
    pub fingerprint: Option<String>,
    /// mtime (ns) of an external file when last fingerprinted; 0 for store files.
    pub src_mtime: i64,
}

impl MediaFile {
    pub fn is_external(&self) -> bool {
        self.location == LOC_EXTERNAL
    }

    pub fn height(&self) -> u32 {
        self.video.as_ref().and_then(|v| v.height).unwrap_or(0)
    }
}

/// Files ordered by height desc, then oldest first.
pub fn sort_files(files: &mut [MediaFile]) {
    files.sort_by(|a, b| b.height().cmp(&a.height()).then(a.added_at.cmp(&b.added_at)).then(a.id.cmp(&b.id)));
}

pub const VIDEO_EXTS: &[&str] =
    &["mkv", "mp4", "m4v", "webm", "mov", "avi", "ts", "m2ts", "wmv", "flv", "mpg", "mpeg", "3gp", "ogv"];
pub const AUDIO_EXTS: &[&str] = &["mp3", "flac", "m4a", "aac", "ogg", "oga", "opus", "wav", "wma", "alac", "aiff", "ape"];

pub fn is_media_ext(ext: &str) -> bool {
    VIDEO_EXTS.contains(&ext) || AUDIO_EXTS.contains(&ext)
}

pub fn mime_for(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "ts" | "m2ts" => "video/mp2t",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "mpg" | "mpeg" => "video/mpeg",
        "3gp" => "video/3gpp",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "alac" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        "aiff" => "audio/aiff",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

pub fn ext_of(path: &std::path::Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}
