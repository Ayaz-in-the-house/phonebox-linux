#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "test yardımcıları")]
//! Test düzeneği: geçici dizinde ayar + varlıklar, sahte toplayıcı, gerçek yönlendirici; 0.6.9 şekil karşılaştırıcısı.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response};
use http_body_util::BodyExt as _;
use phoned::actions::Links;
use phoned::app::{self, AppState, Assets, Shared};
use phoned::collect::{Collector, CpuTimes, Disk, Mem};
use phoned::config::ConfigStore;
use phoned::hub::{Hub, Sampler};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::watch;
use tower::ServiceExt as _;

pub const PORT: u16 = 8766;
const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

pub struct TempDir(pub PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn temp_dir() -> TempDir {
    static N: AtomicU32 = AtomicU32::new(0);
    let p = std::env::temp_dir().join(format!("phoned-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&p).unwrap();
    TempDir(p)
}

/// Her `cpu_times` çağrısında 100 jiffy ilerler, 80'i boşta → %20.
pub struct FakeCol(pub u64);
impl Collector for FakeCol {
    fn cpu_times(&mut self) -> Option<CpuTimes> {
        self.0 += 1;
        Some(CpuTimes { idle: 80 * self.0, total: 100 * self.0 })
    }
    fn memory(&mut self) -> Option<Mem> {
        Some(Mem { total_kb: 32 << 20, available_kb: 20 << 20, swap_total_kb: 100 << 20, swap_free_kb: 88 << 20 })
    }
    fn net_bytes(&mut self) -> Option<(u64, u64)> {
        Some((1000, 1000))
    }
    fn cpu_temp(&mut self) -> Option<f64> {
        Some(48.5)
    }
    fn disks(&mut self) -> Vec<Disk> {
        vec![Disk { name: "/".into(), used_gb: 500.0, total_gb: 900.0 }]
    }
    fn uptime_sec(&mut self) -> Option<u64> {
        Some(1234)
    }
    fn cpu_name(&self) -> &str {
        "Fake CPU"
    }
}

pub struct Harness {
    pub dir: TempDir,
    pub st: Shared,
    pub app: Router,
    pub locked: watch::Sender<bool>,
}

impl Harness {
    pub fn config_path(&self) -> PathBuf {
        self.dir.0.join("cfg/config.json")
    }
}

pub fn harness() -> Harness {
    harness_full(|_| {}, |l| l, phoned::app::Extras::linux())
}

/// Testte dış uçlar: var olmayan geçici soketler, adb kapalı (kimse gerçek herdr/Zen/adb'ye dokunmaz).
pub fn test_links(d: &Path) -> Links {
    let mut l = Links::from_env(PORT);
    l.herdr_sock = d.join("herdr.sock");
    l.zen_sock = d.join("zen.sock");
    l.herdr_url = "https://h.example/herdr/".into();
    l.adb.addr = None;
    l.home = "/home/test".into();
    l
}

/// `edit` ayar dosyasını yazmadan önce değiştirir; `links` geçici dizine göre dış uçları verir (W2.3).
pub fn harness_with(edit: impl FnOnce(&mut Value), links: impl FnOnce(Links) -> Links) -> Harness {
    harness_full(edit, links, phoned::app::Extras::linux())
}

/// W2.4: kaynakları (süreç listesi, sinyal gönderici, medya) değiştirilebilir düzenek.
pub fn harness_extras(extras: phoned::app::Extras) -> Harness {
    harness_full(|_| {}, |l| l, extras)
}

/// Tek düzenek: ayar düzenleme + dış uçlar (W2.3) + W2.4 kaynakları.
pub fn harness_full(edit: impl FnOnce(&mut Value), links: impl FnOnce(Links) -> Links, extras: phoned::app::Extras) -> Harness {
    let dir = temp_dir();
    let d = &dir.0;
    for sub in ["cfg", "agent", "www/widgets"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    let mut cfg_v: Value = serde_json::from_str(&std::fs::read_to_string(Path::new(FIX).join("config.json")).unwrap()).unwrap();
    edit(&mut cfg_v);
    std::fs::write(d.join("cfg/config.json"), serde_json::to_vec_pretty(&cfg_v).unwrap()).unwrap();
    std::fs::copy(Path::new(FIX).join("widgets.json"), d.join("agent/widgets.json")).unwrap();
    std::fs::copy(Path::new(FIX).join("templates.json"), d.join("agent/templates.json")).unwrap();
    std::fs::write(d.join("agent/settings.html"), "<html>settings</html>").unwrap();
    std::fs::write(d.join("agent/phonebox.ico"), b"ico").unwrap();
    std::fs::write(d.join("www/index.html"), "<html>panel</html>").unwrap();
    std::fs::write(d.join("www/main.js"), "console.log(1)").unwrap();
    std::fs::write(d.join("www/style.css"), "body{}").unwrap();
    std::fs::write(d.join("www/widgets/clock.js"), "export{}").unwrap();
    std::fs::write(d.join("secret.txt"), "gizli").unwrap();
    let cfg = Arc::new(ConfigStore::open(d.join("cfg/config.json")));
    let hub = Hub::new();
    let mut s = Sampler::new(Box::new(FakeCol(0)));
    hub.sample_once(&mut s);
    hub.sample_once(&mut s);
    let (locked, rx) = watch::channel(false);
    let l = links(test_links(d));
    let st = AppState::new_with(
        cfg,
        hub,
        rx,
        Assets { agent_dir: d.join("agent"), www: d.join("www") },
        PORT,
        "testhost".into(),
        app::Wiring { links: l, extras },
    )
    .unwrap();
    let app = app::router(Arc::clone(&st));
    Harness { dir, st, app, locked }
}

pub fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).header("host", "127.0.0.1:8766").body(Body::empty()).unwrap()
}

pub async fn send(app: &Router, req: Request<Body>) -> Response<Body> {
    app.clone().oneshot(req).await.unwrap()
}

pub async fn body_bytes(res: Response<Body>) -> Vec<u8> {
    res.into_body().collect().await.unwrap().to_bytes().to_vec()
}

pub async fn get_json(app: &Router, path: &str) -> Value {
    let res = send(app, get(path)).await;
    assert_eq!(res.status(), 200, "{path}");
    serde_json::from_slice(&body_bytes(res).await).unwrap()
}

pub fn fixture(name: &str) -> Value {
    let p = Path::new(FIX).join("contract").join(format!("{name}.shape.json"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    v["shape"].clone()
}

/// Karşılaştırma kuralları: yol gösterimi `a.b[].c`.
#[derive(Default)]
pub struct Rules<'a> {
    /// Bilinçli `null` (belgeli, docs/STATE.md "boş şekiller").
    pub nullable: &'a [&'a str],
    /// Anahtarları veriye göre değişen eşlemler (fixture'daki anahtarlar zorunlu değil).
    pub dynamic: &'a [&'a str],
    /// 0.6.9'un da yazmadığı anahtarlar (ham dosyadan sunulan yerlerde, ör. `templates.json`).
    pub optional: &'a [&'a str],
}

/// 0.6.9 şekli (`fixture`) ile yanıtı (`ours`) karşılaştır; hata listesi döner (boş = uyumlu).
pub fn compare(fixture: &Value, ours: &Value, path: &str, rules: &Rules<'_>, errs: &mut Vec<String>) {
    let here = if path.is_empty() { "$" } else { path };
    if ours.is_null() && rules.nullable.contains(&path) {
        return;
    }
    match fixture {
        Value::String(t) => {
            let ok = match t.as_str() {
                "NoneType" => true,
                "bool" => ours.is_boolean(),
                "int" | "float" => ours.is_number(),
                "str" => ours.is_string(),
                "object" => ours.is_object(),
                other => {
                    errs.push(format!("{here}: bilinmeyen fixture tipi {other}"));
                    return;
                }
            };
            if !ok {
                errs.push(format!("{here}: beklenen {t}, gelen {ours}"));
            }
        }
        Value::Object(fmap) => {
            let Some(omap) = ours.as_object() else {
                errs.push(format!("{here}: nesne bekleniyordu, gelen {ours}"));
                return;
            };
            if rules.dynamic.contains(&path) {
                return;
            }
            for (k, f) in fmap {
                let sub = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                match omap.get(k) {
                    Some(o) => compare(f, o, &sub, rules, errs),
                    None if rules.optional.contains(&sub.as_str()) => {}
                    None => errs.push(format!("{sub}: eksik anahtar")),
                }
            }
        }
        Value::Array(farr) => {
            let Some(oarr) = ours.as_array() else {
                errs.push(format!("{here}: dizi bekleniyordu, gelen {ours}"));
                return;
            };
            if let Some(f0) = farr.first() {
                for o in oarr {
                    compare(f0, o, &format!("{path}[]"), rules, errs);
                }
            }
        }
        other => errs.push(format!("{here}: fixture biçimi tanınmıyor {other}")),
    }
}
