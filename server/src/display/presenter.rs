use super::format::{format_value, resolution_label};
use super::registry::FieldRegistry;
use crate::model::{
    is_empty_value, DisplayBlock, FieldDefinition, FieldType, Item, ItemKind, MediaFile, Person, PersonRef, Placement,
    Widget,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Everything the presenter needs to resolve field values for one item.
pub struct PresentContext<'a> {
    pub item: &'a Item,
    /// Ordered as served (height desc); the first is the "primary" file.
    pub files: &'a [MediaFile],
    pub parent: Option<&'a Item>,
    /// People referenced by `person`-typed values, by id.
    pub people: &'a HashMap<String, PersonRef>,
}

impl PresentContext<'_> {
    fn primary(&self) -> Option<&MediaFile> {
        self.files.first()
    }
}

pub struct Presenter<'r> {
    registry: &'r FieldRegistry,
}

impl<'r> Presenter<'r> {
    pub fn new(registry: &'r FieldRegistry) -> Self {
        Presenter { registry }
    }

    /// Blocks for cards (`card_badge` + `card_meta`), sorted by order.
    pub fn card(&self, ctx: &PresentContext) -> Vec<DisplayBlock> {
        self.blocks(ctx, |p| p.on_card())
    }

    /// All non-hidden blocks for the detail page, sorted by order.
    pub fn detail(&self, ctx: &PresentContext) -> Vec<DisplayBlock> {
        self.blocks(ctx, |p| p != Placement::Hidden)
    }

    fn blocks(&self, ctx: &PresentContext, want: impl Fn(Placement) -> bool) -> Vec<DisplayBlock> {
        let kind = ctx.item.kind.as_str();
        self.registry
            .all()
            .iter()
            .filter(|d| want(d.display.placement) && d.applies(kind))
            .filter_map(|d| {
                let v = Self::raw_value(d, ctx)?;
                Self::block(d, v, ctx.people)
            })
            .collect()
    }

    /// Display blocks for a person's custom fields (definitions that apply to `person`, or
    /// any custom definition that has a value on the person).
    pub fn person(&self, person: &Person, people: &HashMap<String, PersonRef>) -> Vec<DisplayBlock> {
        self.registry
            .all()
            .iter()
            .filter(|d| d.display.placement != Placement::Hidden)
            .filter(|d| (!d.builtin || d.applies_to.iter().any(|k| k == "person")) && !d.derived)
            .filter_map(|d| {
                let v = person.fields.get(&d.key).filter(|v| !is_empty_value(v))?.clone();
                Self::block(d, v, people)
            })
            .collect()
    }

    /// Ids referenced by person-typed field values (to prefetch before presenting).
    pub fn referenced_people<'a>(&self, values: impl Iterator<Item = (&'a String, &'a Value)>) -> Vec<String> {
        let mut out = vec![];
        for (k, v) in values {
            if self.registry.get(k).map(|d| d.field_type == FieldType::Person).unwrap_or(false) {
                match v {
                    Value::String(s) => out.push(s.clone()),
                    Value::Array(a) => out.extend(a.iter().filter_map(|x| x.as_str().map(String::from))),
                    _ => {}
                }
            }
        }
        out
    }

    fn block(def: &FieldDefinition, value: Value, people: &HashMap<String, PersonRef>) -> Option<DisplayBlock> {
        if is_empty_value(&value) {
            return None;
        }
        let is_people = def.field_type == FieldType::Person || def.display.widget == Widget::People;
        let (text, people_out) = if is_people {
            let ids: Vec<String> = match &value {
                Value::String(s) => vec![s.clone()],
                Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                _ => vec![],
            };
            let refs: Vec<PersonRef> = ids.iter().filter_map(|id| people.get(id).cloned()).collect();
            if refs.is_empty() {
                return None;
            }
            let names = refs.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ");
            (names, Some(refs))
        } else if def.field_type == FieldType::Image {
            (String::new(), None)
        } else {
            (format_value(def, &value)?, None)
        };
        Some(DisplayBlock {
            key: def.key.clone(),
            label: def.label.clone(),
            field_type: def.field_type,
            widget: def.display.widget,
            placement: def.display.placement,
            order: def.display.order,
            color: def.display.color.clone(),
            icon: def.display.icon.clone(),
            text,
            value,
            people: people_out,
        })
    }

    /// Raw value of a field for an item: derived from files/structure, core columns,
    /// user values, then classifier values.
    pub fn raw_value(def: &FieldDefinition, ctx: &PresentContext) -> Option<Value> {
        let item = ctx.item;
        let primary = ctx.primary();
        let video = primary.and_then(|f| f.video.as_ref());
        if def.builtin {
            let v = match def.key.as_str() {
                "resolution" => video.and_then(|v| match (v.width, v.height) {
                    (Some(w), Some(h)) if h > 0 => Some(json!(resolution_label(w, h))),
                    _ => None,
                }),
                "fps" => video.and_then(|v| v.fps).filter(|f| *f > 0.0).map(|f| json!(f)),
                "hdr" => video.and_then(|v| v.hdr.clone()).map(Value::String),
                "video_codec" => video.map(|v| json!(v.codec.to_uppercase())),
                "audio_codec" => primary.and_then(|f| f.audio.first()).map(|a| json!(a.codec.to_uppercase())),
                "container" => primary.map(|f| json!(f.container.to_uppercase())),
                "size" => primary.map(|f| json!(f.size)),
                "bitrate" => primary.and_then(|f| f.bitrate).map(|b| json!(b)),
                "duration" => item.duration.or_else(|| primary.and_then(|f| f.duration)).filter(|d| *d > 0.0).map(|d| json!(d)),
                "year" => item.year.map(|y| json!(y)),
                "genres" => Some(json!(item.genres)),
                "overview" => item.overview.clone().map(Value::String),
                "album" => item.field_value("album").cloned().or_else(|| match (item.kind, ctx.parent) {
                    (ItemKind::Track, Some(p)) => Some(json!(p.title)),
                    _ => None,
                }),
                "track_number" => item.field_value("track_number").cloned().or_else(|| match item.kind {
                    ItemKind::Track => item.index.map(|i| json!(i)),
                    _ => None,
                }),
                "language" => item.field_value("language").cloned().or_else(|| {
                    primary.and_then(|f| f.audio.first()).and_then(|a| a.language.clone()).filter(|l| l != "und").map(Value::String)
                }),
                key => item.field_value(key).cloned(),
            };
            return v.filter(|v| !is_empty_value(v));
        }
        item.field_value(&def.key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioStream, VideoStream};

    fn file(h: u32) -> MediaFile {
        MediaFile {
            id: format!("f{h}"),
            item_id: "i".into(),
            hash: format!("{h:064}"),
            store_path: "x.mkv".into(),
            original_name: "x.mkv".into(),
            label: String::new(),
            container: "mkv".into(),
            mime: "video/x-matroska".into(),
            size: 551_123_344,
            duration: Some(178.4),
            bitrate: Some(24_600_000),
            video: Some(VideoStream {
                codec: "av1".into(),
                width: Some(h * 16 / 9),
                height: Some(h),
                fps: Some(29.97),
                ..Default::default()
            }),
            audio: vec![AudioStream { index: 1, codec: "opus".into(), channels: Some(2), language: Some("eng".into()), title: None }],
            subtitles: vec![],
            added_at: 0,
        }
    }

    fn registry() -> FieldRegistry {
        let custom = FieldDefinition {
            key: "mood".into(),
            label: "Mood".into(),
            field_type: FieldType::Text,
            builtin: false,
            derived: false,
            applies_to: vec![],
            display: crate::model::DisplaySpec {
                placement: Placement::CardMeta,
                widget: Widget::Text,
                order: 1,
                format: Some("Mood: {value}".into()),
                color: None,
                icon: None,
            },
        };
        let dancers = FieldDefinition {
            key: "dancers".into(),
            label: "Dancers".into(),
            field_type: FieldType::Person,
            builtin: false,
            derived: false,
            applies_to: vec!["music_video".into()],
            display: crate::model::DisplaySpec {
                placement: Placement::Section,
                widget: Widget::People,
                order: 200,
                format: None,
                color: None,
                icon: None,
            },
        };
        FieldRegistry::from_parts(vec![custom, dancers])
    }

    #[test]
    fn card_blocks_use_primary_file_and_sort_by_order() {
        let reg = registry();
        let mut item = Item::new("i".into(), ItemKind::MusicVideo, "Dilbar".into());
        item.fields.insert("mood".into(), json!("Hype"));
        let files = vec![file(4320), file(1080)];
        let people = HashMap::new();
        let ctx = PresentContext { item: &item, files: &files, parent: None, people: &people };
        let card = Presenter::new(&reg).card(&ctx);
        let keys: Vec<&str> = card.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, vec!["mood", "resolution", "fps", "video_codec"]);
        assert_eq!(card[0].text, "Mood: Hype");
        assert_eq!(card[1].text, "8K");
        assert_eq!(card[2].text, "30fps");
        assert_eq!(card[2].value, json!(29.97));
        assert!(card.windows(2).all(|w| w[0].order <= w[1].order));
    }

    #[test]
    fn detail_includes_info_omits_empty_and_resolves_people() {
        let reg = registry();
        let mut item = Item::new("i".into(), ItemKind::MusicVideo, "Dilbar".into());
        item.year = Some(2018);
        item.fields.insert("dancers".into(), json!(["p1", "missing"]));
        let files = vec![file(4320)];
        let mut people = HashMap::new();
        people.insert("p1".to_string(), PersonRef { id: "p1".into(), name: "Nora Fatehi".into(), images: Default::default() });
        let ctx = PresentContext { item: &item, files: &files, parent: None, people: &people };
        let blocks = Presenter::new(&reg).detail(&ctx);
        let get = |k: &str| blocks.iter().find(|b| b.key == k);
        assert_eq!(get("year").unwrap().text, "2018");
        assert_eq!(get("size").unwrap().text, "551.1 MB");
        assert_eq!(get("bitrate").unwrap().text, "24.6 Mbps");
        assert_eq!(get("duration").unwrap().text, "2:58");
        assert_eq!(get("language").unwrap().text, "eng");
        assert!(get("genres").is_none(), "empty genres omitted");
        assert!(get("overview").is_none());
        let d = get("dancers").unwrap();
        assert_eq!(d.people.as_ref().unwrap().len(), 1);
        assert_eq!(d.text, "Nora Fatehi");
    }

    #[test]
    fn track_album_falls_back_to_parent() {
        let reg = registry();
        let mut item = Item::new("t".into(), ItemKind::Track, "Song".into());
        item.index = Some(3);
        let parent = Item::new("a".into(), ItemKind::Album, "The Album".into());
        let people = HashMap::new();
        let ctx = PresentContext { item: &item, files: &[], parent: Some(&parent), people: &people };
        let blocks = Presenter::new(&reg).detail(&ctx);
        assert_eq!(blocks.iter().find(|b| b.key == "album").unwrap().text, "The Album");
        assert_eq!(blocks.iter().find(|b| b.key == "track_number").unwrap().text, "3");
        assert!(blocks.iter().all(|b| b.key != "resolution"));
    }
}
