//! Name → item mapping. The file name is the only metadata source (ffprobe only says whether the
//! file has video). Smarter detection (lookups, AI...) plugs in as another [`Classifier`].

use super::filename::{
    clean, clean_video_title, humanize, parse_artist_title, parse_episode, parse_movie, parse_mv_artist_title, parse_pipe,
    parse_track, strip_youtube_id, EpisodePattern,
};

/// What a classifier may look at.
#[derive(Debug, Clone)]
pub struct ClassifyInput<'a> {
    /// Original file name, with extension.
    pub file_name: &'a str,
    pub has_video: bool,
    pub has_audio: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeMapping {
    pub show: String,
    pub show_year: Option<i64>,
    pub season: i64,
    pub episode: i64,
    /// Last episode of a multi-episode file.
    pub episode_end: Option<i64>,
    pub title: Option<String>,
}

/// Where a file belongs. Every field stays editable afterwards.
#[derive(Debug, Clone, PartialEq)]
pub enum Mapping {
    Movie { title: String, year: i64 },
    /// `show` › `season` › `episode`
    Episode(EpisodeMapping),
    /// Grouped under `artist` when the name says `Artist - Title`.
    Track { artist: Option<String>, title: String, number: Option<i64>, disc: Option<i64> },
    MusicVideo { artist: Option<String>, title: String, subtitle: Option<String>, youtube_id: Option<String> },
    Video { title: String, youtube_id: Option<String> },
}

pub trait Classifier: Send + Sync {
    fn classify(&self, input: &ClassifyInput) -> Mapping;
}

/// The default classifier: file name patterns only.
pub struct FilenameClassifier;

fn stem_of(file_name: &str) -> &str {
    match file_name.rfind('.') {
        Some(i) if i > 0 => &file_name[..i],
        _ => file_name,
    }
}

impl Classifier for FilenameClassifier {
    fn classify(&self, input: &ClassifyInput) -> Mapping {
        let stem = stem_of(input.file_name).trim();
        let (rest, youtube_id) = strip_youtube_id(stem);

        // audio-only -> track: `NN - Artist - Title`, `Artist - Title`, `Title`
        if !input.has_video && input.has_audio {
            let (disc, number, title) = parse_track(&rest);
            let (artist, title) = match parse_artist_title(&title) {
                Some((a, t)) => (Some(a), t),
                None => (None, humanize(&title)),
            };
            let title = clean(title).unwrap_or_else(|| stem.to_string());
            return Mapping::Track { artist, title, number, disc };
        }

        // `Show - S01E02 - Title`, `1x02`, `ep02`, multi-episode
        if let Some(ep) = parse_episode(stem).filter(|e| e.pattern == EpisodePattern::Numbered) {
            if let Some(episode) = ep.episode {
                let show_raw = ep.show.clone().unwrap_or_else(|| "Unknown Show".into());
                let (show, show_year) = parse_movie(&show_raw);
                let show = clean(show).unwrap_or(show_raw);
                return Mapping::Episode(EpisodeMapping {
                    show,
                    show_year,
                    season: ep.season.unwrap_or(1),
                    episode,
                    episode_end: ep.episode_end.filter(|e| *e != episode),
                    title: ep.title,
                });
            }
        }

        // YouTube-style uploads: `Title ｜ Album ｜ People [id]`
        if let Some(p) = parse_pipe(stem) {
            return Mapping::MusicVideo {
                artist: None,
                title: p.title,
                subtitle: clean(p.segments.join(" · ")),
                youtube_id: p.youtube_id,
            };
        }

        // `Title (Year)`
        let (title, year) = parse_movie(&rest);
        if let (Some(year), Some(title)) = (year, clean(&title)) {
            return Mapping::Movie { title, year };
        }

        // `Artist - Title` video
        if let Some((artist, title)) = parse_mv_artist_title(&humanize(&rest)) {
            return Mapping::MusicVideo {
                title: clean_video_title(&title),
                subtitle: Some(artist.clone()),
                artist: Some(artist),
                youtube_id,
            };
        }

        let title = clean(clean_video_title(&humanize(&rest))).unwrap_or_else(|| stem.to_string());
        Mapping::Video { title, youtube_id }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(name: &str) -> Mapping {
        FilenameClassifier.classify(&ClassifyInput { file_name: name, has_video: true, has_audio: true })
    }
    fn audio(name: &str) -> Mapping {
        FilenameClassifier.classify(&ClassifyInput { file_name: name, has_video: false, has_audio: true })
    }
    fn ep(show: &str, season: i64, episode: i64, end: Option<i64>, title: Option<&str>) -> Mapping {
        Mapping::Episode(EpisodeMapping {
            show: show.into(),
            show_year: None,
            season,
            episode,
            episode_end: end,
            title: title.map(Into::into),
        })
    }

    #[test]
    fn movies() {
        assert_eq!(video("Big Buck Test (2008).mp4"), Mapping::Movie { title: "Big Buck Test".into(), year: 2008 });
        assert_eq!(video("Elephants.Dream.2006.720p.mp4"), Mapping::Movie { title: "Elephants Dream".into(), year: 2006 });
        // a labelled second file maps to the same movie (becomes a version)
        assert_eq!(video("Versions Test (2015) - 480p.mp4"), Mapping::Movie { title: "Versions Test".into(), year: 2015 });
        assert_eq!(video("Blade Runner 2049 (2017).mkv"), Mapping::Movie { title: "Blade Runner 2049".into(), year: 2017 });
    }

    #[test]
    fn episodes() {
        assert_eq!(video("Test Show - S01E02 - The Second.mp4"), ep("Test Show", 1, 2, None, Some("The Second")));
        assert_eq!(video("Test Show - S01E04-E05 - Double.mp4"), ep("Test Show", 1, 4, Some(5), Some("Double")));
        assert_eq!(video("Test Show - S00E01 - Special.mp4"), ep("Test Show", 0, 1, None, Some("Special")));
        assert_eq!(video("show.name.1x03.mkv"), ep("show name", 1, 3, None, None));
        assert_eq!(video("Anime Test ep01-ep02.mp4"), ep("Anime Test", 1, 1, Some(2), None));
        assert_eq!(video("S02E01.mkv"), ep("Unknown Show", 2, 1, None, None));
        match video("Angel (1999) S01E01.mkv") {
            Mapping::Episode(e) => assert_eq!((e.show.as_str(), e.show_year), ("Angel", Some(1999))),
            m => panic!("{m:?}"),
        }
        // loose numbers and dates are not episodes outside of folders
        assert_eq!(video("Holiday 1080.mp4"), Mapping::Video { title: "Holiday 1080".into(), youtube_id: None });
        assert!(matches!(video("Daily Show 2024.10.31.mp4"), Mapping::Video { .. }));
    }

    #[test]
    fn tracks() {
        assert_eq!(
            audio("Other Band - Single.ogg"),
            Mapping::Track { artist: Some("Other Band".into()), title: "Single".into(), number: None, disc: None }
        );
        assert_eq!(audio("01 - Opening.mp3"), Mapping::Track { artist: None, title: "Opening".into(), number: Some(1), disc: None });
        assert_eq!(
            audio("03 - Test Artist - Closing.flac"),
            Mapping::Track { artist: Some("Test Artist".into()), title: "Closing".into(), number: Some(3), disc: None }
        );
        // audio wins over every other pattern
        assert!(matches!(audio("Some Movie (2001).mp3"), Mapping::Track { .. }));
        assert!(matches!(audio("Podcast S01E01.mp3"), Mapping::Track { .. }));
    }

    #[test]
    fn music_videos_and_videos() {
        assert_eq!(
            video("Test Singer - First Hit.mkv"),
            Mapping::MusicVideo {
                artist: Some("Test Singer".into()),
                title: "First Hit".into(),
                subtitle: Some("Test Singer".into()),
                youtube_id: None
            }
        );
        assert_eq!(
            video("Pipe Song 8K Video Song ｜ Some Film ｜ Lead Star [abcdefghijk].mkv"),
            Mapping::MusicVideo {
                artist: None,
                title: "Pipe Song".into(),
                subtitle: Some("Some Film · Lead Star".into()),
                youtube_id: Some("abcdefghijk".into())
            }
        );
        match video("O SAKI SAKI - Nora Fatehi.mkv") {
            Mapping::MusicVideo { artist, title, .. } => assert_eq!((artist.as_deref(), title.as_str()), (Some("Nora Fatehi"), "O SAKI SAKI")),
            m => panic!("{m:?}"),
        }
        assert_eq!(video("beach_day.mp4"), Mapping::Video { title: "beach day".into(), youtube_id: None });
        assert_eq!(video("Making Of.mp4"), Mapping::Video { title: "Making Of".into(), youtube_id: None });
    }
}
