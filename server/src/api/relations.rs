use super::views::{ItemRelation, RelationDetail, RelationSummary, Views};
use super::{JsonBody, QueryParams, Shared};
use crate::db::{new_relation_id, normalize_type, Member, Relation, RelationQuery, Repo};
use crate::error::{AppError, AppResult};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/relations", get(list).post(create))
        .route("/api/relation-types", get(types))
        .route("/api/relations/{id}", get(detail).patch(patch_relation).delete(remove))
        .route("/api/relations/{id}/members", axum::routing::put(replace_members).post(upsert_member))
        .route("/api/relations/{id}/members/{item_id}", delete(remove_member))
        .route("/api/items/{id}/relations", get(for_item))
}

#[derive(Deserialize)]
struct ListParams {
    #[serde(rename = "type")]
    rtype: Option<String>,
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct RelationList {
    total: i64,
    relations: Vec<RelationSummary>,
}

async fn list(State(st): State<Shared>, QueryParams(p): QueryParams<ListParams>) -> AppResult<Json<RelationList>> {
    let q = RelationQuery { rtype: p.rtype, q: p.q, limit: p.limit.unwrap_or(50), offset: p.offset.unwrap_or(0) };
    Ok(Json(
        st.db
            .call(move |r| {
                let (total, rels) = r.list_relations(&q)?;
                Ok(RelationList { total, relations: rels.iter().map(Views::relation_summary).collect() })
            })
            .await?,
    ))
}

#[derive(Serialize)]
struct TypeCount {
    #[serde(rename = "type")]
    rtype: String,
    count: i64,
}

async fn types(State(st): State<Shared>) -> AppResult<Json<Vec<TypeCount>>> {
    Ok(Json(
        st.db
            .call(|r| Ok(r.relation_types()?.into_iter().map(|(rtype, count)| TypeCount { rtype, count }).collect()))
            .await?,
    ))
}

fn detail_of(r: &Repo, id: &str) -> AppResult<RelationDetail> {
    let rel = r.get_relation(id)?;
    Views::new(r)?.relation_detail(&rel)
}

async fn detail(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<RelationDetail>> {
    Ok(Json(st.db.call(move |r| detail_of(r, &id)).await?))
}

#[derive(Deserialize)]
struct MemberBody {
    item_id: String,
    ord: Option<i64>,
    role: Option<String>,
}

fn clean_role(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Validate a full member list: unique items, default `ord` is the list position.
fn to_members(list: Vec<MemberBody>) -> AppResult<Vec<Member>> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for (i, m) in list.into_iter().enumerate() {
        if m.item_id.trim().is_empty() {
            return Err(AppError::bad("member item_id is required"));
        }
        if !seen.insert(m.item_id.clone()) {
            return Err(AppError::bad(format!("duplicate member '{}'", m.item_id)));
        }
        out.push(Member { item_id: m.item_id, ord: m.ord.unwrap_or(i as i64), role: clean_role(m.role) });
    }
    Ok(out)
}

#[derive(Deserialize)]
struct CreateBody {
    #[serde(rename = "type")]
    rtype: Option<String>,
    #[serde(alias = "label")]
    name: Option<String>,
    members: Option<Vec<MemberBody>>,
}

async fn create(State(st): State<Shared>, JsonBody(b): JsonBody<CreateBody>) -> AppResult<(StatusCode, Json<RelationDetail>)> {
    let rtype = normalize_type(&b.rtype.unwrap_or_default())?;
    let name = b.name.unwrap_or_default().trim().to_string();
    let members = to_members(b.members.unwrap_or_default())?;
    if members.is_empty() {
        return Err(AppError::bad("a relation needs at least one member"));
    }
    let out = st
        .db
        .call(move |r| {
            r.tx(|r| {
                let id = new_relation_id();
                r.insert_relation(&id, &rtype, &name)?;
                r.set_relation_members(&id, &members)?;
                detail_of(r, &id)
            })
        })
        .await?;
    Ok((StatusCode::CREATED, Json(out)))
}

#[derive(Deserialize)]
struct PatchBody {
    #[serde(alias = "label")]
    name: Option<String>,
    #[serde(rename = "type")]
    rtype: Option<String>,
}

async fn patch_relation(State(st): State<Shared>, Path(id): Path<String>, JsonBody(b): JsonBody<PatchBody>) -> AppResult<Json<RelationDetail>> {
    let rtype = b.rtype.as_deref().map(normalize_type).transpose()?;
    Ok(Json(
        st.db
            .call(move |r| {
                let mut rel: Relation = r.get_relation(&id)?;
                if let Some(t) = rtype {
                    rel.rtype = t;
                }
                if let Some(n) = b.name {
                    rel.name = n.trim().to_string();
                }
                r.update_relation(&rel)?;
                detail_of(r, &id)
            })
            .await?,
    ))
}

async fn remove(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<StatusCode> {
    st.db.call(move |r| r.tx(|r| r.delete_relation(&id))).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Detail of a relation that may have just been auto-deleted: 204 then.
fn detail_or_gone(r: &Repo, id: &str) -> AppResult<Response> {
    if r.relation_exists(id)? {
        Ok(Json(detail_of(r, id)?).into_response())
    } else {
        Ok(StatusCode::NO_CONTENT.into_response())
    }
}

#[derive(Deserialize)]
struct ReplaceBody {
    members: Vec<MemberBody>,
}

async fn replace_members(State(st): State<Shared>, Path(id): Path<String>, JsonBody(b): JsonBody<ReplaceBody>) -> AppResult<Response> {
    let members = to_members(b.members)?;
    st.db
        .call(move |r| {
            r.tx(|r| {
                r.get_relation(&id)?;
                r.set_relation_members(&id, &members)?;
                detail_or_gone(r, &id)
            })
        })
        .await
}

async fn upsert_member(State(st): State<Shared>, Path(id): Path<String>, JsonBody(b): JsonBody<MemberBody>) -> AppResult<Json<RelationDetail>> {
    // a missing role keeps the current one; an empty string clears it
    let role = b.role.map(|s| clean_role(Some(s)));
    Ok(Json(
        st.db
            .call(move |r| {
                r.tx(|r| {
                    r.get_relation(&id)?;
                    r.upsert_relation_member(&id, &b.item_id, b.ord, role)?;
                    detail_of(r, &id)
                })
            })
            .await?,
    ))
}

async fn remove_member(State(st): State<Shared>, Path((id, item_id)): Path<(String, String)>) -> AppResult<Response> {
    st.db
        .call(move |r| {
            r.tx(|r| {
                r.get_relation(&id)?;
                r.remove_relation_member(&id, &item_id)?;
                detail_or_gone(r, &id)
            })
        })
        .await
}

async fn for_item(State(st): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Vec<ItemRelation>>> {
    Ok(Json(
        st.db
            .call(move |r| {
                r.get_item(&id)?;
                Views::new(r)?.item_relations(&id)
            })
            .await?,
    ))
}
