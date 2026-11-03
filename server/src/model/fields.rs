use super::PersonRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Number,
    Date,
    Url,
    Tags,
    Rating,
    Bool,
    Duration,
    Person,
    /// One `{"hash": "<64 hex>"}` asset ref or an ordered list of them.
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    CardBadge,
    CardMeta,
    Header,
    Info,
    Section,
    Hidden,
}

impl Placement {
    pub fn on_card(self) -> bool {
        matches!(self, Placement::CardBadge | Placement::CardMeta)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Widget {
    Badge,
    Chip,
    Text,
    Rating,
    Link,
    Tags,
    People,
    Markdown,
    Stat,
    Image,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplaySpec {
    pub placement: Placement,
    pub widget: Widget,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    pub builtin: bool,
    pub derived: bool,
    pub applies_to: Vec<String>,
    pub display: DisplaySpec,
}

impl FieldDefinition {
    pub fn applies(&self, kind: &str) -> bool {
        self.applies_to.is_empty() || self.applies_to.iter().any(|k| k == kind)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DisplayBlock {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    pub widget: Widget,
    pub placement: Placement,
    pub order: i64,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub text: String,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub people: Option<Vec<PersonRef>>,
}

/// Parse the value of an `image` field: `{"hash": H}` or a non-empty array of those.
/// Returns the lower-cased hashes in order. Extra keys on a ref are ignored.
pub fn image_refs(v: &serde_json::Value) -> Result<Vec<String>, String> {
    use serde_json::Value;
    fn one(v: &Value) -> Result<String, String> {
        let h = v.get("hash").and_then(Value::as_str).ok_or("each image ref must be an object {\"hash\": \"<hex>\"}")?;
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("'{h}' is not a 64-char hex asset hash"));
        }
        Ok(h.to_ascii_lowercase())
    }
    match v {
        Value::Object(_) => Ok(vec![one(v)?]),
        Value::Array(a) if !a.is_empty() => a.iter().map(one).collect(),
        Value::Array(_) => Err("image list is empty (use null to clear the field)".into()),
        _ => Err("value must be {\"hash\": \"<hex>\"} or a list of those".into()),
    }
}

/// Canonical stored form: a single ref stays an object, a list stays a list.
pub fn image_value(v: &serde_json::Value, hashes: &[String]) -> serde_json::Value {
    use serde_json::{json, Value};
    match v {
        Value::Array(_) => Value::Array(hashes.iter().map(|h| json!({ "hash": h })).collect()),
        _ => json!({ "hash": hashes[0] }),
    }
}
