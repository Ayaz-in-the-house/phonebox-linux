//! `POST /api/dictate[?paste=0|1]` — gövde 16 kHz mono 16 bit PCM WAV (`Content-Type: audio/wav`).
//! Yanıt 200 `{text, ms, pasted[, pasteError]}` · 400 bozuk/boş/bilinmeyen `paste` · 413 > 2 MiB · 415 biçim/tür dışı ·
//! 429 kuyruk dolu · 502 herdr-web hatası/erişilemez. Hata gövdesi `{error}`; günlükte içerik yok.

use super::{Dictator, MAX_BYTES, Outcome, WavError};
use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, serde::Deserialize)]
pub struct DictateQuery {
    pub paste: Option<String>,
}

/// Uç, kendi durumuyla: her yönlendiriciye (`Router<S>`) bağlanabilir.
pub fn routes<S: Clone + Send + Sync + 'static>(d: Arc<Dictator>) -> Router<S> {
    Router::new().route("/api/dictate", post(dictate)).with_state(d)
}

fn reply(status: StatusCode, v: &serde_json::Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json; charset=utf-8")], v.to_string()).into_response()
}

fn wav_type(h: &HeaderMap) -> bool {
    let ct = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let base = ct.split(';').next().unwrap_or("").trim();
    ["audio/wav", "audio/x-wav", "audio/wave"].iter().any(|t| base.eq_ignore_ascii_case(t))
}

async fn dictate(State(d): State<Arc<Dictator>>, h: HeaderMap, Query(q): Query<DictateQuery>, body: Body) -> Response {
    let t = Instant::now();
    let fail = |status: StatusCode, why: &str, n: usize| {
        d.log(&format!("phoned: dikte {n} B → {} ({why})", status.as_u16()));
        reply(status, &json!({ "error": why }))
    };
    let paste = match q.paste.as_deref() {
        None | Some("1") => true,
        Some("0") => false,
        Some(_) => return fail(StatusCode::BAD_REQUEST, "paste yalnız 0 ya da 1", 0),
    };
    if !wav_type(&h) {
        return fail(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Content-Type audio/wav bekleniyor", 0);
    }
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BYTES).await else {
        return fail(StatusCode::PAYLOAD_TOO_LARGE, "kayıt çok büyük (en çok 2 MiB)", MAX_BYTES);
    };
    let n = bytes.len();
    let secs = match super::check_wav(&bytes) {
        Ok(s) => s,
        Err(WavError::Malformed(why)) => return fail(StatusCode::BAD_REQUEST, &why, n),
        Err(WavError::Format(why)) => return fail(StatusCode::UNSUPPORTED_MEDIA_TYPE, &why, n),
    };
    match d.run(&bytes, paste).await {
        Outcome::Done { text, ms, pasted, paste_error } => {
            d.log(&format!(
                "phoned: dikte {n} B {secs:.1} sn → {} krk motor {ms} ms toplam {} ms yapıştırma={}",
                text.chars().count(),
                t.elapsed().as_millis(),
                if pasted {
                    "evet"
                } else if paste_error.is_some() {
                    "HATA"
                } else {
                    "hayır"
                }
            ));
            let v = match paste_error {
                Some(e) => json!({ "text": text, "ms": ms, "pasted": pasted, "pasteError": e }),
                None => json!({ "text": text, "ms": ms, "pasted": pasted }),
            };
            reply(StatusCode::OK, &v)
        }
        Outcome::Busy => fail(StatusCode::TOO_MANY_REQUESTS, "dikte meşgul — biraz sonra yeniden dene", n),
        Outcome::Engine(why) => fail(StatusCode::BAD_GATEWAY, &why, n),
    }
}
