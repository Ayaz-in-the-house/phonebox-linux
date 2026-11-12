//! Panel eylemleri: `push` · `notify` · `goto` · `refresh` · `run/{id}` (0.6.9 `Program.cs:240-275,502` sözleşmesi) +
//! yerleşik herdr yoklayıcısı ve adb izleyicisinin bağlantıları.
//!
//! Katman: `app` (yönlendirici, kapı) → `actions` (uç + bağlama) → `panel` (depo, saf) · `run` (kısayol kararı) ·
//! `herdr` · `zen` · `adb` (G/Ç istemcileri). Kim yazar: herdr görünümünü yalnız `herdr_loop`, olayları uçlar ve
//! `herdr_loop`; kim okur: `/api/state`, `/api/push`, akış.

pub mod panel;
pub mod run;

use crate::adb;
use crate::app::{Shared, looking};
use crate::herdr;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use panel::Panel;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const JSON: &str = "application/json; charset=utf-8";

/// Dış uçlar (komut satırı + ortamdan; testte geçici soketler).
#[derive(Debug, Clone)]
pub struct Links {
    pub herdr_sock: PathBuf,
    pub zen_sock: PathBuf,
    /// herdr-web kökü (`…/herdr/`).
    pub herdr_url: String,
    pub adb: adb::Settings,
    pub home: String,
}

impl Links {
    /// Varsayılanlar: herdr `$HERDR_SOCKET_PATH`, Zen `$XDG_RUNTIME_DIR/deckd-zen.sock`, `$HERDR_URL`, adb 127.0.0.1:5037.
    #[must_use]
    pub fn from_env(port: u16) -> Self {
        Self {
            herdr_sock: herdr::sock_path(),
            zen_sock: crate::zen::sock_path(),
            herdr_url: std::env::var("HERDR_URL").unwrap_or_else(|_| "https://workstation.example-tailnet.ts.net/herdr/".to_owned()),
            adb: adb::Settings {
                addr: Some(adb::DEFAULT_ADDR.to_owned()),
                packages: vec![adb::DEFAULT_PACKAGE.to_owned()],
                phone_port: port,
                agent_port: port,
                open_panel: true,
            },
            home: std::env::var("HOME").unwrap_or_default(),
        }
    }
}

/// Eylem katmanının durumu (`AppState.ext`).
pub struct Ext {
    pub panel: Arc<Panel>,
    pub links: Links,
    pub herdr: Mutex<herdr::Tracker>,
    /// Goals yarısının kayıt okuyucusu (TP-AP-27); G/Ç `spawn_blocking`'te.
    pub goals: Arc<Mutex<herdr::goals::Goals>>,
    pub adb: adb::Shared,
    /// herdr'a giden istek sayısı (TP-W2-08b; `/api/debug/topics`).
    pub herdr_polls: AtomicU64,
}

impl Ext {
    #[must_use]
    pub fn new(data_path: PathBuf, links: Links) -> Self {
        Self {
            panel: Arc::new(Panel::open(data_path, unix_ms())),
            links,
            herdr: Mutex::new(herdr::Tracker::default()),
            goals: Arc::new(Mutex::new(herdr::goals::Goals::from_env())),
            adb: adb::Shared::default(),
            herdr_polls: AtomicU64::new(0),
        }
    }
}

#[must_use]
pub fn unix_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn unix_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn json_ok(v: &Value) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, JSON)], v.to_string()).into_response()
}

fn bad(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, JSON)], json!({"error": msg}).to_string()).into_response()
}

/// ASP.NET `Results.Problem(detail)` şekli.
fn problem(detail: &str) -> Response {
    let body = json!({
        "type": "https://tools.ietf.org/html/rfc9110#section-15.6.1",
        "title": "An error occurred while processing your request.",
        "status": 500,
        "detail": detail,
    });
    (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "application/problem+json")], body.to_string()).into_response()
}

type Q = Query<HashMap<String, String>>;

fn ctype(h: &HeaderMap) -> Option<&str> {
    h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok())
}

async fn push_list(State(st): State<Shared>) -> Response {
    let p = &st.ext.panel;
    let now = unix_ms();
    json_ok(&json!({"version": p.version(now), "items": p.snapshot(now)}))
}

async fn push_get(State(st): State<Shared>, AxPath(key): AxPath<String>) -> Response {
    match st.ext.panel.get(&key, unix_ms()) {
        Some(item) => json_ok(&json!(item)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn push_set(State(st): State<Shared>, AxPath(key): AxPath<String>, Query(q): Q, h: HeaderMap, body: Bytes) -> Response {
    let data = match panel::read_body(q.get("value").map(String::as_str), ctype(&h), &body) {
        Ok(d) => d,
        Err(e) => return bad(&e),
    };
    let ttl = q.get("ttl").and_then(|t| t.parse::<i64>().ok());
    match st.ext.panel.set(&key, data, ttl, unix_ms()) {
        Ok(version) => json_ok(&json!({"ok": true, "key": key, "version": version})),
        Err(e) => bad(&e),
    }
}

async fn push_del(State(st): State<Shared>, AxPath(key): AxPath<String>) -> Response {
    if st.ext.panel.remove(&key, unix_ms()) { StatusCode::OK.into_response() } else { StatusCode::NOT_FOUND.into_response() }
}

async fn push_clear(State(st): State<Shared>) -> Response {
    st.ext.panel.clear(unix_ms());
    StatusCode::OK.into_response()
}

async fn notify(State(st): State<Shared>, Query(q): Q, h: HeaderMap, body: Bytes) -> Response {
    let d = match panel::read_body(q.get("value").map(String::as_str), ctype(&h), &body) {
        Ok(d) => d,
        Err(e) => return bad(&e),
    };
    let payload = if d.is_object() {
        d
    } else {
        let text = d.as_str().map_or_else(|| d.to_string(), str::to_owned);
        json!({"text": text})
    };
    if payload.get("text").is_none() && payload.get("title").is_none() {
        return bad("give a text or a title");
    }
    json_ok(&json!(st.ext.panel.emit("notify", Some(payload), unix_ms())))
}

async fn goto(State(st): State<Shared>, AxPath(page): AxPath<String>, Query(q): Q) -> Response {
    let countdown = q.get("countdown").and_then(|c| c.parse::<i64>().ok()).map(|c| c.clamp(0, 30));
    json_ok(&json!(st.ext.panel.emit("goto", Some(json!({"page": page, "countdown": countdown})), unix_ms())))
}

async fn refresh(State(st): State<Shared>, Query(q): Q) -> Response {
    let reload = q.get("reload").is_some_and(|r| matches!(r.as_str(), "1" | "true" | "True"));
    json_ok(&json!(st.ext.panel.emit(if reload { "reload" } else { "refresh" }, None, unix_ms())))
}

fn shortcut(cfg: &Value, id: &str) -> Option<Value> {
    cfg.get("shortcuts")?.as_array()?.iter().find(|s| s.get("id").and_then(Value::as_str) == Some(id)).cloned()
}

async fn run_shortcut(State(st): State<Shared>, AxPath(id): AxPath<String>) -> Response {
    let (cfg, _) = st.cfg.snapshot();
    let Some(sc) = shortcut(&cfg, &id) else { return problem(&format!("no such shortcut: {id}")) };
    let t0 = Instant::now();
    let recv_ms = unix_ms();
    let r = match run::plan(&sc, &st.ext.links.home) {
        run::Action::HerdrGo(n) => herdr_go(&st, n, t0).await,
        run::Action::Spawn { program, args, cwd } => run::spawn(&program, &args, cwd.as_deref()).map(|()| program),
        run::Action::Unsupported(b) => Err(format!("phoned: {b} bu sürümde yok (W2.4)")),
    };
    let us = t0.elapsed().as_micros();
    match r {
        Ok(what) => {
            eprintln!("phoned: kısayol {id}: {what} · recv_ms={recv_ms} dispatch_us={us}");
            StatusCode::OK.into_response()
        }
        Err(e) => {
            eprintln!("phoned: kısayol {id} başarısız: {e}");
            problem(&e)
        }
    }
}

fn pane_for(st: &Shared, slot: u32) -> Option<String> {
    st.ext.herdr.lock().unwrap_or_else(PoisonError::into_inner).slots.pane_for(slot).map(str::to_owned)
}

/// Yuva → pane → herdr-web rotası → Zen (süreç başlatmadan). Eşlem yoksa (yoklayıcı uyurken) bir kez taze yoklanır.
/// Dönen metin halka sürelerini taşır (TP-AP-21): `pane_us conn_us caps_us[ (önbellek)] host_us`.
async fn herdr_go(st: &Shared, slot: u32, t0: Instant) -> Result<String, String> {
    let pane = match pane_for(st, slot) {
        Some(p) => p,
        None => {
            poll_herdr(st).await?;
            pane_for(st, slot).ok_or_else(|| format!("yuva {slot} boş"))?
        }
    };
    let pane_us = t0.elapsed().as_micros();
    let url = herdr::route_url(&st.ext.links.herdr_url, &pane);
    let r = crate::zen::route(&st.ext.links.zen_sock, &url).await?;
    // Vurgu (TP-AP-17) + "açıldı" (TP-AP-22): rota tuttuysa panel öbür yarısıyla birlikte yoklamayı beklemeden bu ajanı vurgular.
    if crate::zen::reply_ok(&r.reply) {
        let ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        let v = st.ext.herdr.lock().unwrap_or_else(PoisonError::into_inner).set_active(&pane, unix_ms(), ms);
        if let Some(v) = v {
            st.ext.panel.set("herdr", v, None, unix_ms())?;
        }
    }
    let t = r.timing;
    Ok(format!(
        "yuva {slot} → {pane} · {}{} · pane_us={pane_us} conn_us={} caps_us={}{} host_us={}",
        r.reply,
        if r.can_route { "" } else { " (eski host: yalnız sekme öne)" },
        t.conn_us,
        t.caps_us,
        if t.caps_cached { "" } else { " (ikili okundu)" },
        t.host_us
    ))
}

/// Zen host ısıtması (TP-AP-20): açılışta ve 30 sn'de bir yetenek önbelleği tazelenir (host değiştiyse yeniden okunur);
/// hata → 2 sn sonra yeniden. Host'a bayt yazılmaz (yan etkisiz). Yalnız durum değişince günlüğe yazar.
pub async fn zen_warm_loop(st: Shared) {
    let mut last: Option<String> = None;
    loop {
        let r = crate::zen::warm(&st.ext.links.zen_sock).await;
        let wait = if r.is_ok() { WARM_OK } else { WARM_RETRY };
        let msg = r.map_or_else(|e| e, |c| format!("Zen host hazır (route={c})"));
        if last.as_deref() != Some(msg.as_str()) {
            eprintln!("phoned: ısıtma: {msg}");
            last = Some(msg);
        }
        tokio::time::sleep(wait).await;
    }
}

pub const WARM_OK: Duration = Duration::from_secs(30);
pub const WARM_RETRY: Duration = Duration::from_secs(2);

/// Tek yoklama: herdr → izleyici → panel (değişim varsa `push["herdr"]`, yeni blocked → `notify`).
///
/// # Errors
/// herdr erişilemedi (izleyici görünümü unutur: geri gelince yeniden gönderilir).
pub async fn poll_herdr(st: &Shared) -> Result<(), String> {
    let ext = &st.ext;
    ext.herdr_polls.fetch_add(1, Ordering::Relaxed);
    let agents = match herdr::list(&ext.links.herdr_sock, Duration::from_secs(1)).await {
        Ok(a) => a,
        Err(e) => {
            ext.herdr.lock().unwrap_or_else(PoisonError::into_inner).forget_view();
            return Err(e);
        }
    };
    // Kayıt okuma dosya G/Ç'si (ilk karşılaşmada ≤ 16 MB/kayıt): tek iş parçacıklı çalışma zamanını bekletmesin.
    let goals = {
        let reader = Arc::clone(&ext.goals);
        let list = agents.clone();
        tokio::task::spawn_blocking(move || reader.lock().unwrap_or_else(PoisonError::into_inner).view(&list)).await.unwrap_or(Value::Null)
    };
    let change = {
        let mut t = ext.herdr.lock().unwrap_or_else(PoisonError::into_inner);
        t.set_goals(goals);
        t.observe(&agents, unix_secs())
    };
    let now = unix_ms();
    if let Some(v) = change.view {
        ext.panel.set("herdr", v, None, now)?;
    }
    for text in change.notify {
        ext.panel.emit("notify", Some(json!({"text": text})), now);
    }
    Ok(())
}

/// Yoklayıcı: yalnız biri bakarken (`looking`) 1,5 sn'de bir; değilse herdr'a istek yok (TP-W2-08b).
pub async fn herdr_loop(st: Shared) {
    let mut last_err: Option<String> = None;
    loop {
        tokio::time::sleep(Duration::from_millis(herdr::PERIOD_MS)).await;
        if !looking(&st) {
            continue;
        }
        match poll_herdr(&st).await {
            Ok(()) => {
                if last_err.take().is_some() {
                    eprintln!("phoned: herdr yeniden erişilebilir");
                }
            }
            Err(e) => {
                if last_err.as_deref() != Some(e.as_str()) {
                    eprintln!("phoned: {e}");
                    last_err = Some(e);
                }
            }
        }
    }
}

/// `/api/info` adb alanları.
#[must_use]
pub fn adb_info(st: &Shared) -> Value {
    let s = adb::status(&st.ext.adb);
    json!({
        "adb": st.ext.links.adb.addr.as_deref().map_or_else(|| "kapalı".to_owned(), |a| format!("socket {a}")),
        "connected": !s.devices.is_empty(),
        "devices": s.devices,
        "appMissing": s.missing,
    })
}

/// Eylem uçları (`/api` altında; `/agent/api` öneki yönlendiricide eklenir).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/push", get(push_list).delete(push_clear))
        .route("/api/push/{key}", get(push_get).post(push_set).put(push_set).delete(push_del))
        .route("/api/notify", post(notify))
        .route("/api/goto/{page}", post(goto))
        .route("/api/refresh", post(refresh))
        .route("/api/run/{id}", post(run_shortcut))
}
