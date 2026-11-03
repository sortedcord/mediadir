use super::{to_json, Repo};
use crate::error::AppResult;
use crate::model::{DisplaySpec, FieldDefinition, FieldType};
use rusqlite::params;

/// A stored field definition: a custom definition, or an override of a builtin's label/display.
#[derive(Debug, Clone)]
pub struct StoredField {
    pub def: FieldDefinition,
}

impl Repo<'_> {
    pub fn stored_fields(&self) -> AppResult<Vec<StoredField>> {
        let mut st = self.conn.prepare("SELECT key, label, type, applies_to, display, builtin FROM field_defs")?;
        let rows = st
            .query_map([], |r| {
                let ty: String = r.get(2)?;
                let applies: String = r.get(3)?;
                let display: String = r.get(4)?;
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, ty, applies, display, r.get::<_, bool>(5)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(key, label, ty, applies, display, builtin)| {
                let field_type: FieldType = serde_json::from_value(serde_json::Value::String(ty)).ok()?;
                let display: DisplaySpec = serde_json::from_str(&display).ok()?;
                Some(StoredField {
                    def: FieldDefinition {
                        key,
                        label,
                        field_type,
                        builtin,
                        derived: false,
                        applies_to: serde_json::from_str(&applies).unwrap_or_default(),
                        display,
                    },
                })
            })
            .collect())
    }

    /// `auto`: created by the ingest pipeline (e.g. `youtube`) rather than by a user.
    pub fn save_field_def(&self, def: &FieldDefinition, auto: bool) -> AppResult<()> {
        let ty = serde_json::to_value(def.field_type)?.as_str().unwrap_or("text").to_string();
        self.conn.execute(
            "INSERT OR REPLACE INTO field_defs (key, label, type, applies_to, display, builtin, auto)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![def.key, def.label, ty, to_json(&def.applies_to), to_json(&def.display), def.builtin, auto],
        )?;
        Ok(())
    }

    pub fn delete_field_def(&self, key: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM field_defs WHERE key = ?1", [key])?;
        Ok(())
    }
}
