//! Request metadata only: never request bodies, cookies, bearer secrets or public tokens.
use crate::{
    error::{Error, Result},
    service::Drawer,
};
use axum::{
    Json,
    extract::{MatchedPath, Query, Request, State},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
#[derive(Deserialize)]
pub struct Page {
    before: Option<i64>,
    limit: Option<i64>,
}
#[derive(Serialize, sqlx::FromRow)]
pub struct Event {
    id: i64,
    actor: String,
    method: String,
    route: String,
    status: i64,
    created_at: String,
}
pub async fn list(State(s): State<Drawer>, Query(q): Query<Page>) -> Result<Json<Vec<Event>>> {
    let limit = q.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(Error::Invalid("limit must be 1 to 200".into()));
    }
    Ok(Json(sqlx::query_as("SELECT id,actor,method,route,status,created_at FROM activity WHERE id < ? ORDER BY id DESC LIMIT ?").bind(q.before.unwrap_or(i64::MAX)).bind(limit).fetch_all(&s.pool).await?))
}
pub async fn record(State(s): State<Drawer>, req: Request, next: Next) -> Response {
    let method = req.method().to_string();
    // MatchedPath stores route templates, never the secret in /s/{token}.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".into());
    let relevant_method = method != "GET"
        || req.uri().path().ends_with("/files")
        || req.uri().path().ends_with("/archive")
        || req.uri().path().starts_with("/s/");
    let tracked = relevant_method
        && ((req.uri().path().starts_with("/drawer/v1")
            && !req.uri().path().starts_with("/drawer/v1/activity"))
            || req.uri().path() == "/auth/login"
            || req.uri().path().starts_with("/s/")
            || req.uri().path() == "/mcp"
            || req.uri().path().starts_with("/drop/"));
    let actor = if req.uri().path() == "/auth/login"
        || req.uri().path().starts_with("/s/")
        || req.uri().path().starts_with("/drop/")
    {
        "anonymous".to_owned()
    } else {
        req.headers()
            .get("x-drawer-author")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("api")
            .chars()
            .take(100)
            .collect()
    };
    let response = next.run(req).await;
    if tracked && let Err(error) = sqlx::query("INSERT INTO activity(actor,method,route,status,created_at) VALUES(?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))").bind(actor).bind(method).bind(route).bind(i64::from(response.status().as_u16())).execute(&s.pool).await {
            tracing::warn!(%error, "could not persist activity event");
    }
    response
}
