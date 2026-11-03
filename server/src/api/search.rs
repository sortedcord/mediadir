use super::views::{ItemSummary, PersonSummary, Views};
use super::{QueryParams, Shared};
use crate::error::AppResult;
use axum::{extract::State, routing::get, Json, Router};
use serde::{Deserialize, Serialize};

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/search", get(search))
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct SearchResult {
    items: Vec<ItemSummary>,
    people: Vec<PersonSummary>,
}

async fn search(State(st): State<Shared>, QueryParams(p): QueryParams<SearchParams>) -> AppResult<Json<SearchResult>> {
    let q = p.q.unwrap_or_default();
    if q.trim().is_empty() {
        return Ok(Json(SearchResult { items: vec![], people: vec![] }));
    }
    let limit = p.limit.unwrap_or(30).clamp(1, 200);
    Ok(Json(
        st.db
            .call(move |r| {
                let v = Views::new(r)?;
                let items = r.search_items(&q, limit)?;
                let people = r.search_people(&q, limit)?;
                Ok(SearchResult {
                    items: v.summaries(&items)?,
                    people: people.iter().map(|p| v.person_summary(p)).collect::<AppResult<_>>()?,
                })
            })
            .await?,
    ))
}
