//! Ayar sayfasının (`settings.html`) okuduğu, kapsam DIŞI alt sistemlerin dürüst yanıtları (W2.5, TP-W25-S1).
//!
//! `loadInfo()` tek `try` içinde `/api/lan` → `/api/update` → `/api/claude/hook` okur: biri 404 olunca sürüm / ayar dosyası /
//! günlük satırları hiç çizilmez (K5 eşlik denetimi). Wi-Fi dinleyici, otomatik güncelleme, Claude kancası, şarj anahtarı ve
//! telefon kaydı phoned'da YOK (PRD-W2 § Kapsam DIŞINDA) → 0.6.9 şekliyle "kapalı / yok" döner; değer uydurulmaz.
//! Yalnız GET: bu uçların POST eşleri (kur, uygula, sına) yoktur → 405/404 (yan etki yok).

use crate::app::{Shared, json_ok};
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use serde_json::{Value, json};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/lan", get(lan))
        .route("/api/update", get(update))
        .route("/api/claude/hook", get(hook))
        .route("/api/phones", get(phones))
        .route("/api/charge", get(charge))
}

/// `configured`/`port` ayardan; dinleyici yok → `active` false, adres yok (0.6.9 `/api/pair` de `enabled:false`).
async fn lan(State(st): State<Shared>) -> Response {
    let cfg = st.cfg.snapshot().0;
    let lan = cfg.get("lan");
    let configured = lan.and_then(|l| l.get("enabled")).and_then(Value::as_bool).unwrap_or(false);
    let port = lan.and_then(|l| l.get("port")).and_then(Value::as_i64).unwrap_or(8766);
    json_ok(&json!({"configured": configured, "active": false, "port": port, "restartNeeded": false, "hosts": [], "fingerprint": null}))
}

/// Güncelleme servisi yok: hiçbir zaman "güncelleme var" demez, denetim yapılmadı (`lastCheck` 0).
async fn update() -> Response {
    json_ok(&json!({
        "auto": false,
        "installed": false,
        "status": {
            "agentUpdate": false,
            "agentVersion": concat!(env!("CARGO_PKG_VERSION"), "-phoned"),
            "appUpdate": false,
            "appVersion": "",
            "error": null,
            "lastCheck": 0,
            "latest": "",
            "releaseUrl": "",
            "state": "idle",
        },
    }))
}

/// phoned Claude kancası kurmaz (README yasağı: kancayı panele kurdurma).
async fn hook() -> Response {
    json_ok(&json!({"installed": false, "path": ""}))
}

/// Ayardaki telefonlar; görülme kaydı tutulmaz → `model`/`lastSeen`/`via` null ("not seen since the agent started").
async fn phones(State(st): State<Shared>) -> Response {
    let cfg = st.cfg.snapshot().0;
    let list: Vec<Value> = cfg
        .get("phones")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let id = p.get("id").and_then(Value::as_str)?;
                    Some(json!({"id": id, "model": null, "lastSeen": null, "via": null, "name": p.get("name"), "pages": p.get("pages")}))
                })
                .collect()
        })
        .unwrap_or_default();
    json_ok(&Value::Array(list))
}

/// Şarj anahtarı yok, telefon pil raporu alınmıyor → 0.6.9'un "rapor yok" hâli.
async fn charge() -> Response {
    json_ok(&json!({"phone": null, "power": null, "switchedAt": null, "error": null}))
}
