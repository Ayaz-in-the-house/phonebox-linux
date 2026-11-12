#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! K11 dikte (PRD-DT01, TP-DT-01..08): `POST /api/dictate` — telefonun 16 kHz WAV'ı → herdr-web motoru → `handy-smart-paste`.
//! Sahte herdr-web = geçici portta yerel axum sunucusu; sahte yapıştırıcı = geçici dizinde kabuk betiği (çağrı + argv kaydı).
//! Gerçek herdr-web'e, gerçek yapıştırıcıya, kullanıcının masaüstüne dokunulmaz.

mod common;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use common::{body_bytes, send, temp_dir};
use phoned::dictate::{self, Dictator, Settings, WavError};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TOKEN: &str = "tok-0123456789abcdef-gizli";
const SPOKEN: &str = "merhaba dünya $(touch /tmp/phoned-pwned) 'tırnak' \"çift\"";

/// `rate` Hz, `channels`, `bits` bit, `samples` örneklik PCM WAV.
fn wav(rate: u32, channels: u16, bits: u16, samples: usize) -> Vec<u8> {
    let data_len = u32::try_from(samples * usize::from(channels) * usize::from(bits / 8)).unwrap();
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits / 8)).to_le_bytes());
    v.extend_from_slice(&(channels * bits / 8).to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.resize(v.len() + data_len as usize, 0);
    v
}

fn good_wav() -> Vec<u8> {
    wav(16_000, 1, 16, 16_000)
}

#[derive(Clone, Debug, Default)]
struct Seen {
    auth: Option<String>,
    ctype: Option<String>,
    len: usize,
}

#[derive(Clone)]
enum Reply {
    Text(&'static str),
    Rejected,
    Status(u16),
}

#[derive(Clone)]
struct FakeState {
    reply: Reply,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn js(st: StatusCode, v: &Value) -> Response {
    (st, [(axum::http::header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

async fn fake_dictate(State(s): State<FakeState>, h: HeaderMap, body: Bytes) -> Response {
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
    s.seen.lock().unwrap().push(Seen { auth: get("authorization"), ctype: get("content-type"), len: body.len() });
    match s.reply {
        Reply::Text(t) => js(StatusCode::OK, &json!({"ok": true, "text": t, "language": "tr", "ms": 229, "telemetry": null})),
        Reply::Rejected => js(StatusCode::OK, &json!({"ok": true, "text": "", "language": null, "ms": 180, "rejected": "noise", "telemetry": null})),
        Reply::Status(c) => js(StatusCode::from_u16(c).unwrap(), &json!({"error": "motor düştü"})),
    }
}

/// Sahte herdr-web: `POST /api/dictate` kaydeder ve `reply` ile yanıtlar. Dönen: `http://127.0.0.1:<port>` + kayıt.
async fn fake_herdr(reply: Reply) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route("/api/dictate", post(fake_dictate)).with_state(FakeState { reply, seen: Arc::clone(&seen) });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

/// Sahte yapıştırıcı: her çağrıda `calls`'a `start` / `argc N` / `arg <metin>` / `end` yazar, arada 300 ms uyur.
fn fake_paste(dir: &Path, exit: i32) -> PathBuf {
    let p = dir.join("paste.sh");
    let log = dir.join("calls");
    std::fs::write(
        &p,
        format!("#!/bin/sh\necho start >> '{l}'\necho \"argc $#\" >> '{l}'\nprintf 'arg %s\\n' \"$1\" >> '{l}'\nsleep 0.3\necho end >> '{l}'\nexit {exit}\n", l = log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls")).map(|s| s.lines().map(str::to_owned).collect()).unwrap_or_default()
}

struct Rig {
    dir: common::TempDir,
    app: Router,
    lines: Arc<Mutex<Vec<String>>>,
}

/// `url` = sahte herdr-web adresi; adres dosyası + token dosyası geçici dizinde (herdr-web'in `$XDG_RUNTIME_DIR` düzeni).
fn rig(url: &str, paste_exit: i32) -> Rig {
    let dir = temp_dir();
    let d = dir.0.clone();
    std::fs::write(d.join("url"), url).unwrap();
    std::fs::write(d.join("token"), TOKEN).unwrap();
    let settings = Settings { url: None, url_file: d.join("url"), token_file: d.join("token"), paste: fake_paste(&d, paste_exit) };
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let dictator = Arc::new(Dictator::with_log(settings, Arc::new(move |l: &str| sink.lock().unwrap().push(l.to_owned()))));
    Rig { dir, app: dictate::http::routes(dictator), lines }
}

fn post_wav(path: &str, ctype: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder().method("POST").uri(path).header("host", "127.0.0.1:8766").header("content-type", ctype).body(Body::from(body)).unwrap()
}

async fn json_of(res: Response<Body>) -> (StatusCode, Value) {
    let st = res.status();
    let b = body_bytes(res).await;
    (st, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

// ── TP-DT-01 ana yol ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn tp_dt_01_forwards_with_token_and_pastes_once() {
    let (url, seen) = fake_herdr(Reply::Text(SPOKEN)).await;
    let r = rig(&url, 0);
    let body = good_wav();
    let n = body.len();
    let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", body)).await).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["text"], SPOKEN);
    assert_eq!(v["ms"], 229);
    assert_eq!(v["pasted"], true);
    let s = seen.lock().unwrap().clone();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
    assert_eq!(s[0].ctype.as_deref(), Some("audio/wav"));
    assert_eq!(s[0].len, n, "gövde birebir iletilir");
    // Tek çağrı, tek argüman, kabuk yorumu yok ($(...) çalışmadı, tırnaklar olduğu gibi).
    assert_eq!(calls(&r.dir.0), vec!["start".to_owned(), "argc 1".into(), format!("arg {SPOKEN}"), "end".into()]);
}

#[tokio::test]
async fn tp_dt_01_token_file_reread_each_request() {
    // herdr-web yeniden başlayınca token'ı yeniden üretir: phoned onu her istekte okur.
    let (url, seen) = fake_herdr(Reply::Text("bir")).await;
    let r = rig(&url, 0);
    assert_eq!(send(&r.app, post_wav("/api/dictate?paste=0", "audio/wav", good_wav())).await.status(), 200);
    std::fs::write(r.dir.0.join("token"), "yeni-token-42\n").unwrap();
    assert_eq!(send(&r.app, post_wav("/api/dictate?paste=0", "audio/wav", good_wav())).await.status(), 200);
    let s = seen.lock().unwrap().clone();
    assert_eq!(s[1].auth.as_deref(), Some("Bearer yeni-token-42"));
}

#[tokio::test]
async fn tp_dt_01_agent_prefix_and_guard_through_full_router() {
    // Gerçek yönlendirici: uç `/api` ve `/agent/api` altında bağlı; yabancı Host kapıda 403.
    let h = common::harness();
    let bad = post_wav("/api/dictate", "text/plain", b"x".to_vec());
    assert_eq!(send(&h.app, bad).await.status(), 415);
    let bad = post_wav("/agent/api/dictate", "text/plain", b"x".to_vec());
    assert_eq!(send(&h.app, bad).await.status(), 415);
    let foreign = Request::builder()
        .method("POST")
        .uri("/api/dictate")
        .header("host", "evil.example")
        .header("content-type", "audio/wav")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&h.app, foreign).await.status(), 403);
}

// ── TP-DT-02 biçim dışı ───────────────────────────────────────────────────────────

#[test]
fn tp_dt_02_check_wav_unit() {
    assert!((dictate::check_wav(&good_wav()).unwrap() - 1.0).abs() < 1e-9);
    assert!(matches!(dictate::check_wav(&wav(44_100, 1, 16, 100)), Err(WavError::Format(_))));
    assert!(matches!(dictate::check_wav(&wav(16_000, 2, 16, 100)), Err(WavError::Format(_))));
    assert!(matches!(dictate::check_wav(&wav(16_000, 1, 8, 100)), Err(WavError::Format(_))));
    assert!(matches!(dictate::check_wav(b"not a wav at all"), Err(WavError::Malformed(_))));
    assert!(matches!(dictate::check_wav(&wav(16_000, 1, 16, 0)), Err(WavError::Malformed(_))));
    assert!(matches!(dictate::check_wav(&[]), Err(WavError::Malformed(_))));
    // Geçerli fmt/data ama RIFF/WAVE başlığı yok.
    let mut rifx = good_wav();
    rifx[3] = b'X';
    assert!(matches!(dictate::check_wav(&rifx), Err(WavError::Malformed(_))));
    let mut wavx = good_wav();
    wavx[11] = b'X';
    assert!(matches!(dictate::check_wav(&wavx), Err(WavError::Malformed(_))));
    // fmt bölümü yok.
    let mut no_fmt = b"RIFF\0\0\0\0WAVEdata".to_vec();
    no_fmt.extend_from_slice(&4u32.to_le_bytes());
    no_fmt.extend_from_slice(&[0; 4]);
    assert!(matches!(dictate::check_wav(&no_fmt), Err(WavError::Malformed(_))));
    // Tek baytlık bölüm dolgusu (çift uzunluk) atlanır, ardındaki fmt/data okunur.
    let g = good_wav();
    let mut padded = g[..12].to_vec();
    padded.extend_from_slice(b"LIST");
    padded.extend_from_slice(&1u32.to_le_bytes());
    padded.extend_from_slice(&[7, 0]);
    padded.extend_from_slice(&g[12..]);
    assert!(dictate::check_wav(&padded).is_ok());
}

#[tokio::test]
async fn tp_dt_02_rejects_without_engine_or_paste() {
    let (url, seen) = fake_herdr(Reply::Text("olmamalı")).await;
    let r = rig(&url, 0);
    let cases: Vec<(&str, Vec<u8>, u16)> = vec![
        ("text/plain", good_wav(), 415),
        ("application/octet-stream", good_wav(), 415),
        ("audio/wav", wav(48_000, 1, 16, 1000), 415),
        ("audio/wav", wav(16_000, 2, 16, 1000), 415),
        ("audio/wav", Vec::new(), 400),
        ("audio/wav", b"RIFFxxxxJUNK-bozuk".to_vec(), 400),
        ("audio/wav", wav(16_000, 1, 16, 0), 400),
    ];
    for (ct, body, want) in cases {
        let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", ct, body)).await).await;
        assert_eq!(st.as_u16(), want, "{ct}: {v}");
        assert!(v["error"].is_string(), "okunur hata: {v}");
    }
    // `audio/wav; charset` biçimi ve `audio/x-wav` kabul.
    let (st, _) = json_of(send(&r.app, post_wav("/api/dictate?paste=0", "Audio/WAV; codecs=1", good_wav())).await).await;
    assert_eq!(st, 200);
    let (st, _) = json_of(send(&r.app, post_wav("/api/dictate?paste=0", "audio/x-wav", good_wav())).await).await;
    assert_eq!(st, 200);
    assert_eq!(seen.lock().unwrap().len(), 2, "yalnız geçerli ikisi motora gitti");
    assert!(calls(&r.dir.0).is_empty());
    // Bilinmeyen paste değeri 400.
    assert_eq!(send(&r.app, post_wav("/api/dictate?paste=evet", "audio/wav", good_wav())).await.status(), 400);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

// ── TP-DT-03 boyut tavanı ─────────────────────────────────────────────────────────

#[tokio::test]
async fn tp_dt_03_over_2mb_is_413() {
    let (url, seen) = fake_herdr(Reply::Text("olmamalı")).await;
    let r = rig(&url, 0);
    let mut big = good_wav();
    big.resize(dictate::MAX_BYTES + 1, 0);
    let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", big)).await).await;
    assert_eq!(st, 413, "{v}");
    assert_eq!(dictate::MAX_BYTES, 2 * 1024 * 1024);
    // Tam sınır kabul (≈ 65 sn).
    let at = wav(16_000, 1, 16, (dictate::MAX_BYTES - 44) / 2);
    assert_eq!(at.len(), dictate::MAX_BYTES);
    assert_eq!(send(&r.app, post_wav("/api/dictate?paste=0", "audio/wav", at)).await.status(), 200);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(calls(&r.dir.0).is_empty());
}

// ── TP-DT-04 herdr-web hatası ─────────────────────────────────────────────────────

#[tokio::test]
async fn tp_dt_04_engine_error_is_502_no_paste() {
    let (url, _) = fake_herdr(Reply::Status(500)).await;
    let r = rig(&url, 0);
    let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await).await;
    assert_eq!(st, 502, "{v}");
    assert!(v["error"].as_str().unwrap().contains("500"), "{v}");
    assert!(calls(&r.dir.0).is_empty());
}

#[tokio::test]
async fn tp_dt_04_engine_down_is_502() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let r = rig(&format!("http://127.0.0.1:{port}"), 0);
    let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await).await;
    assert_eq!(st, 502, "{v}");
    assert!(v["error"].is_string());
    assert!(calls(&r.dir.0).is_empty());
}

#[tokio::test]
async fn tp_dt_04_missing_token_or_foreign_url_is_502_without_sending() {
    let (url, seen) = fake_herdr(Reply::Text("olmamalı")).await;
    let r = rig(&url, 0);
    std::fs::remove_file(r.dir.0.join("token")).unwrap();
    let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await).await;
    assert_eq!(st, 502, "{v}");
    assert!(seen.lock().unwrap().is_empty(), "token yokken istek gitmez");
    // Token yalnız loopback'e gider: başka adres reddedilir (bağlanmadan).
    std::fs::write(r.dir.0.join("token"), TOKEN).unwrap();
    std::fs::write(r.dir.0.join("url"), "http://10.0.0.1:8791").unwrap();
    let (st, _) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await).await;
    assert_eq!(st, 502);
    std::fs::write(r.dir.0.join("url"), "https://127.0.0.1:8791").unwrap();
    assert_eq!(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await.status(), 502);
    assert!(seen.lock().unwrap().is_empty());
    assert!(calls(&r.dir.0).is_empty());
}

#[test]
fn tp_dt_04_parse_url_unit() {
    assert_eq!(dictate::loopback_addr("http://127.0.0.1:8791").unwrap().to_string(), "127.0.0.1:8791");
    assert_eq!(dictate::loopback_addr("http://localhost:8791/\n").unwrap().to_string(), "127.0.0.1:8791");
    assert!(dictate::loopback_addr("http://127.0.0.1").is_err());
    assert!(dictate::loopback_addr("http://127.0.0.2:80").is_err());
    assert!(dictate::loopback_addr("http://example.com:80").is_err());
    assert!(dictate::loopback_addr("http://127.0.0.1:99999").is_err());
}

// ── TP-DT-05 boş metin / uydurma kapısı ───────────────────────────────────────────

#[tokio::test]
async fn tp_dt_05_empty_or_rejected_text_is_not_pasted() {
    for reply in [Reply::Text(""), Reply::Text("   \n "), Reply::Rejected] {
        let (url, seen) = fake_herdr(reply).await;
        let r = rig(&url, 0);
        let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["text"], "");
        assert_eq!(v["pasted"], false);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(calls(&r.dir.0).is_empty());
    }
}

// ── TP-DT-06 paste=0 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn tp_dt_06_paste_0_returns_text_only() {
    let (url, _) = fake_herdr(Reply::Text("önizleme")).await;
    let r = rig(&url, 0);
    let (st, v) = json_of(send(&r.app, post_wav("/api/dictate?paste=0", "audio/wav", good_wav())).await).await;
    assert_eq!(st, 200);
    assert_eq!(v["text"], "önizleme");
    assert_eq!(v["pasted"], false);
    assert!(calls(&r.dir.0).is_empty());
    // paste=1 açıkça: yapıştırır.
    let (_, v) = json_of(send(&r.app, post_wav("/api/dictate?paste=1", "audio/wav", good_wav())).await).await;
    assert_eq!(v["pasted"], true);
    assert_eq!(calls(&r.dir.0).iter().filter(|l| *l == "start").count(), 1);
}

#[tokio::test]
async fn tp_dt_06_paste_failure_is_reported() {
    let (url, _) = fake_herdr(Reply::Text("yazılamadı")).await;
    let r = rig(&url, 3);
    let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await).await;
    assert_eq!(st, 200);
    assert_eq!(v["text"], "yazılamadı");
    assert_eq!(v["pasted"], false);
    assert!(v["pasteError"].as_str().unwrap().contains('3'), "{v}");
}

// ── TP-DT-07 eşzamanlılık ─────────────────────────────────────────────────────────

#[tokio::test]
async fn tp_dt_07_concurrent_requests_queue_and_pastes_do_not_interleave() {
    let (url, seen) = fake_herdr(Reply::Text("sırada")).await;
    let r = rig(&url, 0);
    let a = send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav()));
    let b = send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav()));
    let c = send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav()));
    let (ra, rb, rc) = tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(a, b, c) }).await.unwrap();
    for res in [ra, rb, rc] {
        assert_eq!(res.status(), 200);
    }
    assert_eq!(seen.lock().unwrap().len(), 3);
    let log: Vec<String> = calls(&r.dir.0).into_iter().filter(|l| l == "start" || l == "end").collect();
    assert_eq!(log, ["start", "end", "start", "end", "start", "end"], "yapıştırmalar iç içe geçmez");
}

#[tokio::test]
async fn tp_dt_07_queue_has_a_ceiling() {
    let (url, _) = fake_herdr(Reply::Text("dolu")).await;
    let r = rig(&url, 0);
    let reqs: Vec<_> = (0..dictate::MAX_QUEUE + 2).map(|_| send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav()))).collect();
    let all = tokio::time::timeout(Duration::from_secs(15), futures_util::future::join_all(reqs)).await.unwrap();
    let busy = all.iter().filter(|r| r.status() == 429).count();
    let ok = all.iter().filter(|r| r.status() == 200).count();
    assert!(busy >= 1, "tavan aşınca 429");
    assert_eq!(ok + busy, all.len());
    assert!(ok >= dictate::MAX_QUEUE, "tavana kadar sıraya girer: ok={ok}");
}

// ── TP-DT-08 günlükte içerik yok ──────────────────────────────────────────────────

#[tokio::test]
async fn tp_dt_08_log_has_sizes_not_content() {
    let (url, _) = fake_herdr(Reply::Text(SPOKEN)).await;
    let r = rig(&url, 0);
    let body = good_wav();
    let n = body.len();
    assert_eq!(send(&r.app, post_wav("/api/dictate", "audio/wav", body)).await.status(), 200);
    assert_eq!(send(&r.app, post_wav("/api/dictate", "text/plain", b"gizli-icerik".to_vec())).await.status(), 415);
    let (_, _) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await).await;
    let lines = r.lines.lock().unwrap().clone();
    assert!(lines.len() >= 3, "{lines:?}");
    assert!(lines.iter().any(|l| l.contains(&format!("{n} B"))), "uzunluk yazılır: {lines:?}");
    assert!(lines.iter().any(|l| l.contains(&format!("{} krk", SPOKEN.chars().count()))), "karakter sayısı yazılır: {lines:?}");
    for l in &lines {
        assert!(!l.contains("merhaba") && !l.contains("tırnak"), "metin günlükte: {l}");
        assert!(!l.contains(TOKEN), "token günlükte: {l}");
        assert!(!l.contains("gizli-icerik"), "gövde günlükte: {l}");
    }
}

#[tokio::test]
async fn tp_dt_08_engine_error_text_not_logged_with_token() {
    let (url, _) = fake_herdr(Reply::Status(503)).await;
    let r = rig(&url, 0);
    let (st, v) = json_of(send(&r.app, post_wav("/api/dictate", "audio/wav", good_wav())).await).await;
    assert_eq!(st, 502);
    assert!(!v.to_string().contains(TOKEN));
    for l in r.lines.lock().unwrap().iter() {
        assert!(!l.contains(TOKEN), "{l}");
    }
}
