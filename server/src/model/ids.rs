//! Stable 16-hex FNV-1a identifiers derived from logical keys.

pub fn fnv1a(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Lowercase, whitespace-collapsed form used for matching names.
pub fn normalize_name(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Playable item created for the first file with this content hash.
pub fn leaf_item_id(hash: &str) -> String {
    fnv1a(&format!("item\u{0}{hash}"))
}

pub fn media_file_id(hash: &str) -> String {
    fnv1a(&format!("file\u{0}{hash}"))
}

/// Container item (show, season, artist) keyed by a logical key.
pub fn container_id(kind: &str, key: &str) -> String {
    fnv1a(&format!("container\u{0}{kind}\u{0}{}", normalize_name(key)))
}

pub fn person_id(name: &str) -> String {
    fnv1a(&format!("person\u{0}{}", normalize_name(name)))
}

pub fn credit_id(item_id: &str, person_id: &str, role: &str, manual: bool) -> String {
    let src = if manual { "manual" } else { "scan" };
    fnv1a(&format!("credit\u{0}{item_id}\u{0}{person_id}\u{0}{role}\u{0}{src}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_are_stable_and_normalized() {
        assert_eq!(fnv1a("").len(), 16);
        assert_eq!(person_id("Nora  Fatehi"), person_id("nora fatehi"));
        assert_ne!(leaf_item_id("ab"), media_file_id("ab"));
        assert_eq!(container_id("show", "Test  Show"), container_id("show", "test show"));
    }
}
