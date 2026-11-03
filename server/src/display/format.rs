//! Human formatting for display block text.

use crate::model::{FieldDefinition, FieldType};
use serde_json::Value;

pub fn human_size(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{v:.0} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn human_bitrate(bps: f64) -> String {
    if bps >= 1_000_000.0 {
        format!("{:.1} Mbps", bps / 1_000_000.0)
    } else {
        format!("{:.0} kbps", bps / 1000.0)
    }
}

pub fn human_duration(secs: f64) -> String {
    let s = secs.round().max(0.0) as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "8K", "4K", "1440p", "1080p", ... from a video height (and width for wide/cropped video).
pub fn resolution_label(width: u32, height: u32) -> String {
    let h = height.max(width * 9 / 16);
    match h {
        h if h >= 4000 => "8K".into(),
        h if h >= 2000 => "4K".into(),
        h if h >= 1400 => "1440p".into(),
        h if h >= 1000 => "1080p".into(),
        h if h >= 700 => "720p".into(),
        h if h >= 560 => "576p".into(),
        h if h >= 470 => "480p".into(),
        _ => format!("{height}p"),
    }
}

pub fn number(n: f64) -> String {
    if (n - n.round()).abs() < 1e-9 {
        format!("{}", n.round() as i64)
    } else {
        let s = format!("{n:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => n.as_f64().map(number),
        Value::Bool(b) => Some(if *b { "Yes" } else { "No" }.into()),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(scalar_text).collect();
            Some(parts.join(", ")).filter(|s| !s.is_empty())
        }
        Value::Object(_) => Some(v.to_string()),
    }
}

/// Format a raw value according to its definition (type, well-known key, template).
pub fn format_value(def: &FieldDefinition, value: &Value) -> Option<String> {
    let as_f64 = || value.as_f64().or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()));
    let text = match (def.key.as_str(), def.field_type) {
        ("size", _) if def.builtin => as_f64().map(human_size),
        ("bitrate", _) if def.builtin => as_f64().map(human_bitrate),
        ("fps", _) if def.builtin => as_f64().map(|f| format!("{}", f.round() as i64)),
        (_, FieldType::Duration) => as_f64().map(human_duration).or_else(|| scalar_text(value)),
        (_, FieldType::Rating) => as_f64().map(|r| number((r * 10.0).round() / 10.0)),
        (_, FieldType::Bool) => match value {
            Value::Bool(b) => Some(if *b { "Yes" } else { "No" }.into()),
            v => scalar_text(v),
        },
        _ => scalar_text(value),
    }?;
    Some(match &def.display.format {
        Some(tpl) if tpl.contains("{value}") => tpl.replace("{value}", &text),
        _ => text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats() {
        assert_eq!(human_size(551_123_344.0), "551.1 MB");
        assert_eq!(human_bitrate(24_600_000.0), "24.6 Mbps");
        assert_eq!(human_duration(178.4), "2:58");
        assert_eq!(human_duration(3723.0), "1:02:03");
        assert_eq!(resolution_label(7680, 4320), "8K");
        assert_eq!(resolution_label(3840, 1600), "4K");
        assert_eq!(resolution_label(1920, 1080), "1080p");
        assert_eq!(resolution_label(320, 240), "240p");
        assert_eq!(number(29.97), "29.97");
        assert_eq!(number(5.0), "5");
    }
}
