//! Filename parsers used by the classifier: `Title (Year)`, `S01E02` / `1x02`, `Artist - Title`,
//! `Title ｜ Album ｜ People [youtubeid]`, `01 - Track`.

/// Trim and drop empty strings.
pub fn clean(s: impl AsRef<str>) -> Option<String> {
    let t = s.as_ref().trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Scene-style names ("The.Movie.2019.1080p") use dots/underscores as spaces.
pub fn humanize(stem: &str) -> String {
    let s = stem.trim();
    let s = if !s.contains(' ') && (s.contains('.') || s.contains('_')) { s.replace(['.', '_'], " ") } else { s.to_string() };
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trim_seps(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '.' | '_' | '–' | '—' | ':'))
}

fn is_year(s: &str) -> Option<i64> {
    (s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten().filter(|y| (1900..2100).contains(y))
}

/// `Title (2019)`, `Title [2019]`, `Title.2019.1080p.BluRay` -> (title, year)
pub fn parse_movie(stem: &str) -> (String, Option<i64>) {
    let s = humanize(stem);
    for (open, close) in [('(', ')'), ('[', ']')] {
        let mut search = s.as_str();
        let mut offset = 0;
        while let Some(i) = search.find(open) {
            let after = &search[i + 1..];
            if let Some(j) = after.find(close) {
                if let Some(y) = is_year(after[..j].trim()) {
                    let title = trim_seps(&s[..offset + i]).to_string();
                    if !title.is_empty() {
                        return (title, Some(y));
                    }
                }
            }
            offset += i + 1;
            search = &s[offset..];
        }
    }
    let tokens: Vec<&str> = s.split(' ').collect();
    if let Some(pos) = tokens.iter().enumerate().skip(1).rev().find_map(|(i, t)| is_year(t).map(|_| i)) {
        let title = trim_seps(&tokens[..pos].join(" ")).to_string();
        if !title.is_empty() {
            return (title, is_year(tokens[pos]));
        }
    }
    (trim_seps(&s).to_string(), None)
}

/// Which kind of pattern recognised an episode.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub enum EpisodePattern {
    /// `S01E02`, `1x02`, `ep02` and their multi-episode forms.
    #[default]
    Numbered,
    /// `2024.10.31`, `31-10-2024`
    Date,
    /// A bare `102` (S1E02).
    Loose,
}

#[derive(Debug, PartialEq, Default)]
pub struct EpisodeName {
    pub pattern: EpisodePattern,
    /// None for season-less patterns (`ep02`); date-based episodes use the year.
    pub season: Option<i64>,
    pub episode: Option<i64>,
    /// Last episode of a multi-episode file.
    pub episode_end: Option<i64>,
    /// `YYYY-MM-DD` for date-based episodes.
    pub aired: Option<String>,
    pub show: Option<String>,
    pub title: Option<String>,
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new($pat).unwrap());
    };
}
re!(SXE, r"(?i)(?:^|[^a-z0-9])s(\d{1,3})[ ._x-]{0,2}e(\d{1,4})");
re!(SXE_CONT, r"(?i)^(?:[-_ .]*s\d{1,3}[ ._x-]{0,2}e|-e|_e|e|-ep|-)(\d{1,4})");
re!(NX, r"(?i)(?:^|[^a-z0-9])(\d{1,2})x(\d{2,3})");
re!(NX_CONT, r"(?i)^(?:x|-\d{1,2}x|-)(\d{2,3})");
re!(EP, r"(?i)(?:^|[^a-z0-9])ep[_ .]?(\d{1,4})");
re!(EP_CONT, r"(?i)^-(?:ep)?(\d{1,4})");
re!(DATE_YMD, r"(?:^|[^0-9])((?:19|20)\d{2})[.\-_ ](\d{2})[.\-_ ](\d{2})(?:[^0-9]|$)");
re!(DATE_DMY, r"(?:^|[^0-9])(\d{2})[.\-_ ](\d{2})[.\-_ ]((?:19|20)\d{2})(?:[^0-9]|$)");
re!(NUM, r"(?:^|[ ._-])(\d{1,2})(\d{2})(?:[ ._-]|$)");

fn next_is_digit(s: &str, at: usize) -> bool {
    s[at..].chars().next().map(|c| c.is_ascii_digit() || c == 'p' || c == 'P').unwrap_or(false)
}

/// Collect continuation episodes (`E02`, `-E03`, `-04`, `-S01E05`) after a match.
fn continuation(s: &str, mut pos: usize, cont: &regex::Regex) -> (usize, Option<i64>) {
    let mut last = None;
    while let Some(c) = cont.captures(&s[pos..]) {
        let m = c.get(1).unwrap();
        let end = pos + m.end();
        if next_is_digit(s, end) {
            break;
        }
        last = m.as_str().parse().ok();
        pos = end;
    }
    (pos, last)
}

fn finish(s: &str, start: usize, end: usize, mut e: EpisodeName) -> EpisodeName {
    e.show = clean(trim_seps(&humanize(&s[..start])));
    e.title = clean(trim_seps(&humanize(&s[end..])));
    e
}

/// Episode patterns: `S01E02`, `S1E2`, `S01.E02`, `S01_E02`, `S01xE02`, `1x02`, `ep02`,
/// multi-episode (`S01E01-E04`, `S01E01E02`, `s01e01-02-03`, `s01e01-s01e02`, `1x01x02`,
/// `1x01-1x04`, `ep01-ep02`), date-based (`2024.10.31`, `31-10-2024`) and `102` (S1E02).
pub fn parse_episode(stem: &str) -> Option<EpisodeName> {
    let s = stem.trim();
    if let Some(c) = SXE.captures(s) {
        let (m_start, m_end) = (c.get(1).unwrap().start() - 1, c.get(2).unwrap().end());
        if !next_is_digit(s, m_end) {
            let (end, last) = continuation(s, m_end, &SXE_CONT);
            let e = EpisodeName {
                season: c[1].parse().ok(),
                episode: c[2].parse().ok(),
                episode_end: last,
                ..Default::default()
            };
            return Some(finish(s, m_start, end, e));
        }
    }
    if let Some(c) = NX.captures(s) {
        let (m_start, m_end) = (c.get(1).unwrap().start(), c.get(2).unwrap().end());
        if !next_is_digit(s, m_end) {
            let (end, last) = continuation(s, m_end, &NX_CONT);
            let e = EpisodeName { season: c[1].parse().ok(), episode: c[2].parse().ok(), episode_end: last, ..Default::default() };
            return Some(finish(s, m_start, end, e));
        }
    }
    if let Some(c) = EP.captures(s) {
        let g = c.get(1).unwrap();
        let m_start = s[..g.start()].to_lowercase().rfind("ep").unwrap_or(g.start());
        let (end, last) = continuation(s, g.end(), &EP_CONT);
        let e = EpisodeName { season: None, episode: g.as_str().parse().ok(), episode_end: last, ..Default::default() };
        return Some(finish(s, m_start, end, e));
    }
    for (re, ymd) in [(&*DATE_YMD, true), (&*DATE_DMY, false)] {
        if let Some(c) = re.captures(s) {
            let (y, m, d) = if ymd { (&c[1], &c[2], &c[3]) } else { (&c[3], &c[2], &c[1]) };
            let (yi, mi, di): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
            if (1..=12).contains(&mi) && (1..=31).contains(&di) {
                let whole = c.get(0).unwrap();
                let e = EpisodeName {
                    season: Some(yi),
                    episode: Some(mi * 100 + di),
                    aired: Some(format!("{y}-{m}-{d}")),
                    pattern: EpisodePattern::Date,
                    ..Default::default()
                };
                return Some(finish(s, whole.start(), whole.end(), e));
            }
        }
    }
    if let Some(c) = NUM.captures(s) {
        let whole = format!("{}{}", &c[1], &c[2]);
        let n: i64 = whole.parse().ok()?;
        let is_year = whole.len() == 4 && (1900..2100).contains(&n);
        if !is_year && c.get(1).unwrap().start() > 0 {
            let e = EpisodeName {
                season: c[1].parse().ok(),
                episode: c[2].parse().ok(),
                pattern: EpisodePattern::Loose,
                ..Default::default()
            };
            return Some(finish(s, c.get(1).unwrap().start(), c.get(2).unwrap().end(), e));
        }
    }
    None
}

/// Strip a trailing YouTube id `[xxxxxxxxxxx]`.
pub fn strip_youtube_id(stem: &str) -> (String, Option<String>) {
    let t = stem.trim();
    if t.ends_with(']') {
        if let Some(i) = t.rfind('[') {
            let id = &t[i + 1..t.len() - 1];
            if id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return (t[..i].trim().to_string(), Some(id.to_string()));
            }
        }
    }
    (t.to_string(), None)
}

#[derive(Debug, PartialEq)]
pub struct PipeName {
    pub title: String,
    pub segments: Vec<String>,
    pub youtube_id: Option<String>,
}

/// `Title ｜ Album ｜ People [youtubeid]` (full-width or ASCII pipes).
pub fn parse_pipe(stem: &str) -> Option<PipeName> {
    let (s, youtube_id) = strip_youtube_id(stem);
    let parts: Vec<String> = s.split(['｜', '|']).filter_map(clean).collect();
    if parts.len() < 2 {
        return None;
    }
    Some(PipeName { title: clean_video_title(&parts[0]), segments: parts[1..].to_vec(), youtube_id })
}

/// `Artist - Title`
pub fn parse_artist_title(stem: &str) -> Option<(String, String)> {
    let (a, t) = stem.split_once(" - ")?;
    Some((clean(a)?, clean(t)?))
}

fn shouty(s: &str) -> bool {
    s.chars().any(|c| c.is_alphabetic()) && !s.chars().any(|c| c.is_lowercase())
}

/// `Artist - Title`, except `SHOUTED TITLE - Artist Name` (an all-caps left side with a
/// mixed-case right side is how song uploads are commonly named).
pub fn parse_mv_artist_title(stem: &str) -> Option<(String, String)> {
    let (a, b) = parse_artist_title(stem)?;
    if shouty(&a) && !shouty(&b) {
        Some((b, a))
    } else {
        Some((a, b))
    }
}

/// Removes upload noise: everything from a resolution marker ("8K Video Song", "4K Full Song") on.
pub fn clean_video_title(s: &str) -> String {
    const MARKERS: &[&str] = &["8k", "4k", "uhd", "fhd", "2160p", "4320p", "1080p", "720p", "(official", "[official", "official"];
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let cut = tokens.iter().enumerate().skip(1).find(|(_, t)| MARKERS.contains(&t.to_lowercase().as_str())).map(|(i, _)| i);
    let kept = match cut {
        Some(i) => tokens[..i].join(" "),
        None => tokens.join(" "),
    };
    let out = trim_seps(&kept).to_string();
    if out.is_empty() {
        s.trim().to_string()
    } else {
        out
    }
}

/// `01 - Title`, `01. Title`, `1-03 Title` -> (disc, track, title)
pub fn parse_track(stem: &str) -> (Option<i64>, Option<i64>, String) {
    let s = stem.trim();
    let lead: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if lead.is_empty() || lead.len() > 3 {
        return (None, None, s.to_string());
    }
    let rest = &s[lead.len()..];
    // disc-track: "1-03 Title"
    if let Some(r) = rest.strip_prefix('-') {
        let t: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !t.is_empty() && t.len() <= 3 && r[t.len()..].starts_with(' ') {
            return (lead.parse().ok(), t.parse().ok(), trim_seps(&r[t.len()..]).to_string());
        }
    }
    if rest.starts_with([' ', '.', '-', '_']) {
        let title = trim_seps(rest);
        if !title.is_empty() {
            return (None, lead.parse().ok(), title.to_string());
        }
    }
    (None, None, s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movie_names() {
        assert_eq!(parse_movie("Big Buck Bunny (2008)"), ("Big Buck Bunny".into(), Some(2008)));
        assert_eq!(parse_movie("The.Matrix.1999.1080p.BluRay.x264"), ("The Matrix".into(), Some(1999)));
        assert_eq!(parse_movie("2001 A Space Odyssey (1968)"), ("2001 A Space Odyssey".into(), Some(1968)));
        assert_eq!(parse_movie("1917"), ("1917".into(), None));
        assert_eq!(parse_movie("Blade Runner 2049 (2017)"), ("Blade Runner 2049".into(), Some(2017)));
        assert_eq!(parse_movie("Untitled"), ("Untitled".into(), None));
    }

    fn ep(s: &str) -> (Option<i64>, Option<i64>, Option<i64>, Option<String>) {
        let e = parse_episode(s).unwrap_or_else(|| panic!("no match: {s}"));
        (e.season, e.episode, e.episode_end, e.title)
    }

    #[test]
    fn episode_names() {
        let e = parse_episode("Test Show - S01E02 - The Second").unwrap();
        assert_eq!((e.season, e.episode, e.show.as_deref(), e.title.as_deref()), (Some(1), Some(2), Some("Test Show"), Some("The Second")));
        let e = parse_episode("test.show.s02e10.720p").unwrap();
        assert_eq!((e.season, e.episode), (Some(2), Some(10)));
        assert_eq!(e.show.as_deref(), Some("test show"));
        assert_eq!(ep("Name S1E2"), (Some(1), Some(2), None, None));
        assert_eq!(ep("Name S01.E02"), (Some(1), Some(2), None, None));
        assert_eq!(ep("Name S01_E02"), (Some(1), Some(2), None, None));
        assert_eq!(ep("Name S01xE02"), (Some(1), Some(2), None, None));
        assert_eq!(ep("1x03 Pilot"), (Some(1), Some(3), None, Some("Pilot".into())));
        assert_eq!(ep("Name ep02"), (None, Some(2), None, None));
        assert_eq!(ep("Name ep_02"), (None, Some(2), None, None));
        assert_eq!(parse_episode("S01E01").unwrap().title, None);
        assert_eq!(ep("Name S00E01 - Special"), (Some(0), Some(1), None, Some("Special".into())));
        // multi-episode
        assert_eq!(ep("Angel (1999) S01E01-E04"), (Some(1), Some(1), Some(4), None));
        assert_eq!(ep("name s01e01-s01e04"), (Some(1), Some(1), Some(4), None));
        assert_eq!(ep("name S01E01E02E04"), (Some(1), Some(1), Some(4), None));
        assert_eq!(ep("name s01e01-02-03"), (Some(1), Some(1), Some(3), None));
        assert_eq!(ep("name 1x01-1x04"), (Some(1), Some(1), Some(4), None));
        assert_eq!(ep("name 1x01x02"), (Some(1), Some(1), Some(2), None));
        assert_eq!(ep("name ep01-ep02"), (None, Some(1), Some(2), None));
        assert_eq!(ep("Show - S01E01-E02 - Double"), (Some(1), Some(1), Some(2), Some("Double".into())));
        assert_eq!(ep("Show S01E01 - 2 Fast"), (Some(1), Some(1), None, Some("2 Fast".into())));
        // date based
        let e = parse_episode("Daily Show 2024.10.31").unwrap();
        assert_eq!((e.season, e.episode, e.aired.as_deref()), (Some(2024), Some(1031), Some("2024-10-31")));
        assert_eq!(parse_episode("Daily 31-10-2024").unwrap().aired.as_deref(), Some("2024-10-31"));
        // S1E02 as 102
        assert_eq!(ep("Show 102 Title"), (Some(1), Some(2), None, Some("Title".into())));
        assert!(parse_episode("Show 2010").is_none());
        assert!(parse_episode("Season Finale 1920x1080").is_none());
        assert!(parse_episode("Just a movie").is_none());
    }

    #[test]
    fn pipe_names() {
        let p = parse_pipe("DILBAR 8K Full Song ｜ Satyameva Jayate ｜ John Abraham Nora Fatehi ｜Tanishk B Neha Kakkar Ikka Dhvani [siDLgxNEbP8]").unwrap();
        assert_eq!(p.title, "DILBAR");
        assert_eq!(p.segments, vec!["Satyameva Jayate", "John Abraham Nora Fatehi", "Tanishk B Neha Kakkar Ikka Dhvani"]);
        assert_eq!(p.youtube_id.as_deref(), Some("siDLgxNEbP8"));
        let p = parse_pipe("Aaj Ki Raat - 8K Video ｜ Stree 2 ｜ Tamannaah Bhatia [ri1Ar5nEq4s]").unwrap();
        assert_eq!(p.title, "Aaj Ki Raat");
        assert!(parse_pipe("O SAKI SAKI - Nora Fatehi").is_none());
        assert_eq!(strip_youtube_id("x [short]"), ("x [short]".into(), None));
    }

    #[test]
    fn artist_title_and_tracks() {
        assert_eq!(parse_artist_title("Daft Punk - One More Time"), Some(("Daft Punk".into(), "One More Time".into())));
        assert_eq!(parse_artist_title("NoSeparator"), None);
        assert_eq!(parse_mv_artist_title("O SAKI SAKI - Nora Fatehi"), Some(("Nora Fatehi".into(), "O SAKI SAKI".into())));
        assert_eq!(parse_mv_artist_title("Test Singer - First Hit"), Some(("Test Singer".into(), "First Hit".into())));
        assert_eq!(parse_track("01 - Intro"), (None, Some(1), "Intro".into()));
        assert_eq!(parse_track("02. Second"), (None, Some(2), "Second".into()));
        assert_eq!(parse_track("1-03 Third"), (Some(1), Some(3), "Third".into()));
        assert_eq!(parse_track("Song"), (None, None, "Song".into()));
        assert_eq!(parse_track("1999"), (None, None, "1999".into()));
        assert_eq!(clean_video_title("Kusu Kusu 8K Video Song"), "Kusu Kusu");
        assert_eq!(clean_video_title("8K"), "8K");
    }
}
