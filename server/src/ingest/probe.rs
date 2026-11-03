//! Technical probing with ffprobe: codecs, resolution, fps, duration, tracks. Tags are not read
//! (the file name is the only metadata source). Never transcodes.

use crate::model::{AudioStream, SubtitleStream, VideoStream};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Default)]
pub struct ProbeResult {
    pub duration: Option<f64>,
    pub bitrate: Option<u64>,
    /// First real video stream (attached cover pictures are not video).
    pub video: Option<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub subtitles: Vec<SubtitleStream>,
}

impl ProbeResult {
    pub fn is_media(&self) -> bool {
        self.video.is_some() || !self.audio.is_empty()
    }
}

fn parse_rate(s: &str) -> Option<f64> {
    let (a, b) = s.split_once('/')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    if a == 0.0 || b == 0.0 {
        None
    } else {
        Some((a / b * 1000.0).round() / 1000.0)
    }
}

fn str_of(v: &Value) -> Option<String> {
    v.as_str().map(|s| s.to_string()).filter(|s| !s.is_empty())
}

fn num_of(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// `None` when ffprobe cannot open the file.
pub fn probe(path: &Path) -> Option<ProbeResult> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(parse_probe_json(&v))
}

pub fn parse_probe_json(v: &Value) -> ProbeResult {
    let fmt = &v["format"];
    let mut r = ProbeResult {
        duration: num_of(&fmt["duration"]).filter(|d| *d > 0.0),
        bitrate: num_of(&fmt["bit_rate"]).map(|b| b as u64),
        ..Default::default()
    };
    let mut stream_duration = None;
    for s in v["streams"].as_array().cloned().unwrap_or_default() {
        let index = s["index"].as_u64().unwrap_or(0) as u32;
        let codec = str_of(&s["codec_name"]).unwrap_or_else(|| "unknown".into());
        let lang = str_of(&s["tags"]["language"]);
        let title = str_of(&s["tags"]["title"]);
        match s["codec_type"].as_str().unwrap_or("") {
            "video" if s["disposition"]["attached_pic"].as_i64() == Some(1) => {}
            "video" if r.video.is_none() => {
                let hdr = match s["color_transfer"].as_str() {
                    Some("smpte2084") => Some("HDR10".to_string()),
                    Some("arib-std-b67") => Some("HLG".to_string()),
                    _ => None,
                };
                let bit_depth = str_of(&s["bits_per_raw_sample"]).and_then(|b| b.parse().ok()).or_else(|| {
                    let pf = s["pix_fmt"].as_str().unwrap_or("");
                    if pf.contains("10") {
                        Some(10)
                    } else if pf.contains("12") {
                        Some(12)
                    } else if pf.is_empty() {
                        None
                    } else {
                        Some(8)
                    }
                });
                r.video = Some(VideoStream {
                    codec,
                    profile: str_of(&s["profile"]),
                    width: s["width"].as_u64().map(|x| x as u32),
                    height: s["height"].as_u64().map(|x| x as u32),
                    fps: s["avg_frame_rate"].as_str().and_then(parse_rate).or_else(|| s["r_frame_rate"].as_str().and_then(parse_rate)),
                    hdr,
                    bit_depth,
                });
                stream_duration = stream_duration.or_else(|| s["tags"]["DURATION"].as_str().and_then(parse_hms));
            }
            "audio" => {
                stream_duration = stream_duration.or_else(|| s["tags"]["DURATION"].as_str().and_then(parse_hms));
                r.audio.push(AudioStream {
                    index,
                    codec,
                    channels: s["channels"].as_u64().map(|x| x as u32),
                    language: lang,
                    title,
                });
            }
            "subtitle" => r.subtitles.push(SubtitleStream {
                index,
                codec,
                language: lang,
                title,
                forced: s["disposition"]["forced"].as_i64() == Some(1),
            }),
            _ => {}
        }
    }
    // matroska may only store per-stream DURATION values
    r.duration = r.duration.or(stream_duration);
    r
}

/// "00:02:58.400000000" -> 178.4
fn parse_hms(s: &str) -> Option<f64> {
    let parts: Vec<f64> = s.split(':').map(|p| p.parse().ok()).collect::<Option<Vec<_>>>()?;
    Some(parts.iter().fold(0.0, |acc, p| acc * 60.0 + p))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_ffprobe_json() {
        let v: Value = serde_json::from_str(
            r#"{"format":{"duration":"178.4","bit_rate":"24600000","tags":{"ARTIST":"Nora"}},
               "streams":[{"index":0,"codec_type":"video","codec_name":"av1","width":7680,"height":4320,"avg_frame_rate":"30000/1001","pix_fmt":"yuv420p"},
                          {"index":1,"codec_type":"audio","codec_name":"opus","channels":2,"tags":{"language":"eng"}},
                          {"index":2,"codec_type":"subtitle","codec_name":"subrip","disposition":{"forced":0}},
                          {"index":3,"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}}]}"#,
        )
        .unwrap();
        let r = parse_probe_json(&v);
        assert_eq!(r.duration, Some(178.4));
        let vid = r.video.clone().unwrap();
        assert_eq!(vid.fps, Some(29.97));
        assert_eq!(vid.bit_depth, Some(8));
        assert_eq!(r.audio[0].language.as_deref(), Some("eng"));
        assert_eq!(r.subtitles.len(), 1);
        assert!(r.is_media());
        assert_eq!(parse_hms("00:02:58.4"), Some(178.4));
        // audio file with a cover picture is audio-only
        let v: Value = serde_json::from_str(
            r#"{"format":{},"streams":[{"index":0,"codec_type":"audio","codec_name":"mp3","tags":{"DURATION":"00:00:03.5"}},
                          {"index":1,"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}}]}"#,
        )
        .unwrap();
        let r = parse_probe_json(&v);
        assert!(r.video.is_none() && r.audio.len() == 1);
        assert_eq!(r.duration, Some(3.5));
    }
}
