//! HTTP yüzeyi: güvenlik katmanı, uçlar, SSE akışı, statik sunum. Yalnız 127.0.0.1.
//!
//! Uç şekilleri PhoneBox 0.6.9 `Program.cs` ve `audit/contract/0.6.9/*.shape.json` ile aynıdır. W2.3 (`actions`: push,
//! adb, herdr, Zen) ve W2.4 (`Extras`: süreçler, kapatma, medya) bağlı; henüz taşınmamış alt sistemler 0.6.9'un
//! "veri yok" hâlindeki boş şekli döndürür.

use crate::collect::linux::LinuxCollector;
use crate::config::{self, ConfigStore, SaveError};
use crate::guard::{self, Incoming};
use crate::hub::{self, Hub, PcSnapshot, Sampler, Subscription};
use crate::stream::{self, HEARTBEAT, Mode, Out, Pacer};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const JSON: &str = "application/json; charset=utf-8";
/// `work` (oturumlar + GitHub) kapsam dışı: 0.6.9'un GitHub erişilemez hâlindeki şekil.
const WORK: &str = r#"{"sessions":[],"github":{"error":"phoned: GitHub kapsam dışı","myPrs":[],"notifications":0,"reviewRequests":[],"updatedAt":null,"user":null}}"#;
/// Akışta örnek beklerken en uzun bekleme (TP-W2-13: bakılırken boşluk ≤ 1,5 sn).
const WAIT_MS: u64 = 1_500;

static SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
static SEC_FETCH_MODE: HeaderName = HeaderName::from_static("sec-fetch-mode");
static CONFIG_VERSION: HeaderName = HeaderName::from_static("x-config-version");
static DEVICE: HeaderName = HeaderName::from_static("x-phonebox-device");
static ACCEL: HeaderName = HeaderName::from_static("x-accel-buffering");

/// Sunulan dosyaların yeri (kopyalanmaz; upstream ağacından okunur).
#[derive(Debug, Clone)]
pub struct Assets {
    /// `settings.html`, `widgets.json`, `templates.json`, `phonebox.ico`.
    pub agent_dir: PathBuf,
    /// Telefon paneli (`index.html`, `main.js`, `widgets/`).
    pub www: PathBuf,
}

/// W2.4 konuları: süreç listesi (çekme), korumalı kapatma, MPRIS, ses (W2.4b, pulse yerel soketi).
pub struct Extras {
    pub procs: Arc<crate::procs::Procs>,
    pub killer: Arc<crate::kill::Killer>,
    pub media: Arc<crate::media::Media>,
    pub audio: Arc<crate::audio::Audio>,
    pub ctx: crate::kill::Ctx,
}

impl Extras {
    /// Gerçek kaynaklar: `/proc`, pidfd, (MPRIS bağlantısı `serve` içinde açılır).
    #[must_use]
    pub fn linux() -> Self {
        Self {
            procs: Arc::new(crate::procs::Procs::new(Box::new(crate::procs::linux::LinuxProcs::new()))),
            killer: Arc::new(crate::kill::Killer::new(Box::new(crate::kill::linux::PidfdSignaller))),
            media: Arc::new(crate::media::Media::new()),
            audio: Arc::new(crate::audio::Audio::new(crate::audio::Settings::from_env())),
            ctx: crate::kill::http::ctx(),
        }
    }
}

/// Ajanın dış dünyaya bağlantıları tek yerde: W2.3 uçları (`Links`: herdr, Zen, adb) + W2.4 kaynakları (`Extras`).
pub struct Wiring {
    pub links: crate::actions::Links,
    pub extras: Extras,
}

pub struct AppState {
    pub cfg: Arc<ConfigStore>,
    pub hub: Arc<Hub>,
    pub locked: watch::Receiver<bool>,
    pub assets: Assets,
    pub port: u16,
    pub host: String,
    /// W2.3: panel deposu, herdr, adb, Zen (`actions`).
    pub ext: crate::actions::Ext,
    /// W2.4: süreç listesi, korumalı kapatma, MPRIS.
    pub extras: Extras,
    /// K11 dikte: herdr-web motoruna köprü + tek yapıştırma kuyruğu (`dictate`).
    pub dictate: Arc<crate::dictate::Dictator>,
    catalog: String,
}

pub type Shared = Arc<AppState>;

impl AppState {
    /// # Errors
    /// `widgets.json` / `templates.json` okunamaz ya da JSON değilse.
    pub fn new(
        cfg: Arc<ConfigStore>,
        hub: Arc<Hub>,
        locked: watch::Receiver<bool>,
        assets: Assets,
        port: u16,
        host: String,
    ) -> Result<Shared, String> {
        let links = crate::actions::Links::from_env(port);
        Self::new_with(cfg, hub, locked, assets, port, host, Wiring { links, extras: Extras::linux() })
    }

    /// `new` + dış bağlantılar elle (`Wiring`: W2.3 uçları + W2.4 kaynakları; testte geçici soketler, sahte kaynaklar).
    ///
    /// # Errors
    /// `new` ile aynı.
    pub fn new_with(
        cfg: Arc<ConfigStore>,
        hub: Arc<Hub>,
        locked: watch::Receiver<bool>,
        assets: Assets,
        port: u16,
        host: String,
        wiring: Wiring,
    ) -> Result<Shared, String> {
        let Wiring { links, extras } = wiring;
        let read = |name: &str| -> Result<String, String> {
            let p = assets.agent_dir.join(name);
            let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            serde_json::from_str::<Value>(&s).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok(s)
        };
        let catalog = format!(r#"{{"cols":12,"rows":6,"widgets":{},"templates":{}}}"#, read("widgets.json")?.trim(), read("templates.json")?.trim());
        let data = cfg.path().with_file_name("data.json");
        let ext = crate::actions::Ext::new(data, links);
        let dictate = Arc::new(crate::dictate::Dictator::new(crate::dictate::Settings::from_env()));
        Ok(Arc::new(Self { cfg, hub, locked, assets, port, host, ext, extras, dictate, catalog }))
    }
}

fn json_text(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, JSON)], body).into_response()
}

pub(crate) fn json_ok(v: &Value) -> Response {
    json_text(StatusCode::OK, v.to_string())
}

fn unix_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Yerel saatin günün kaçıncı dakikası olduğu (gece modu penceresi için).
fn local_minute() -> Option<u32> {
    // SAFETY: `time(NULL)` yan etkisizdir.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    // SAFETY: `libc::tm` düz C yapısıdır; sıfır bit deseni geçerli başlangıç değeridir.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: iki işaretçi de geçerli ve hizalı; `localtime_r` yalnız `tm`'ye yazar, iş parçacığı güvenlidir.
    let r = unsafe { libc::localtime_r(&raw const now, &raw mut tm) };
    if r.is_null() {
        return None;
    }
    u32::try_from(tm.tm_hour * 60 + tm.tm_min).ok()
}

/// 0.6.9 `State()` şekli (+ `pc.swapTotalGb/swapUsedGb`). `ev` = telefonun son gördüğü olay (`?ev=`).
pub fn state_value(st: &AppState, pc: Option<&PcSnapshot>, ev: Option<i64>) -> Value {
    let (cfg, version) = st.cfg.snapshot();
    let now = unix_ms();
    let panel = &st.ext.panel;
    let events = ev.map(|s| panel.since(s, now)).unwrap_or_default();
    json!({
        "update": null,
        "ts": unix_ms(),
        "host": st.host,
        "cfgVersion": version,
        "pushVersion": panel.version(now),
        "evSeq": panel.seq(),
        "events": events,
        "pc": pc,
        "media": st.extras.media.snapshot(),
        "audio": st.extras.audio.snapshot(),
        "ping": null,
        "rules": [],
        "rulesPaused": null,
        "privacy": {"camera": false, "cameraApps": [], "mic": false, "micApps": []},
        "obs": null,
        "toggles": config::toggles(&cfg),
    })
}

/// Biri bakıyor mu: oturum açık ve gece modu dışında → canlı (1 Hz).
fn mode(st: &AppState) -> Mode {
    let locked = *st.locked.borrow();
    let night = local_minute().is_some_and(|m| config::night_active(&st.cfg.snapshot().0, m));
    if locked || night { Mode::Quiet } else { Mode::Live }
}

/// Biri bakıyor mu (çekme/akış var + canlı kip): herdr yoklaması bunun arkasında (D11 deseni, TP-W2-08b).
#[must_use]
pub fn looking(st: &AppState) -> bool {
    st.hub.active(st.hub.now_ms()) && mode(st) == Mode::Live
}

async fn guard_mw(State(st): State<Shared>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let allowed = {
        let h = req.headers();
        let get = |n: &HeaderName| h.get(n).map(|v| v.to_str().unwrap_or("\u{1}"));
        let inc = Incoming {
            host: get(&header::HOST),
            origin: get(&header::ORIGIN),
            sec_fetch_site: get(&SEC_FETCH_SITE),
            sec_fetch_mode: get(&SEC_FETCH_MODE),
            is_get: req.method() == axum::http::Method::GET,
            path: &path,
        };
        guard::allowed(&inc, st.port)
    };
    if !allowed {
        return StatusCode::FORBIDDEN.into_response();
    }
    let framed = guard::starts_with_segment(&path, "/panel");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static(if framed { "SAMEORIGIN" } else { "DENY" }));
    res
}

async fn pair(State(st): State<Shared>) -> Response {
    json_ok(&json!({"enabled": false, "pc": st.host}))
}

async fn info(State(st): State<Shared>) -> Response {
    let adb = crate::actions::adb_info(&st);
    json_ok(&json!({
        "version": concat!(env!("CARGO_PKG_VERSION"), "-phoned"),
        "os": "linux",
        "configPath": st.cfg.path().display().to_string(),
        "logPath": "journalctl --user -u phoned",
        "adb": adb.get("adb"),
        "connected": adb.get("connected"),
        "devices": adb.get("devices"),
        "appMissing": adb.get("appMissing"),
        "usbProblem": null,
    }))
}

async fn catalog(State(st): State<Shared>) -> Response {
    json_text(StatusCode::OK, st.catalog.clone())
}

async fn config_public(State(st): State<Shared>, headers: HeaderMap) -> Response {
    let (cfg, version) = st.cfg.snapshot();
    let device = headers.get(&DEVICE).and_then(|v| v.to_str().ok()).filter(|d| !d.is_empty());
    json_ok(&json!({"version": version, "config": config::to_public(&cfg, device)}))
}

async fn settings_get(State(st): State<Shared>) -> Response {
    let (cfg, version) = st.cfg.snapshot();
    let body = serde_json::to_string_pretty(&*cfg).unwrap_or_else(|_| "{}".to_owned());
    let mut res = json_text(StatusCode::OK, body);
    if let Ok(v) = HeaderValue::from_str(&version.to_string()) {
        res.headers_mut().insert(CONFIG_VERSION.clone(), v);
    }
    res
}

async fn settings_put(State(st): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let is_json = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|c| c.starts_with("application/json"));
    if !is_json {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let expected = headers.get(&CONFIG_VERSION).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<i64>().ok());
    let Ok(text) = std::str::from_utf8(&body) else {
        return json_text(StatusCode::BAD_REQUEST, json!({"error": "Invalid JSON: not UTF-8"}).to_string());
    };
    match st.cfg.save(text, expected) {
        Ok(version) => {
            eprintln!("phoned: settings saved");
            json_ok(&json!({"version": version}))
        }
        Err(SaveError::Conflict) => json_text(StatusCode::CONFLICT, json!({"error": config::CONFLICT}).to_string()),
        Err(SaveError::Invalid(m)) => json_text(StatusCode::BAD_REQUEST, json!({"error": m}).to_string()),
        Err(SaveError::Io(e)) => {
            eprintln!("phoned: ayarlar yazılamadı: {e}");
            json_text(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "could not write the settings file"}).to_string())
        }
    }
}

async fn state(State(st): State<Shared>, Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
    st.hub.pull();
    let snap = st.hub.tx.borrow().clone();
    let ev = q.get("ev").and_then(|e| e.parse::<i64>().ok());
    json_ok(&state_value(&st, snap.as_deref(), ev))
}

async fn debug_topics(State(st): State<Shared>) -> Response {
    let hub = &st.hub;
    let mut v = json!({
        "pc": {
            "subscribers": hub.subscribers(),
            "active": hub.active(hub.now_ms()),
            "runs": hub.runs.load(std::sync::atomic::Ordering::Relaxed),
            "collectUs": hub.collect_us.load(std::sync::atomic::Ordering::Relaxed),
        },
        "herdr": {"polls": st.ext.herdr_polls.load(std::sync::atomic::Ordering::Relaxed), "looking": looking(&st)},
        "changes": stream::CHANGE_FIELDS.iter().zip(&hub.changes).map(|(k, c)| ((*k).to_owned(), json!(c.load(std::sync::atomic::Ordering::Relaxed)))).collect::<serde_json::Map<_, _>>(),
        "locked": *st.locked.borrow(),
        "mode": if mode(&st) == Mode::Live { "live" } else { "quiet" },
    });
    if let Some(m) = v.as_object_mut() {
        m.extend(crate::debug::topics(&st));
    }
    json_ok(&v)
}

/// Bir akış bağlantısı; düşünce abonelik de düşer (örnekleyici uyuyabilir).
struct Conn {
    st: Shared,
    _sub: Subscription,
    rx: watch::Receiver<Option<Arc<PcSnapshot>>>,
    /// Panel deposu tiki (olay / push değişimi) → bekleyen tik olmadan uyan (TP-W2-P4, TP-W2-08c).
    panel_rx: watch::Receiver<u64>,
    /// Ses değişim tiki (W2.4b) → bekleyen tik olmadan `state` (TP-W24b-A6).
    audio_rx: watch::Receiver<u64>,
    /// Medya (MPRIS) değişim tiki (W2.5) → ses gibi, sessiz kipte de bekleyen tik olmadan `state` (TP-W25-M1).
    media_rx: watch::Receiver<u64>,
    ev_seq: i64,
    push_version: i64,
    pacer: Pacer,
    sent_work: bool,
    sent_state: bool,
}

/// Panel / ses tiki; yayıncı düştüyse hiç dönmez (sıcak döngü yok).
async fn panel_changed(rx: &mut watch::Receiver<u64>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Ses ya da medya tiki (ikisi de `state`'in parçası, kip ne olursa olsun hemen gider).
async fn extras_changed(audio: &mut watch::Receiver<u64>, media: &mut watch::Receiver<u64>) {
    tokio::select! {
        () = panel_changed(audio) => {}
        () = panel_changed(media) => {}
    }
}

impl Conn {
    async fn next_chunk(&mut self) -> Vec<u8> {
        if !self.sent_work {
            self.sent_work = true;
            return stream::frame("work", WORK);
        }
        loop {
            let now = unix_ms();
            if let Some(e) = self.st.ext.panel.since(self.ev_seq, now).into_iter().next() {
                self.ev_seq = e.seq;
                return stream::frame("event", &json!(e).to_string());
            }
            if self.sent_state && self.st.ext.panel.version(now) != self.push_version {
                return self.state_frame(None);
            }
            if self.sent_state || self.rx.borrow().is_none() {
                // Zaman aşımı (örnekleyici geç kaldı) da bir tik sayılır; yayıncı düştüyse dönmeden önce bekle (sıcak döngü yok).
                tokio::select! {
                    r = tokio::time::timeout(Duration::from_millis(WAIT_MS), self.rx.changed()) => {
                        if let Ok(Err(_)) = r {
                            tokio::time::sleep(Duration::from_millis(hub::SAMPLE_MS)).await;
                        }
                    }
                    () = panel_changed(&mut self.panel_rx) => continue,
                    () = extras_changed(&mut self.audio_rx, &mut self.media_rx) => {
                        if self.sent_state {
                            return self.state_frame(None);
                        }
                        continue;
                    }
                }
            }
            let snap = self.rx.borrow_and_update().clone();
            let out = self.pacer.on_tick(self.st.hub.now_ms(), mode(&self.st), snap.as_deref());
            match out {
                Out::State => {
                    self.sent_state = true;
                    for (i, c) in self.st.hub.changes.iter().enumerate() {
                        if self.pacer.last_changes & (1 << i) != 0 {
                            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    return self.state_frame(snap.as_deref());
                }
                Out::Heartbeat => return HEARTBEAT.to_vec(),
                Out::Nothing => {}
            }
        }
    }
}

impl Conn {
    /// Tam `state` çerçevesi; `pc` yoksa son örnek.
    fn state_frame(&mut self, pc: Option<&PcSnapshot>) -> Vec<u8> {
        let last = if pc.is_none() { self.rx.borrow().clone() } else { None };
        let v = state_value(&self.st, pc.or(last.as_deref()), None);
        self.push_version = v.get("pushVersion").and_then(Value::as_i64).unwrap_or(0);
        stream::frame("state", &v.to_string())
    }
}

async fn event_stream(State(st): State<Shared>) -> Response {
    let conn = Conn {
        _sub: st.hub.subscribe(),
        rx: st.hub.tx.subscribe(),
        panel_rx: st.ext.panel.watch(),
        audio_rx: st.extras.audio.watch(),
        media_rx: st.extras.media.watch(),
        ev_seq: st.ext.panel.seq(),
        push_version: 0,
        st,
        pacer: Pacer::default(),
        sent_work: false,
        sent_state: false,
    };
    let chunks = futures_util::stream::unfold(conn, |mut c| async move {
        let chunk = c.next_chunk().await;
        Some((Ok::<_, std::convert::Infallible>(chunk), c))
    });
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream")), (ACCEL.clone(), HeaderValue::from_static("no"))],
        Body::from_stream(chunks),
    )
        .into_response()
}

/// 0.6.9 `^(widgets/)?[A-Za-z0-9_.-]+$` ve `..` yok.
#[must_use]
pub fn panel_file_ok(file: &str) -> bool {
    let name = file.strip_prefix("widgets/").unwrap_or(file);
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')) && !file.contains("..")
}

fn content_type(file: &str) -> &'static str {
    match file.rsplit_once('.').map(|(_, e)| e) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

async fn serve_file(path: PathBuf, ctype: &'static str) -> Response {
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, ctype)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn panel_redirect(uri: Uri) -> Response {
    let loc = uri.query().map_or_else(|| "/panel/".to_owned(), |q| format!("/panel/?{q}"));
    (StatusCode::FOUND, [(header::LOCATION, loc)]).into_response()
}

async fn panel_index(State(st): State<Shared>) -> Response {
    serve_file(st.assets.www.join("index.html"), content_type("index.html")).await
}

async fn panel_file(State(st): State<Shared>, AxPath(file): AxPath<String>) -> Response {
    if !panel_file_ok(&file) {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_file(st.assets.www.join(&file), content_type(&file)).await
}

async fn settings_page(State(st): State<Shared>) -> Response {
    serve_file(st.assets.agent_dir.join("settings.html"), "text/html; charset=utf-8").await
}

async fn favicon(State(st): State<Shared>) -> Response {
    serve_file(st.assets.agent_dir.join("phonebox.ico"), "image/x-icon").await
}

fn fixed(body: &'static str) -> axum::routing::MethodRouter<Shared> {
    get(move || async move { json_text(StatusCode::OK, body.to_owned()) })
}

/// Tüm yönlendirici (testler aynı yolu kullanır).
pub fn router(st: Shared) -> Router {
    let api: Router<Shared> = Router::new()
        .route("/api/pair", get(pair))
        .route("/api/info", get(info))
        .route("/api/catalog", get(catalog))
        .route("/api/config", get(config_public))
        .route("/api/settings", get(settings_get).put(settings_put))
        .route("/api/state", get(state))
        .route("/api/stream", get(event_stream))
        .route("/api/debug/topics", get(debug_topics))
        .route("/api/work", fixed(WORK))
        .route("/api/approvals", fixed(r#"{"items":[]}"#))
        .route("/api/quotas", fixed(r#"{"providers":[]}"#))
        .route("/api/procs", get(crate::procs::http::list))
        .route("/api/procs/detail", get(crate::procs::http::details))
        .route("/api/procs/kill", post(crate::kill::http::kill))
        .route("/api/art", get(crate::media::http::art))
        .route("/api/media/{action}", post(crate::media::http::control))
        .route("/api/volume", post(crate::audio::http::volume))
        .route("/api/calendar", fixed(r#"{"updated":0,"events":[],"errors":[]}"#))
        .route("/api/batteries", fixed(r#"{"devices":[]}"#))
        .merge(crate::dictate::http::routes(Arc::clone(&st.dictate)))
        .merge(crate::actions::routes())
        .merge(crate::compat::routes());
    Router::new()
        .merge(api.clone())
        .nest("/agent", api)
        .route("/", get(settings_page))
        .route("/favicon.ico", get(favicon))
        .route("/panel", get(panel_redirect))
        .route("/panel/", get(panel_index))
        .route("/panel/{*file}", get(panel_file))
        .layer(middleware::from_fn_with_state(Arc::clone(&st), guard_mw))
        .with_state(st)
}

/// Komut satırı.
#[derive(Debug, Clone)]
pub struct Args {
    pub port: u16,
    pub config: PathBuf,
    pub seed_from: Option<PathBuf>,
    pub assets: Assets,
    /// adb sunucusu (`127.0.0.1:5037`) ya da `off`.
    pub adb: Option<String>,
    /// Panel uygulaması aday paketleri (K2: kendi APK'mız farklı `applicationId` ile).
    pub packages: Vec<String>,
}

/// Telefonun `adb reverse tcp:8765` ile bağlandığı port (0.6.9 `USB_BASE`).
pub const PRODUCTION_PORT: u16 = 8765;

pub const USAGE: &str = "kullanım: phoned [--port 8766] [--config <yol>] [--seed-from <0.6.9 config.json>] [--agent-dir <dizin>] [--www <dizin>] [--adb <host:port|off> (öntanım: --port 8765'te 127.0.0.1:5037, başka portta off)] [--packages <a,b>]";

impl Args {
    /// # Errors
    /// Bilinmeyen bayrak, eksik değer, geçersiz port, `HOME` yok.
    pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Self, String> {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME tanımlı değil")?;
        let refpool = home.join(".cartography/refpool/ph-phonebox");
        let config_home = std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        let mut a = Self {
            port: 8766,
            config: config_home.join("phoned/config.json"),
            seed_from: None,
            assets: Assets { agent_dir: refpool.join("agent/PhoneBox.Agent"), www: refpool.join("android/app/src/main/assets/www") },
            adb: Some(crate::adb::DEFAULT_ADDR.to_owned()),
            packages: vec![crate::adb::DEFAULT_PACKAGE.to_owned()],
        };
        let mut adb_given = false;
        while let Some(flag) = it.next() {
            let mut val = || it.next().ok_or_else(|| format!("{flag} değer ister"));
            match flag.as_str() {
                "--port" => a.port = val()?.parse().map_err(|e| format!("--port: {e}"))?,
                "--config" => a.config = val()?.into(),
                "--seed-from" => a.seed_from = Some(val()?.into()),
                "--agent-dir" => a.assets.agent_dir = val()?.into(),
                "--www" => a.assets.www = val()?.into(),
                "--adb" => {
                    a.adb = Some(val()?).filter(|v| v != "off");
                    adb_given = true;
                }
                "--packages" => a.packages = crate::adb::parse_packages(&val()?),
                other => return Err(format!("bilinmeyen bayrak {other}")),
            }
        }
        // Güvenli varsayılan (W2.5, TP-W25-A1): telefon yalnız üretim portunu tanır; gölge/test portundaki örnek
        // telefona `reverse`/WAKEUP/`am start` yazmasın — açık `--adb` ile istenmedikçe izleyici kapalı.
        if !adb_given && a.port != PRODUCTION_PORT {
            a.adb = None;
        }
        Ok(a)
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "linux".to_owned())
}

/// Ayar dosyası yoksa `--seed-from`'dan bir kez kopyalanır (mod 600); kaynak dosyaya yazılmaz.
fn ensure_config(a: &Args) -> Result<(), String> {
    if a.config.exists() {
        return Ok(());
    }
    let Some(seed) = &a.seed_from else {
        return Err(format!("ayar dosyası yok: {} (--seed-from ile ilk kopyayı ver)", a.config.display()));
    };
    let bytes = std::fs::read(seed).map_err(|e| format!("{}: {e}", seed.display()))?;
    config::validate(&serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", seed.display()))?)?;
    config::write_atomic(&a.config, &bytes).map_err(|e| format!("{}: {e}", a.config.display()))
}

async fn shutdown() {
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = term => {}
    }
}

/// Ajanı koştur (tek iş parçacıklı çalışma zamanında çağrılır).
///
/// # Errors
/// Ayar/varlık hatası, port bağlanamadı.
pub async fn serve(a: Args) -> Result<(), String> {
    ensure_config(&a)?;
    let cfg = Arc::new(ConfigStore::open(a.config.clone()));
    let hub = Hub::new();
    let (locked_tx, locked_rx) = watch::channel(false);
    let mut links = crate::actions::Links::from_env(a.port);
    links.adb.addr.clone_from(&a.adb);
    links.adb.packages.clone_from(&a.packages);
    let st = AppState::new_with(
        Arc::clone(&cfg),
        Arc::clone(&hub),
        locked_rx,
        a.assets.clone(),
        a.port,
        hostname(),
        Wiring { links, extras: Extras::linux() },
    )?;
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, a.port)).await.map_err(|e| format!("127.0.0.1:{}: {e}", a.port))?;
    eprintln!("phoned {} dinliyor: http://127.0.0.1:{} · ayar {}", env!("CARGO_PKG_VERSION"), a.port, a.config.display());
    let gpu = Arc::new(crate::gpu::Gpu::new(crate::gpu::Settings::system()));
    tokio::spawn(crate::gpu::run(Arc::clone(&gpu), hub.tx.subscribe()));
    tokio::spawn(hub::run_sampler(hub, Sampler::new(Box::new(LinuxCollector::new())).with_gpu(gpu)));
    tokio::spawn(crate::session::watch_locked(locked_tx));
    tokio::spawn(crate::actions::herdr_loop(Arc::clone(&st)));
    tokio::spawn(crate::actions::zen_warm_loop(Arc::clone(&st)));
    tokio::spawn(Arc::clone(&st.ext.panel).persist_loop(crate::actions::unix_ms));
    let serial = move || cfg.snapshot().0.get("deviceSerial").and_then(Value::as_str).map(str::to_owned);
    tokio::spawn(crate::adb::run(st.ext.links.adb.clone(), Arc::clone(&st.ext.adb), serial));
    tokio::spawn(crate::media::dbus::run(Arc::clone(&st.extras.media)));
    tokio::spawn(crate::media::yt::run(Arc::clone(&st.extras.media)));
    tokio::spawn(crate::audio::client::run(Arc::clone(&st.extras.audio)));
    axum::serve(listener, router(st).into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())
}
