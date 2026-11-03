use super::views::{ItemDetail, ItemSummary, Playback, Views};
use super::{JsonBody, QueryParams, Shared};
use crate::db::{ItemQuery, Repo};
use crate::display::FieldRegistry;
use crate::error::{AppError, AppResult};
use crate::model::{FieldType, Item, ItemKind, Role, CORE_FIELDS};
use axum::{
    extract::{Path, State},
    routing::{delete, get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/items", get(list))
        .route("/api/items/{id}", get(detail).patch(patch_item).delete(super::ingest::delete_item))
        .route("/api/items/{id}/fields/{key}", put(set_field).delete(delete_field))
        .route("/api/items/{id}/credits", axum::routing::post(add_credit))
        .route("/api/items/{id}/credits/{credit_id}", delete(remove_credit))
        .route("/api/items/{id}/playback", get(playback))
}

#[derive(Deserialize, Default)]
struct ListParams {
    library_id: Option<String>,
    kind: Option<String>,
    parent_id: Option<String>,
    person_id: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    top: Option<String>,
}

#[derive(Serialize)]
struct ItemList {
    total: i64,
    items: Vec<ItemSummary>,
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

async fn list(State(st): State<Shared>, QueryParams(p): QueryParams<ListParams>) -> AppResult<Json<ItemList>> {
    if let Some(k) = &p.kind {
        if ItemKind::parse(k).is_none() {
            return Err(AppError::bad(format!("unknown item kind '{k}'")));
        }
    }
    if let Some(s) = &p.sort {
        if !["title", "added", "year", "index"].contains(&s.as_str()) {
            return Err(AppError::bad("sort must be one of title, added, year, index"));
        }
    }
    let desc = match p.order.as_deref() {
        None => None,
        Some("asc") => Some(false),
        Some("desc") => Some(true),
        Some(_) => return Err(AppError::bad("order must be asc or desc")),
    };
    // top-level only by default, unless a parent, kind, person or query narrows the listing
    let default_top = p.parent_id.is_none() && p.kind.is_none() && p.person_id.is_none() && p.q.is_none();
    let top = p.top.as_deref().and_then(parse_bool).unwrap_or(default_top);
    let q = ItemQuery {
        library_id: p.library_id,
        kind: p.kind,
        parent_id: p.parent_id,
        person_id: p.person_id,
        q: p.q,
        sort: p.sort,
        desc,
        top,
        limit: p.limit.unwrap_or(50),
        offset: p.offset.unwrap_or(0),
    };
    let out = st
        .db
        .call(move |r| {
            let (total, items) = r.query_items(&q)?;
            Ok(ItemList { total, items: Views::new(r)?.summaries(&items)? })
        })
        .await?;
    Ok(Json(out))
}

fn detail_of(r: &Repo, id: &str) -> AppResult<ItemDetail> {
    let item = r.get_item(id)?;
    Views::new(r)?.detail(&item)
}

async fn detail(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<ItemDetail>> {
    Ok(Json(st.db.call(move |r| detail_of(r, &id)).await?))
}

fn opt_string(key: &str, v: &Value) -> AppResult<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        _ => Err(AppError::bad(format!("{key} must be a string or null"))),
    }
}

fn opt_int(key: &str, v: &Value) -> AppResult<Option<i64>> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).map(Some).ok_or_else(|| AppError::bad(format!("{key} must be an integer"))),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => s.trim().parse().map(Some).map_err(|_| AppError::bad(format!("{key} must be an integer"))),
        _ => Err(AppError::bad(format!("{key} must be an integer or null"))),
    }
}

/// Kinds a top-level playable item can be switched between (no parent required).
const STANDALONE_KINDS: &[ItemKind] = &[ItemKind::Movie, ItemKind::MusicVideo, ItemKind::Video, ItemKind::Track];

/// Apply a core-field edit and mark the field as manually edited.
fn apply_core(item: &mut Item, key: &str, v: &Value) -> AppResult<()> {
    match key {
        "kind" => {
            let k = v.as_str().and_then(ItemKind::parse).ok_or_else(|| AppError::bad("kind must be an item kind"))?;
            if k != item.kind {
                if !STANDALONE_KINDS.contains(&k) || !STANDALONE_KINDS.contains(&item.kind) || item.parent_id.is_some() {
                    return Err(AppError::bad("kind can only change between movie, music_video, video and track on top-level items"));
                }
                item.kind = k;
                // its name identity no longer applies: later files are not grouped into it
                item.group_key = None;
            }
        }
        "title" => item.title = opt_string(key, v)?.ok_or_else(|| AppError::bad("title cannot be empty"))?,
        "sort_title" => item.sort_title = opt_string(key, v)?,
        "subtitle" => item.subtitle = opt_string(key, v)?,
        "overview" => item.overview = opt_string(key, v)?,
        "year" => item.year = opt_int(key, v)?,
        "index" => item.index = opt_int(key, v)?,
        "parent_index" => item.parent_index = opt_int(key, v)?,
        "genres" => {
            item.genres = match v {
                Value::Null => vec![],
                Value::Array(a) => a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                Value::String(s) => s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                _ => return Err(AppError::bad("genres must be an array of strings")),
            }
        }
        _ => return Err(AppError::bad(format!("'{key}' is not an editable core field"))),
    }
    if !item.locked.iter().any(|l| l == key) {
        item.locked.push(key.to_string());
    }
    Ok(())
}

async fn patch_item(
    State(st): State<Shared>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<Map<String, Value>>,
) -> AppResult<Json<ItemDetail>> {
    Ok(Json(
        st.db
            .call(move |r| {
                let mut item = r.get_item(&id)?;
                for (k, v) in &body {
                    if CORE_FIELDS.contains(&k.as_str()) {
                        apply_core(&mut item, k, v)?;
                    } else if k != "id" {
                        return Err(AppError::bad(format!("'{k}' cannot be edited here (use PUT /api/items/{{id}}/fields/{{key}})")));
                    }
                }
                r.save_item(&item)?;
                detail_of(r, &id)
            })
            .await?,
    ))
}

#[derive(Deserialize)]
struct FieldValue {
    value: Value,
}

/// Loose validation of a value against a field type.
pub fn validate_value(t: FieldType, v: &Value) -> AppResult<Value> {
    let bad = |what: &str| AppError::bad(format!("value must be {what}"));
    Ok(match t {
        FieldType::Number | FieldType::Rating | FieldType::Duration => match v {
            Value::Number(_) => v.clone(),
            Value::String(s) => s.trim().parse::<f64>().map(Value::from).map_err(|_| bad("a number"))?,
            _ => return Err(bad("a number")),
        },
        FieldType::Bool => match v {
            Value::Bool(_) => v.clone(),
            Value::String(s) => Value::Bool(parse_bool(s).ok_or_else(|| bad("a boolean"))?),
            _ => return Err(bad("a boolean")),
        },
        FieldType::Tags => match v {
            Value::Array(a) if a.iter().all(|x| x.is_string()) => v.clone(),
            Value::String(s) => Value::Array(s.split(',').map(|x| Value::String(x.trim().into())).filter(|x| x != "").collect()),
            _ => return Err(bad("an array of strings")),
        },
        FieldType::Person => match v {
            Value::String(_) => v.clone(),
            Value::Array(a) if a.iter().all(|x| x.is_string()) => v.clone(),
            _ => return Err(bad("a person id or an array of person ids")),
        },
        FieldType::Text | FieldType::Date | FieldType::Url => match v {
            Value::String(_) => v.clone(),
            Value::Number(n) => Value::String(n.to_string()),
            _ => return Err(bad("a string")),
        },
    })
}

async fn set_field(
    State(st): State<Shared>,
    Path((id, key)): Path<(String, String)>,
    JsonBody(b): JsonBody<FieldValue>,
) -> AppResult<Json<ItemDetail>> {
    Ok(Json(
        st.db
            .call(move |r| {
                let registry = FieldRegistry::load(r)?;
                let def = registry.get(&key).ok_or_else(|| AppError::not_found(format!("field '{key}'")))?;
                if def.derived {
                    return Err(AppError::bad(format!("'{key}' is derived and cannot be set")));
                }
                let mut item = r.get_item(&id)?;
                if CORE_FIELDS.contains(&key.as_str()) {
                    apply_core(&mut item, &key, &b.value)?;
                } else if b.value.is_null() {
                    item.fields.remove(&key);
                } else {
                    let v = validate_value(def.field_type, &b.value)?;
                    item.fields.insert(key.clone(), v);
                }
                r.save_item(&item)?;
                detail_of(r, &id)
            })
            .await?,
    ))
}

async fn delete_field(State(st): State<Shared>, Path((id, key)): Path<(String, String)>) -> AppResult<Json<ItemDetail>> {
    Ok(Json(
        st.db
            .call(move |r| {
                let mut item = r.get_item(&id)?;
                item.fields.remove(&key);
                // core fields: no longer marked as edited
                item.locked.retain(|l| l != &key);
                r.save_item(&item)?;
                detail_of(r, &id)
            })
            .await?,
    ))
}

#[derive(Deserialize)]
struct AddCredit {
    person_id: Option<String>,
    person_name: Option<String>,
    role: String,
    character: Option<String>,
    order: Option<i64>,
}

async fn add_credit(State(st): State<Shared>, Path(id): Path<String>, JsonBody(b): JsonBody<AddCredit>) -> AppResult<Json<ItemDetail>> {
    let role = Role::new(&b.role);
    if role.as_str().is_empty() {
        return Err(AppError::bad("role is required"));
    }
    Ok(Json(
        st.db
            .call(move |r| {
                r.get_item(&id)?;
                let pid = match (b.person_id.as_deref(), b.person_name.as_deref().map(str::trim)) {
                    (Some(pid), _) => r.get_person(pid)?.id,
                    (None, Some(name)) if !name.is_empty() => r.ensure_person(name, None)?,
                    _ => return Err(AppError::bad("person_id or person_name is required")),
                };
                let character = b.character.as_deref().map(str::trim).filter(|c| !c.is_empty());
                r.add_manual_credit(&id, &pid, &role, character, b.order)?;
                detail_of(r, &id)
            })
            .await?,
    ))
}

async fn remove_credit(State(st): State<Shared>, Path((id, credit_id)): Path<(String, String)>) -> AppResult<Json<ItemDetail>> {
    Ok(Json(
        st.db
            .call(move |r| {
                r.delete_credit(&id, &credit_id)?;
                detail_of(r, &id)
            })
            .await?,
    ))
}

async fn playback(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Playback>> {
    Ok(Json(
        st.db
            .call(move |r| {
                let item = r.get_item(&id)?;
                Views::new(r)?.playback(&item)
            })
            .await?,
    ))
}
