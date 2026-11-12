#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! TP-W2-02 (güvenlik) · TP-W2-03 (ayar yazımı) · TP-W2-P1/P2 (statik) · akış.

mod common;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{body_bytes, get, get_json, harness, send};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt as _;

fn with(path: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::builder().uri(path);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::empty()).unwrap()
}

type GuardCase = (&'static [(&'static str, &'static str)], &'static str, u16);

#[tokio::test]
async fn tp_w2_02_guard_over_http() {
    let h = harness();
    let cases: &[GuardCase] = &[
        (&[("host", "127.0.0.1:8766")], "/api/pair", 200),
        (&[("host", "localhost:8766")], "/api/pair", 200),
        (&[("host", "evil.example")], "/api/pair", 403),
        (&[("host", "127.0.0.1.evil:8766")], "/api/pair", 403),
        (&[("host", "127.0.0.1:8766"), ("origin", "http://evil.example")], "/api/pair", 403),
        (&[("host", "127.0.0.1:8766"), ("origin", "http://127.0.0.1:9999")], "/api/pair", 403),
        (&[("host", "127.0.0.1:8766"), ("origin", "http://127.0.0.1:8766")], "/api/pair", 200),
        (&[("host", "127.0.0.1:8766"), ("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "cors")], "/api/pair", 403),
        (&[("host", "127.0.0.1:8766"), ("sec-fetch-site", "same-site"), ("sec-fetch-mode", "no-cors")], "/api/state", 403),
        (&[("host", "127.0.0.1:8766"), ("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "navigate")], "/agent/api/pair", 403),
        (&[("host", "127.0.0.1:8766"), ("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "navigate")], "/", 200),
        (&[("host", "127.0.0.1:8766"), ("sec-fetch-site", "same-origin")], "/api/pair", 200),
        (&[("host", "evil.example")], "/nope", 403),
    ];
    for (hdrs, path, want) in cases {
        let res = send(&h.app, with(path, hdrs)).await;
        assert_eq!(res.status().as_u16(), *want, "{path} {hdrs:?}");
    }
}

#[tokio::test]
async fn tp_w2_02e_headers() {
    let h = harness();
    let api = send(&h.app, get("/api/pair")).await;
    assert_eq!(api.headers()["cache-control"], "no-store");
    assert_eq!(api.headers()["x-frame-options"], "DENY");
    let panel = send(&h.app, get("/panel/main.js")).await;
    assert_eq!(panel.headers()["x-frame-options"], "SAMEORIGIN");
    assert_eq!(panel.headers()["cache-control"], "no-store");
}

fn put(body: &str, ctype: &str, version: Option<i64>) -> Request<Body> {
    let mut b = Request::builder().method("PUT").uri("/api/settings").header("host", "127.0.0.1:8766").header("content-type", ctype);
    if let Some(v) = version {
        b = b.header("x-config-version", v.to_string());
    }
    b.body(Body::from(body.to_owned())).unwrap()
}

async fn settings(h: &common::Harness) -> (Value, i64) {
    let res = send(&h.app, get("/api/settings")).await;
    assert_eq!(res.status(), 200);
    let v: i64 = res.headers()["x-config-version"].to_str().unwrap().parse().unwrap();
    (serde_json::from_slice(&body_bytes(res).await).unwrap(), v)
}

#[tokio::test]
async fn tp_w2_03f_settings_version_matches_config() {
    let h = harness();
    let (body, v) = settings(&h).await;
    assert_eq!(body["lan"]["token"], "test-lan-token");
    let cfg = get_json(&h.app, "/api/config").await;
    assert_eq!(cfg["version"], json!(v));
    assert!(v > 621_355_968_000_000_000, ".NET tick biçimi");
}

#[tokio::test]
async fn tp_w2_03ab_conflict_then_atomic_600() {
    let h = harness();
    let path = h.config_path();
    let (mut body, v) = settings(&h).await;
    let before = std::fs::read(&path).unwrap();
    body["weather"]["city"] = json!("Yenikent");
    let res = send(&h.app, put(&body.to_string(), "application/json", Some(v - 1))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let err: Value = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert!(err["error"].as_str().unwrap().contains("changed in another tab"));
    assert_eq!(std::fs::read(&path).unwrap(), before, "409'da dosya değişmez");

    std::thread::sleep(std::time::Duration::from_millis(5));
    let res = send(&h.app, put(&body.to_string(), "application/json; charset=utf-8", Some(v))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let ok: Value = serde_json::from_slice(&body_bytes(res).await).unwrap();
    let v2 = ok["version"].as_i64().unwrap();
    assert_ne!(v2, v);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let bak = path.with_file_name("config.json.bak");
    assert_eq!(std::fs::read(&bak).unwrap(), before, ".bak = önceki içerik");
    assert_eq!(std::fs::metadata(&bak).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(!path.with_file_name("config.json.tmp").exists());
    let (again, v3) = settings(&h).await;
    assert_eq!(again["weather"]["city"], "Yenikent");
    assert_eq!(v3, v2);
    // eski sürümle tekrar → 409
    let res = send(&h.app, put(&body.to_string(), "application/json", Some(v))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn tp_w2_03c_invalid_bodies_rejected() {
    let h = harness();
    let path = h.config_path();
    let before = std::fs::read(&path).unwrap();
    let (body, _) = settings(&h).await;
    let mut no_cmd = body.clone();
    no_cmd["shortcuts"][1]["cmd"] = json!("  ");
    let mut lat = body.clone();
    lat["weather"]["lat"] = json!(91.0);
    let mut night = body.clone();
    night["night"]["start"] = json!("25:00");
    for (b, needle) in [
        ("{bozuk".to_owned(), "Invalid JSON"),
        (no_cmd.to_string(), "has no command"),
        (lat.to_string(), "Invalid weather location"),
        (night.to_string(), "HH:MM"),
        ("[1]".to_owned(), "Invalid JSON"),
    ] {
        let res = send(&h.app, put(&b, "application/json", None)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{needle}");
        let err: Value = serde_json::from_slice(&body_bytes(res).await).unwrap();
        assert!(err["error"].as_str().unwrap().contains(needle), "{err}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn tp_w2_03de_content_type_and_no_version() {
    let h = harness();
    let (body, _) = settings(&h).await;
    let res = send(&h.app, put(&body.to_string(), "text/plain", None)).await;
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let res = send(&h.app, put(&body.to_string(), "application/json", None)).await;
    assert_eq!(res.status(), StatusCode::OK, "sürüm başlığı yoksa kayıt yapılır");
}

#[tokio::test]
async fn tp_w2_03g_public_config_hides_commands() {
    let h = harness();
    let cfg = get_json(&h.app, "/api/config").await;
    let text = cfg.to_string();
    assert!(!text.contains("secret-cmd") && !text.contains("secret-unix"), "komut widget betiği sızdı");
    assert!(!text.contains("test-lan-token") && !text.contains("test-webhook-token"));
    assert!(!text.contains("echo test"), "kısayol komutu sızdı");
    let c = &cfg["config"];
    assert!(c.get("lan").is_none());
    let sc = &c["shortcuts"][0];
    assert_eq!(sc["toggle"], json!(true), "builtin:mute → toggle");
    assert!(sc.get("cmd").is_none());
    let cmd = c["pages"][0]["widgets"].as_array().unwrap().iter().find(|w| w["type"] == "command").unwrap();
    assert_eq!(cmd["options"], json!({"title": "T", "interval": 5}));
}

#[tokio::test]
async fn tp_w2_p1_panel_static() {
    let h = harness();
    let r = send(&h.app, get("/panel?x=1")).await;
    assert_eq!(r.status(), StatusCode::FOUND);
    assert_eq!(r.headers()["location"], "/panel/?x=1");
    let r = send(&h.app, get("/panel/")).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/html; charset=utf-8");
    assert_eq!(body_bytes(r).await, b"<html>panel</html>");
    let r = send(&h.app, get("/panel/main.js")).await;
    assert_eq!(r.headers()["content-type"], "text/javascript; charset=utf-8");
    let r = send(&h.app, get("/panel/style.css")).await;
    assert_eq!(r.headers()["content-type"], "text/css; charset=utf-8");
    assert_eq!(send(&h.app, get("/panel/widgets/clock.js")).await.status(), 200);
    for bad in
        ["/panel/../secret.txt", "/panel/%2e%2e/secret.txt", "/panel/a/b/c.js", "/panel/..", "/panel/widgets/../../secret.txt", "/panel/yok.js"]
    {
        assert_eq!(send(&h.app, get(bad)).await.status(), 404, "{bad}");
    }
}

#[tokio::test]
async fn tp_w2_p2_settings_page() {
    let h = harness();
    let r = send(&h.app, get("/")).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/html; charset=utf-8");
    assert_eq!(body_bytes(r).await, b"<html>settings</html>");
}

#[tokio::test]
async fn stream_starts_with_work_then_state() {
    let h = harness();
    let res = send(&h.app, get("/api/stream")).await;
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    assert_eq!(res.headers()["x-accel-buffering"], "no");
    assert_eq!(h.st.hub.subscribers(), 1);
    let mut body = res.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(first.starts_with(b"event: work\ndata: {"));
    let second = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let text = std::str::from_utf8(&second).unwrap();
    assert!(text.starts_with("event: state\ndata: {"));
    let v: Value = serde_json::from_str(text.trim_start_matches("event: state\ndata: ").trim_end()).unwrap();
    assert!(v["pc"]["swapUsedGb"].is_number());
    drop(body);
    assert_eq!(h.st.hub.subscribers(), 0, "bağlantı kapanınca abonelik düşer");
}

#[tokio::test]
async fn tp_w2_s3_debug_topics_reports_lock_and_mode() {
    let h = harness();
    let t = get_json(&h.app, "/api/debug/topics").await;
    assert_eq!(t["locked"], json!(false));
    assert_eq!(t["pc"]["runs"], json!(2));
    h.locked.send_replace(true);
    let t = get_json(&h.app, "/api/debug/topics").await;
    assert_eq!(t["locked"], json!(true));
    assert_eq!(t["mode"], "quiet");
}
