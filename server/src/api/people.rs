use super::items::validate_value;
use super::views::{PersonDetail, PersonSummary, Views};
use super::{ok, JsonBody, QueryParams, Shared};
use crate::db::{default_sort_name, PeopleQuery, Repo};
use crate::display::FieldRegistry;
use crate::error::{AppError, AppResult};
use crate::model::{person_id, Person};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/people", get(list).post(create))
        .route("/api/people/{id}", get(detail).patch(patch_person).delete(remove))
        .route("/api/people/{id}/fields/{key}", put(set_field).delete(delete_field))
}

#[derive(Deserialize)]
struct ListParams {
    q: Option<String>,
    role: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct PeopleList {
    total: i64,
    people: Vec<PersonSummary>,
}

async fn list(State(st): State<Shared>, QueryParams(p): QueryParams<ListParams>) -> AppResult<Json<PeopleList>> {
    let q = PeopleQuery { q: p.q, role: p.role, limit: p.limit.unwrap_or(50), offset: p.offset.unwrap_or(0) };
    Ok(Json(
        st.db
            .call(move |r| {
                let (total, people) = r.list_people(&q)?;
                let v = Views::new(r)?;
                Ok(PeopleList { total, people: people.iter().map(|p| v.person_summary(p)).collect::<AppResult<_>>()? })
            })
            .await?,
    ))
}

fn detail_of(r: &Repo, id: &str) -> AppResult<PersonDetail> {
    let p = r.get_person(id)?;
    Views::new(r)?.person_detail(&p)
}

async fn detail(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<PersonDetail>> {
    Ok(Json(st.db.call(move |r| detail_of(r, &id)).await?))
}

#[derive(Deserialize)]
struct PersonBody {
    name: Option<String>,
    sort_name: Option<Option<String>>,
    bio: Option<Option<String>>,
    birth_date: Option<Option<String>>,
}

fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

async fn create(State(st): State<Shared>, JsonBody(b): JsonBody<PersonBody>) -> AppResult<(StatusCode, Json<PersonDetail>)> {
    let name = clean(b.name).ok_or_else(|| AppError::bad("name is required"))?;
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let p = Person {
        id: person_id(&name),
        sort_name: clean(b.sort_name.flatten()).or_else(|| Some(default_sort_name(&name))),
        name,
        bio: clean(b.bio.flatten()),
        birth_date: clean(b.birth_date.flatten()),
        image: None,
        fields: Default::default(),
    };
    let out = st
        .db
        .call(move |r| {
            r.insert_person(&p)?;
            detail_of(r, &p.id)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(out)))
}

/// PATCH keeps the id stable even when the name changes.
async fn patch_person(State(st): State<Shared>, Path(id): Path<String>, JsonBody(b): JsonBody<PersonBody>) -> AppResult<Json<PersonDetail>> {
    Ok(Json(
        st.db
            .call(move |r| {
                let mut p = r.get_person(&id)?;
                if let Some(n) = clean(b.name) {
                    p.name = n;
                }
                if let Some(s) = b.sort_name {
                    p.sort_name = clean(s).or_else(|| Some(default_sort_name(&p.name)));
                }
                if let Some(s) = b.bio {
                    p.bio = clean(s);
                }
                if let Some(s) = b.birth_date {
                    p.birth_date = clean(s);
                }
                r.update_person(&p)?;
                detail_of(r, &id)
            })
            .await?,
    ))
}

async fn remove(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    st.db.call(move |r| r.delete_person(&id)).await?;
    Ok(ok())
}

#[derive(Deserialize)]
struct FieldValue {
    value: Value,
}

async fn set_field(
    State(st): State<Shared>,
    Path((id, key)): Path<(String, String)>,
    JsonBody(b): JsonBody<FieldValue>,
) -> AppResult<Json<PersonDetail>> {
    Ok(Json(
        st.db
            .call(move |r| {
                let registry = FieldRegistry::load(r)?;
                let def = registry.get(&key).ok_or_else(|| AppError::not_found(format!("field '{key}'")))?;
                if def.derived {
                    return Err(AppError::bad(format!("'{key}' is derived and cannot be set")));
                }
                r.tx(|r| {
                    let mut p = r.get_person(&id)?;
                    if b.value.is_null() {
                        p.fields.remove(&key);
                    } else {
                        p.fields.insert(key.clone(), validate_value(r, def.field_type, &b.value)?);
                    }
                    r.set_person_fields(&id, &p.fields)?;
                    detail_of(r, &id)
                })
            })
            .await?,
    ))
}

async fn delete_field(State(st): State<Shared>, Path((id, key)): Path<(String, String)>) -> AppResult<Json<PersonDetail>> {
    Ok(Json(
        st.db
            .call(move |r| {
                let mut p = r.get_person(&id)?;
                p.fields.remove(&key);
                r.set_person_fields(&id, &p.fields)?;
                detail_of(r, &id)
            })
            .await?,
    ))
}
