use super::{ok, JsonBody, Shared};
use crate::display::FieldRegistry;
use crate::error::{AppError, AppResult};
use crate::model::{DisplaySpec, FieldDefinition, FieldType, ItemKind, Placement, Widget};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/fields", get(list).post(create))
        .route("/api/fields/{key}", put(update).delete(remove).get(get_one))
}

async fn list(State(st): State<Shared>) -> AppResult<Json<Vec<FieldDefinition>>> {
    Ok(Json(st.db.call(|r| Ok(FieldRegistry::load(r)?.all().to_vec())).await?))
}

async fn get_one(State(st): State<Shared>, Path(key): Path<String>) -> AppResult<Json<FieldDefinition>> {
    Ok(Json(
        st.db
            .call(move |r| FieldRegistry::load(r)?.get(&key).cloned().ok_or_else(|| AppError::not_found(format!("field '{key}'"))))
            .await?,
    ))
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 64 && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn validate_applies(a: &[String]) -> AppResult<()> {
    for k in a {
        if ItemKind::parse(k).is_none() && k != "person" {
            return Err(AppError::bad(format!("unknown item kind '{k}' in applies_to")));
        }
    }
    Ok(())
}

/// Merge a partial display object over a base spec.
fn merge_display(base: &DisplaySpec, patch: Option<Value>) -> AppResult<DisplaySpec> {
    let mut v = serde_json::to_value(base)?;
    if let (Some(Value::Object(p)), Value::Object(b)) = (patch, &mut v) {
        for (k, x) in p {
            b.insert(k, x);
        }
    }
    serde_json::from_value(v).map_err(|e| AppError::bad(format!("invalid display: {e}")))
}

fn validate_color(d: &DisplaySpec) -> AppResult<()> {
    if let Some(c) = &d.color {
        let ok = c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(AppError::bad("display.color must be #RRGGBB or null"));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct CreateField {
    key: String,
    label: Option<String>,
    #[serde(rename = "type")]
    field_type: FieldType,
    #[serde(default)]
    applies_to: Vec<String>,
    display: Option<Value>,
}

fn default_display(t: FieldType) -> DisplaySpec {
    let widget = match t {
        FieldType::Tags => Widget::Tags,
        FieldType::Rating => Widget::Rating,
        FieldType::Url => Widget::Link,
        FieldType::Person => Widget::People,
        FieldType::Image => Widget::Image,
        _ => Widget::Text,
    };
    let placement = match t {
        FieldType::Person => Placement::Section,
        // images are consumed by clients explicitly, not rendered as generic blocks
        FieldType::Image => Placement::Hidden,
        _ => Placement::Info,
    };
    DisplaySpec { placement, widget, order: 300, format: None, color: None, icon: None }
}

async fn create(State(st): State<Shared>, JsonBody(b): JsonBody<CreateField>) -> AppResult<(StatusCode, Json<FieldDefinition>)> {
    let key = b.key.trim().to_string();
    if !valid_key(&key) {
        return Err(AppError::bad("key must match [a-z0-9_]{1,64}"));
    }
    if FieldRegistry::is_builtin(&key) || crate::model::CORE_FIELDS.contains(&key.as_str()) {
        return Err(AppError::Conflict(format!("'{key}' is a builtin field")));
    }
    validate_applies(&b.applies_to)?;
    let display = merge_display(&default_display(b.field_type), b.display)?;
    validate_color(&display)?;
    let def = FieldDefinition {
        label: b.label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).unwrap_or_else(|| key.clone()),
        key,
        field_type: b.field_type,
        builtin: false,
        derived: false,
        applies_to: b.applies_to,
        display,
    };
    let d2 = def.clone();
    st.db
        .call(move |r| {
            if r.stored_fields()?.iter().any(|s| s.def.key == d2.key) {
                return Err(AppError::Conflict(format!("field '{}' already exists", d2.key)));
            }
            r.save_field_def(&d2, false)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(def)))
}

#[derive(Deserialize)]
struct UpdateField {
    label: Option<String>,
    applies_to: Option<Vec<String>>,
    display: Option<Value>,
}

async fn update(State(st): State<Shared>, Path(key): Path<String>, JsonBody(b): JsonBody<UpdateField>) -> AppResult<Json<FieldDefinition>> {
    Ok(Json(
        st.db
            .call(move |r| {
                let registry = FieldRegistry::load(r)?;
                let mut def = registry.get(&key).cloned().ok_or_else(|| AppError::not_found(format!("field '{key}'")))?;
                if let Some(l) = b.label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()) {
                    def.label = l;
                }
                if let Some(a) = b.applies_to {
                    if def.builtin {
                        return Err(AppError::bad("applies_to of builtin fields cannot be changed"));
                    }
                    validate_applies(&a)?;
                    def.applies_to = a;
                }
                def.display = merge_display(&def.display, b.display)?;
                validate_color(&def.display)?;
                r.save_field_def(&def, false)?;
                FieldRegistry::load(r)?.get(&key).cloned().ok_or_else(|| AppError::not_found("field"))
            })
            .await?,
    ))
}

async fn remove(State(st): State<Shared>, Path(key): Path<String>) -> AppResult<Json<Value>> {
    st.db
        .call(move |r| {
            if FieldRegistry::is_builtin(&key) {
                return Err(AppError::bad("builtin fields cannot be deleted (set display.placement to hidden instead)"));
            }
            if !r.stored_fields()?.iter().any(|s| s.def.key == key) {
                return Err(AppError::not_found(format!("field '{key}'")));
            }
            r.tx(|r| {
                r.delete_field_def(&key)?;
                r.remove_field_everywhere(&key)
            })
        })
        .await?;
    Ok(ok())
}
