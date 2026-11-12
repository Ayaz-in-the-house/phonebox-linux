//! `POST /api/media/{action}?ms=` · `GET /api/art`. (`POST /api/volume` W2.4b'de `audio::http`.)

use crate::app::Shared;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

#[derive(Debug, serde::Deserialize)]
pub struct SeekQuery {
    pub ms: Option<i64>,
}

/// K8: gösterilen kaynak YouTube ise denetim eklentiye (deckd Zen soketi), değilse MPRIS. `skip?ms=±N` göreli.
pub async fn control(State(st): State<Shared>, Path(action): Path<String>, Query(q): Query<SeekQuery>) -> Response {
    let media = &st.extras.media;
    let ok = if media.snapshot().is_some_and(|s| s.source == "yt") {
        match super::yt::control(&super::yt::runtime("deckd-zen.sock"), &action, q.ms).await {
            Ok(_) => true,
            Err(e) => {
                eprintln!("phoned: YouTube {action}: {e}");
                false
            }
        }
    } else {
        super::dbus::control(media, &action, q.ms).await
    };
    if ok { StatusCode::OK.into_response() } else { StatusCode::BAD_REQUEST.into_response() }
}

#[derive(Debug, serde::Deserialize)]
pub struct ArtQuery {
    pub id: Option<u64>,
}

pub async fn art(State(st): State<Shared>, Query(q): Query<ArtQuery>) -> Response {
    // K8: YouTube görselleri bellekte (eklenti 128×72 JPEG); kimlik = görsel anahtarı.
    if let Some(b) = q.id.and_then(|id| st.extras.media.yt_art(id)) {
        return ([(header::CONTENT_TYPE, "image/jpeg")], b).into_response();
    }
    let Some(path) = st.extras.media.art_file() else { return StatusCode::NOT_FOUND.into_response() };
    let ok = tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_file() && m.len() <= super::ART_MAX);
    if !ok {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(&path).await {
        Ok(b) => ([(header::CONTENT_TYPE, super::art_mime(&path))], b).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
