//! `POST /api/volume?level=N | ?op=mute|up|down` (0.6.9 `Program.cs:461`): başarılıysa yeni `{volume, muted}`, değilse 400.

use crate::app::Shared;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

#[derive(Debug, serde::Deserialize)]
pub struct VolumeQuery {
    pub level: Option<i64>,
    pub op: Option<String>,
}

pub async fn volume(State(st): State<Shared>, Query(q): Query<VolumeQuery>) -> Response {
    let Some(op) = super::parse_op(q.level, q.op.as_deref()) else { return StatusCode::BAD_REQUEST.into_response() };
    let Some(s) = st.extras.audio.apply(op).await else { return StatusCode::BAD_REQUEST.into_response() };
    eprintln!("phoned: ses {op:?} → {}%{}", s.volume, if s.muted { " sessiz" } else { "" });
    ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], serde_json::to_string(&s).unwrap_or_default()).into_response()
}
