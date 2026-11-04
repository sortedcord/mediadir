use crate::db::Repo;
use crate::error::AppResult;
use crate::model::{DisplaySpec, FieldDefinition, FieldType, Placement, Widget};

const VIDEO: &[&str] = &["movie", "episode", "music_video", "video"];
const PLAYABLE: &[&str] = &["movie", "episode", "music_video", "video", "track"];
const ALL: &[&str] = &[];

/// Builtin keys whose values are computed from probing/structure.
pub const DERIVED_KEYS: &[&str] =
    &["resolution", "fps", "hdr", "video_codec", "audio_codec", "container", "size", "bitrate", "duration"];


#[allow(clippy::too_many_arguments)]
fn def(
    key: &str,
    label: &str,
    field_type: FieldType,
    applies: &[&str],
    placement: Placement,
    widget: Widget,
    order: i64,
    format: Option<&str>,
    color: Option<&str>,
    icon: Option<&str>,
) -> FieldDefinition {
    FieldDefinition {
        key: key.into(),
        label: label.into(),
        field_type,
        builtin: true,
        derived: DERIVED_KEYS.contains(&key),
        applies_to: applies.iter().map(|s| s.to_string()).collect(),
        display: DisplaySpec {
            placement,
            widget,
            order,
            format: format.map(Into::into),
            color: color.map(Into::into),
            icon: icon.map(Into::into),
        },
    }
}

/// Field definitions known to the server, merged with stored overrides and custom fields.
#[derive(Debug, Clone)]
pub struct FieldRegistry {
    defs: Vec<FieldDefinition>,
}

impl FieldRegistry {
    pub fn builtin_definitions() -> Vec<FieldDefinition> {
        use FieldType as T;
        use Placement as P;
        use Widget as W;
        vec![
            def("year", "Year", T::Number, ALL, P::Header, W::Chip, 5, None, None, Some("calendar_today")),
            def("genres", "Genres", T::Tags, ALL, P::Header, W::Tags, 6, None, None, None),
            def("rating", "Rating", T::Rating, ALL, P::Header, W::Rating, 7, None, Some("#FFC107"), Some("star")),
            def("content_rating", "Content rating", T::Text, &["movie", "show", "season", "episode"], P::Header, W::Chip, 8, None, None, None),
            def("resolution", "Resolution", T::Text, VIDEO, P::CardBadge, W::Badge, 10, None, Some("#3D7BFF"), Some("hd")),
            def("hdr", "HDR", T::Text, VIDEO, P::CardBadge, W::Badge, 15, None, Some("#F5A623"), Some("hdr_on")),
            def("fps", "Frame rate", T::Number, VIDEO, P::CardBadge, W::Badge, 20, Some("{value}fps"), Some("#FF4D6D"), None),
            def("video_codec", "Video codec", T::Text, VIDEO, P::CardMeta, W::Text, 30, None, None, Some("movie")),
            def("audio_codec", "Audio codec", T::Text, PLAYABLE, P::Info, W::Text, 40, None, None, Some("graphic_eq")),
            def("container", "Container", T::Text, PLAYABLE, P::Info, W::Text, 45, None, None, None),
            def("size", "File size", T::Number, PLAYABLE, P::Info, W::Text, 50, None, None, Some("storage")),
            def("bitrate", "Bitrate", T::Number, PLAYABLE, P::Info, W::Text, 55, None, None, Some("speed")),
            def("duration", "Duration", T::Duration, ALL, P::Info, W::Stat, 60, None, None, Some("schedule")),
            def("studio", "Studio", T::Text, &["movie", "show"], P::Info, W::Text, 65, None, None, Some("business")),
            def("album", "Album", T::Text, &["track", "music_video"], P::Info, W::Text, 70, None, None, Some("album")),
            def("track_number", "Track", T::Number, &["track"], P::Info, W::Text, 71, None, None, None),
            def("language", "Language", T::Text, ALL, P::Info, W::Text, 75, None, None, Some("language")),
            def("tagline", "Tagline", T::Text, &["movie", "show"], P::Section, W::Text, 90, None, None, None),
            def("overview", "Overview", T::Text, ALL, P::Section, W::Markdown, 100, None, None, None),
        ]
    }

    pub fn is_builtin(key: &str) -> bool {
        Self::builtin_definitions().iter().any(|d| d.key == key)
    }

    pub fn from_parts(stored: Vec<FieldDefinition>) -> Self {
        let mut defs = Self::builtin_definitions();
        for s in stored {
            if let Some(b) = defs.iter_mut().find(|d| d.key == s.key) {
                // builtins: label/display/applies_to are overridable
                b.label = s.label;
                b.applies_to = s.applies_to;
                b.display = s.display;
            } else if !s.builtin {
                defs.push(FieldDefinition { builtin: false, derived: false, ..s });
            }
        }
        defs.sort_by(|a, b| a.display.order.cmp(&b.display.order).then(a.key.cmp(&b.key)));
        FieldRegistry { defs }
    }

    pub fn load(repo: &Repo) -> AppResult<Self> {
        Ok(Self::from_parts(repo.stored_fields()?.into_iter().map(|s| s.def).collect()))
    }

    pub fn all(&self) -> &[FieldDefinition] {
        &self.defs
    }

    pub fn get(&self, key: &str) -> Option<&FieldDefinition> {
        self.defs.iter().find(|d| d.key == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_applies_to_override_is_honoured() {
        let mut o = FieldRegistry::builtin_definitions().into_iter().find(|d| d.key == "studio").unwrap();
        assert!(o.applies("movie") && !o.applies("episode"));
        o.applies_to = vec!["episode".into()];
        let reg = FieldRegistry::from_parts(vec![o]);
        let d = reg.get("studio").unwrap();
        assert!(d.builtin && d.applies("episode") && !d.applies("movie"));
        o = d.clone();
        o.applies_to = vec![];
        let reg = FieldRegistry::from_parts(vec![o]);
        assert!(reg.get("studio").unwrap().applies("movie"), "[] = all kinds");
    }
}
