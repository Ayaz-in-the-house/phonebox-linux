//! `GET /api/procs?top=N` · `GET /api/procs/detail?name=X` — 0.6.9 şekli. Tarama bloklayan iş havuzunda (SSE akışı durmaz).

use super::{Scan, detail, group};
use crate::app::Shared;
use crate::kill::{Policy, locked_set};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, serde::Deserialize)]
pub struct TopQuery {
    pub top: Option<i64>,
}

#[derive(Debug, serde::Deserialize)]
pub struct NameQuery {
    pub name: Option<String>,
}

fn json(v: &serde_json::Value) -> Response {
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")], v.to_string()).into_response()
}

async fn pull(st: &Shared) -> (Arc<Scan>, std::collections::HashSet<i32>) {
    let procs = Arc::clone(&st.extras.procs);
    let scan = tokio::task::spawn_blocking(move || procs.pull()).await.unwrap_or_default();
    let policy = Policy::load(&crate::kill::http::policy_path(st));
    let locked = locked_set(&scan.procs, st.extras.ctx, &policy.allow);
    (scan, locked)
}

/// 0.6.9 `Math.Clamp(top ?? 40, 1, 200)`.
#[must_use]
pub fn clamp_top(top: Option<i64>) -> usize {
    usize::try_from(top.unwrap_or(40).clamp(1, 200)).unwrap_or(40)
}

pub async fn list(State(st): State<Shared>, Query(q): Query<TopQuery>) -> Response {
    let (scan, locked) = pull(&st).await;
    let groups = group(&scan, |p| locked.contains(&p.pid));
    let total: usize = groups.iter().map(|g| g.count).sum();
    let top: Vec<_> = groups.iter().take(clamp_top(q.top)).collect();
    json(&json!({"total": total, "groupCount": groups.len(), "groups": top}))
}

pub async fn details(State(st): State<Shared>, Query(q): Query<NameQuery>) -> Response {
    let (scan, locked) = pull(&st).await;
    let d = detail(&scan, q.name.as_deref().unwrap_or(""), |p| locked.contains(&p.pid));
    json(&serde_json::to_value(&d).unwrap_or_default())
}
