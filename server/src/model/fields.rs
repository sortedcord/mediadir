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
