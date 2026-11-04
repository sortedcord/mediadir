//! Turns a classifier [`Mapping`] plus a stored, probed file into items and a MediaFile.
//! Files whose mapping has the same normalized identity (movie title+year, show+season+episode)
//! become additional versions of one item.

use super::classify::{EpisodeMapping, Mapping};
use super::probe::ProbeResult;
use crate::db::{CreditDraft, Repo};
use crate::display::FieldRegistry;
use crate::error::AppResult;
use crate::model::{
    container_id, leaf_item_id, media_file_id, mime_for, normalize_name, DisplaySpec, FieldDefinition, FieldType, Item,
    ItemKind, MediaFile, Placement, Role, VideoStream, Widget,
};
use serde_json::json;

/// A file that has been hashed, probed and placed in the store.
pub struct StoredFile {
    pub hash: String,
    pub store_path: String,
    pub original_name: String,
    pub ext: String,
    pub size: u64,
    pub probe: ProbeResult,
    /// `store` or `external` (the store entry is a symlink).
    pub location: String,
    /// `full` or `pending`.
    pub hash_state: String,
    pub fingerprint: Option<String>,
    pub src_mtime: i64,
}

#[derive(Debug, Clone)]
pub struct Attached {
    pub item_id: String,
    pub file_id: String,
    /// The file joined an existing item as another version.
    pub version_of_existing: bool,
}

pub fn file_label(video: Option<&VideoStream>, audio_codec: Option<&str>, bitrate: Option<u64>) -> String {
    match video {
        Some(v) => match v.height {
            Some(h) => format!("{h}p · {}", v.codec.to_uppercase()),
            None => v.codec.to_uppercase(),
        },
        None => match (audio_codec, bitrate) {
            (Some(c), Some(b)) => format!("{} · {} kbps", c.to_uppercase(), b / 1000),
            (Some(c), None) => c.to_uppercase(),
            _ => "Original".into(),
        },
    }
}

/// Normalized identity used to group versions; `None` = every file is its own item.
pub fn group_key(m: &Mapping) -> Option<String> {
    match m {
        Mapping::Movie { title, year } => Some(format!("movie\u{0}{}\u{0}{year}", normalize_name(title))),
        Mapping::Episode(e) => Some(format!("episode\u{0}{}\u{0}{}\u{0}{}", normalize_name(&e.show), e.season, e.episode)),
        _ => None,
    }
}

fn season_title(n: i64) -> String {
    match n {
        0 => "Specials".into(),
        n => format!("Season {n}"),
    }
}

/// Insert the file (and any missing items) in one transaction.
pub fn attach(repo: &Repo, mapping: &Mapping, f: &StoredFile, now: i64) -> AppResult<Attached> {
    repo.tx(|r| {
        let key = group_key(mapping);
        let existing = match &key {
            Some(k) => r.find_by_group_key(k)?,
            None => None,
        };
        let version_of_existing = existing.is_some();
        let item = match existing {
            Some(item) => item,
            None => {
                let item = new_item(r, mapping, &f.hash, key, now)?;
                r.save_item(&item)?;
                ensure_auto_fields(r, &item)?;
                item
            }
        };
        let mut id = media_file_id(&f.hash);
        if r.find_file(&id)?.is_some() {
            // ids derive from the hash at insert time and outlive it (fp: -> full hash), so a
            // different file can land on a taken id; never collide
            id = crate::model::fnv1a(&format!("file\u{0}{}\u{0}{now}", f.hash));
        }
        let file = MediaFile {
            id,
            item_id: item.id.clone(),
            hash: f.hash.clone(),
            store_path: f.store_path.clone(),
            original_name: f.original_name.clone(),
            label: file_label(f.probe.video.as_ref(), f.probe.audio.first().map(|a| a.codec.as_str()), f.probe.bitrate),
            container: f.ext.clone(),
            mime: mime_for(&f.ext).to_string(),
            size: f.size,
            duration: f.probe.duration,
            bitrate: f.probe.bitrate,
            video: f.probe.video.clone(),
            audio: f.probe.audio.clone(),
            subtitles: f.probe.subtitles.clone(),
            added_at: now,
            location: f.location.clone(),
            hash_state: f.hash_state.clone(),
            fingerprint: f.fingerprint.clone(),
            src_mtime: f.src_mtime,
        };
        r.insert_file(&file)?;
        Ok(Attached { item_id: item.id, file_id: file.id, version_of_existing })
    })
}

fn new_item(r: &Repo, m: &Mapping, hash: &str, group_key: Option<String>, now: i64) -> AppResult<Item> {
    let mut id = leaf_item_id(hash);
    if r.find_item(&id)?.is_some() {
        // the item first made for this content still exists (its file was deleted and re-added)
        id = crate::model::fnv1a(&format!("item\u{0}{hash}\u{0}{now}"));
    }
    let mut item = match m {
        Mapping::Movie { title, year } => {
            let mut i = Item::new(id, ItemKind::Movie, title.clone());
            i.year = Some(*year);
            i
        }
        Mapping::Episode(e) => episode(r, id, e, now)?,
        Mapping::Track { artist, title, number, disc } => {
            let mut i = Item::new(id, ItemKind::Track, title.clone());
            i.index = *number;
            i.parent_index = *disc;
            if let Some(a) = artist {
                i.parent_id = Some(ensure_artist(r, a, now)?);
                i.subtitle = Some(a.clone());
            }
            i
        }
        Mapping::MusicVideo { title, subtitle, youtube_id, .. } => {
            let mut i = Item::new(id, ItemKind::MusicVideo, title.clone());
            i.subtitle = subtitle.clone();
            if let Some(y) = youtube_id {
                i.extra.insert("youtube".into(), json!(format!("https://youtu.be/{y}")));
            }
            i
        }
        Mapping::Video { title, youtube_id } => {
            let mut i = Item::new(id, ItemKind::Video, title.clone());
            if let Some(y) = youtube_id {
                i.extra.insert("youtube".into(), json!(format!("https://youtu.be/{y}")));
            }
            i
        }
    };
    item.group_key = group_key;
    item.added_at = now;
    Ok(item)
}

fn episode(r: &Repo, id: String, e: &EpisodeMapping, now: i64) -> AppResult<Item> {
    let show_key = normalize_name(&e.show);
    let show_id = container_id("show", &show_key);
    if r.find_item(&show_id)?.is_none() {
        let mut show = Item::new(show_id.clone(), ItemKind::Show, e.show.clone());
        show.year = e.show_year;
        show.added_at = now;
        r.save_item(&show)?;
    }
    let season_id = container_id("season", &format!("{show_key}/{}", e.season));
    if r.find_item(&season_id)?.is_none() {
        let mut season = Item::new(season_id.clone(), ItemKind::Season, season_title(e.season));
        season.parent_id = Some(show_id);
        season.index = Some(e.season);
        season.subtitle = Some(e.show.clone());
        season.added_at = now;
        r.save_item(&season)?;
    }
    let title = e.title.clone().unwrap_or_else(|| format!("Episode {}", e.episode));
    let mut ep = Item::new(id, ItemKind::Episode, title);
    ep.parent_id = Some(season_id);
    ep.index = Some(e.episode);
    ep.index_end = e.episode_end;
    ep.parent_index = Some(e.season);
    ep.subtitle = Some(match e.episode_end {
        Some(end) => format!("S{:02}E{:02}-E{end:02}", e.season, e.episode),
        None => format!("S{:02}E{:02}", e.season, e.episode),
    });
    Ok(ep)
}

fn ensure_artist(r: &Repo, name: &str, now: i64) -> AppResult<String> {
    let id = container_id("artist", name);
    if r.find_item(&id)?.is_none() {
        let mut artist = Item::new(id.clone(), ItemKind::Artist, name.to_string());
        artist.added_at = now;
        r.save_item(&artist)?;
        let credit = CreditDraft { name: name.to_string(), role: Role::new("artist"), character: None, order: 0, image: None };
        r.replace_scan_credits(&id, &[credit])?;
    }
    Ok(id)
}

/// Classifier values without a field definition (e.g. `youtube`) get an automatic custom one.
fn ensure_auto_fields(r: &Repo, item: &Item) -> AppResult<()> {
    let known: Vec<String> = r.stored_fields()?.into_iter().map(|s| s.def.key).collect();
    for k in item.extra.keys() {
        if known.contains(k) || FieldRegistry::is_builtin(k) {
            continue;
        }
        let (label, field_type, widget, icon) = match k.as_str() {
            "youtube" => ("YouTube".to_string(), FieldType::Url, Widget::Link, Some("link".to_string())),
            _ => (k.replace('_', " "), FieldType::Text, Widget::Text, None),
        };
        let def = FieldDefinition {
            key: k.clone(),
            label,
            field_type,
            builtin: false,
            derived: false,
            applies_to: vec![],
            display: DisplaySpec { placement: Placement::Info, widget, order: 500, format: None, color: None, icon },
        };
        r.save_field_def(&def, true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_group_by_normalized_name() {
        let a = Mapping::Movie { title: "Versions Test".into(), year: 2015 };
        let b = Mapping::Movie { title: "versions  test".into(), year: 2015 };
        let c = Mapping::Movie { title: "Versions Test".into(), year: 2016 };
        assert_eq!(group_key(&a), group_key(&b));
        assert_ne!(group_key(&a), group_key(&c));
        let e = |s: i64, n: i64| {
            Mapping::Episode(EpisodeMapping { show: "Show".into(), show_year: None, season: s, episode: n, episode_end: None, title: None })
        };
        assert_eq!(group_key(&e(1, 2)), group_key(&e(1, 2)));
        assert_ne!(group_key(&e(1, 2)), group_key(&e(2, 2)));
        assert_eq!(group_key(&Mapping::Video { title: "x".into(), youtube_id: None }), None);
    }

    #[test]
    fn labels() {
        let v = VideoStream { codec: "av1".into(), height: Some(4320), ..Default::default() };
        assert_eq!(file_label(Some(&v), None, None), "4320p · AV1");
        assert_eq!(file_label(None, Some("flac"), Some(900_000)), "FLAC · 900 kbps");
    }
}
