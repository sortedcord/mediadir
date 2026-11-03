use serde::{Deserialize, Serialize};

/// A virtual library groups items by kind; there is one per kind that has items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryKind {
    Movies,
    Shows,
    Music,
    MusicVideos,
    Videos,
}

impl LibraryKind {
    pub const ALL: [LibraryKind; 5] =
        [LibraryKind::Movies, LibraryKind::Shows, LibraryKind::Music, LibraryKind::MusicVideos, LibraryKind::Videos];

    pub fn as_str(self) -> &'static str {
        match self {
            LibraryKind::Movies => "movies",
            LibraryKind::Shows => "shows",
            LibraryKind::Music => "music",
            LibraryKind::MusicVideos => "music_videos",
            LibraryKind::Videos => "videos",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LibraryKind::Movies => "Movies",
            LibraryKind::Shows => "Shows",
            LibraryKind::Music => "Music",
            LibraryKind::MusicVideos => "Music Videos",
            LibraryKind::Videos => "Videos",
        }
    }

    /// Stable id of the virtual library.
    pub fn id(self) -> String {
        super::fnv1a(&format!("library\u{0}{}", self.as_str()))
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    pub fn of(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Movie => LibraryKind::Movies,
            ItemKind::Show | ItemKind::Season | ItemKind::Episode => LibraryKind::Shows,
            ItemKind::Artist | ItemKind::Album | ItemKind::Track => LibraryKind::Music,
            ItemKind::MusicVideo => LibraryKind::MusicVideos,
            ItemKind::Video => LibraryKind::Videos,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Movie,
    Show,
    Season,
    Episode,
    Artist,
    Album,
    Track,
    MusicVideo,
    Video,
}

impl ItemKind {
    pub const ALL: [ItemKind; 9] = [
        ItemKind::Movie,
        ItemKind::Show,
        ItemKind::Season,
        ItemKind::Episode,
        ItemKind::Artist,
        ItemKind::Album,
        ItemKind::Track,
        ItemKind::MusicVideo,
        ItemKind::Video,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Movie => "movie",
            ItemKind::Show => "show",
            ItemKind::Season => "season",
            ItemKind::Episode => "episode",
            ItemKind::Artist => "artist",
            ItemKind::Album => "album",
            ItemKind::Track => "track",
            ItemKind::MusicVideo => "music_video",
            ItemKind::Video => "video",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
    pub fn playable(self) -> bool {
        matches!(self, ItemKind::Movie | ItemKind::Episode | ItemKind::Track | ItemKind::MusicVideo | ItemKind::Video)
    }
    pub fn aspect(self) -> &'static str {
        match self {
            ItemKind::Movie | ItemKind::Show | ItemKind::Season => "poster",
            ItemKind::Episode | ItemKind::MusicVideo | ItemKind::Video => "landscape",
            ItemKind::Artist | ItemKind::Album | ItemKind::Track => "square",
        }
    }
}
