//! Domain types shared by the database, ingest pipeline, presenter and API layers.

mod fields;
mod ids;
mod kinds;
mod media;

pub use fields::*;
pub use ids::*;
pub use kinds::*;
pub use media::*;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type JsonMap = Map<String, Value>;

/// A virtual library: one per item kind that has items. `path` is always null.
#[derive(Debug, Clone, Serialize)]
pub struct Library {
    pub id: String,
    pub name: String,
    pub kind: LibraryKind,
    pub path: Option<String>,
}

impl Library {
    pub fn virtual_for(kind: LibraryKind) -> Self {
        Library { id: kind.id(), name: kind.name().to_string(), kind, path: None }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub parent_id: Option<String>,
    pub title: String,
    pub sort_title: Option<String>,
    pub subtitle: Option<String>,
    pub year: Option<i64>,
    pub index: Option<i64>,
    pub parent_index: Option<i64>,
    /// Last episode number of a multi-episode file (`S01E01-E03` -> index 1, index_end 3).
    pub index_end: Option<i64>,
    pub overview: Option<String>,
    pub genres: Vec<String>,
    pub duration: Option<f64>,
    pub added_at: i64,
    /// Milliseconds; bumped on every save (drives the sidecar sync).
    pub updated_at: i64,
    /// Core field names that were manually edited.
    pub locked: Vec<String>,
    /// Legacy column (generated thumbnails): no longer written or served.
    pub thumb: Option<String>,
    /// Field values produced by the classifier (e.g. `youtube`, `aired`).
    pub extra: JsonMap,
    /// Field values set by users (custom fields + editable builtins). Override `extra`.
    pub fields: JsonMap,
    /// Normalized identity from the file name; files mapping to the same key become versions.
    pub group_key: Option<String>,
}

impl Item {
    pub fn new(id: String, kind: ItemKind, title: String) -> Self {
        Item {
            id,
            kind,
            parent_id: None,
            title,
            sort_title: None,
            subtitle: None,
            year: None,
            index: None,
            parent_index: None,
            index_end: None,
            overview: None,
            genres: vec![],
            duration: None,
            added_at: 0,
            updated_at: 0,
            locked: vec![],
            thumb: None,
            extra: JsonMap::new(),
            fields: JsonMap::new(),
            group_key: None,
        }
    }

    pub fn library_id(&self) -> String {
        LibraryKind::of(self.kind).id()
    }

    /// User value first, then classifier value.
    pub fn field_value(&self, key: &str) -> Option<&Value> {
        self.fields.get(key).or_else(|| self.extra.get(key)).filter(|v| !is_empty_value(v))
    }
}

/// Core item fields that can be edited with PATCH and become `locked`.
pub const CORE_FIELDS: &[&str] =
    &["title", "sort_title", "subtitle", "year", "overview", "genres", "index", "parent_index", "kind"];

pub fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub bio: Option<String>,
    pub birth_date: Option<String>,
    /// Legacy column: no longer written or served (images are `image` field values).
    pub image: Option<String>,
    pub fields: JsonMap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditSource {
    Scan,
    Manual,
}

impl CreditSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CreditSource::Scan => "scan",
            CreditSource::Manual => "manual",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "manual" {
            CreditSource::Manual
        } else {
            CreditSource::Scan
        }
    }
}

/// Credit role: an open, lowercase set (`actor`, `director`, `artist`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Role(String);

impl Role {
    pub fn new(s: &str) -> Self {
        Role(s.trim().to_lowercase().replace([' ', '-'], "_"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct Credit {
    pub id: String,
    pub item_id: String,
    pub person_id: String,
    pub role: Role,
    pub character: Option<String>,
    pub order: i64,
    pub source: CreditSource,
}

/// Compact person reference embedded in credits and display blocks.
#[derive(Debug, Clone, Serialize)]
pub struct PersonRef {
    pub id: String,
    pub name: String,
}
