//! Panel deposu: dışarıdan itilen değerler (`push`) + telefona giden kısa olay kuyruğu (`notify`, `goto`, `refresh`…).
//!
//! 0.6.9 `PushStore.cs` eşi: anahtar `^[A-Za-z0-9_.-]{1,64}$`, en çok 200 anahtar, gövde ≤ 64 KB, olaylar bellekte (≤ 50,
//! ≤ 60 sn), değerler `data.json`'da kalıcı (yazma 2 sn geciktirilir). Saat dışarıdan verilir (`now` = Unix ms) → testte sanal.
//! Her değişim/olay `tick`'i artırır: akış bağlantıları bekleyen tik olmadan uyanır (TP-W2-08c, TP-W2-P4).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio::sync::{Notify, watch};

pub const MAX_BYTES: usize = 64 * 1024;
const MAX_ITEMS: usize = 200;
const MAX_EVENTS: usize = 50;
const EVENT_TTL_MS: i64 = 60_000;
/// Disk yazımı ertelemesi (0.6.9 ile aynı).
pub const PERSIST_DELAY_MS: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub data: Value,
    pub at: i64,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub seq: i64,
    pub kind: String,
    pub data: Option<Value>,
    pub at: i64,
}

#[derive(Debug)]
struct Inner {
    items: BTreeMap<String, Item>,
    version: i64,
    seq: i64,
    events: VecDeque<Event>,
}

pub struct Panel {
    inner: Mutex<Inner>,
    tick: watch::Sender<u64>,
    dirty: Notify,
    data_path: PathBuf,
}

#[must_use]
pub fn valid_key(k: &str) -> bool {
    (1..=64).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// "21.5" → sayı, "true"/"false" → bool, öbürü dizge (0.6.9 `Parse`).
#[must_use]
pub fn parse_text(text: &str) -> Value {
    if let Ok(d) = text.parse::<f64>()
        && d.is_finite()
        && let Some(n) = serde_json::Number::from_f64(d)
    {
        return Value::Number(n);
    }
    match text {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(text.to_owned()),
    }
}

/// Webhook gövdesi: `?value=` önce; JSON türünde gövde JSON; öbürü kırpılmış metin (0.6.9 `ReadBody`).
///
/// # Errors
/// Gövde > 64 KB, JSON bozuk, UTF-8 değil.
pub fn read_body(value: Option<&str>, content_type: Option<&str>, body: &[u8]) -> Result<Value, String> {
    if let Some(v) = value {
        return Ok(parse_text(v));
    }
    if body.len() > MAX_BYTES {
        return Err(format!("body too large (max {} KB)", MAX_BYTES / 1024));
    }
    let text = std::str::from_utf8(body).map_err(|_| "body is not UTF-8".to_owned())?.trim();
    if content_type.is_some_and(|c| c.to_ascii_lowercase().contains("json")) {
        let t = if text.is_empty() { "null" } else { text };
        return serde_json::from_str(t).map_err(|e| format!("invalid JSON: {e}"));
    }
    Ok(parse_text(text))
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Inner {
    fn changed(&mut self, now: i64) {
        self.version = now.max(self.version.saturating_add(1));
    }

    fn purge(&mut self, now: i64) -> bool {
        let before = self.items.len();
        self.items.retain(|_, v| v.expires_at.is_none_or(|e| e > now));
        let removed = self.items.len() != before;
        if removed {
            self.changed(now);
        }
        removed
    }
}

impl Panel {
    /// `data_path` varsa okunur (bozuksa boş başlar, tek günlük satırı).
    #[must_use]
    pub fn open(data_path: PathBuf, now: i64) -> Self {
        let mut items = BTreeMap::new();
        match std::fs::read(&data_path) {
            Ok(b) => match serde_json::from_slice::<BTreeMap<String, Item>>(&b) {
                Ok(m) => items = m.into_iter().filter(|(k, _)| valid_key(k)).collect(),
                Err(e) => eprintln!("phoned: {} okunamadı: {e}", data_path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("phoned: {} okunamadı: {e}", data_path.display()),
        }
        Self {
            inner: Mutex::new(Inner { items, version: now, seq: 0, events: VecDeque::new() }),
            tick: watch::Sender::new(0),
            dirty: Notify::new(),
            data_path,
        }
    }

    #[must_use]
    pub fn data_path(&self) -> &Path {
        &self.data_path
    }

    /// Akış bağlantıları bunu dinler.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.tick.subscribe()
    }

    fn bump(&self, persist: bool) {
        self.tick.send_modify(|t| *t = t.wrapping_add(1));
        if persist {
            self.dirty.notify_one();
        }
    }

    /// Süresi dolanlar düşülür (bu da bir değişimdir).
    pub fn version(&self, now: i64) -> i64 {
        let (v, purged) = {
            let mut g = lock(&self.inner);
            let p = g.purge(now);
            (g.version, p)
        };
        if purged {
            self.bump(true);
        }
        v
    }

    #[must_use]
    pub fn seq(&self) -> i64 {
        lock(&self.inner).seq
    }

    #[must_use]
    pub fn snapshot(&self, now: i64) -> BTreeMap<String, Item> {
        let _ = self.version(now);
        lock(&self.inner).items.clone()
    }

    #[must_use]
    pub fn get(&self, key: &str, now: i64) -> Option<Item> {
        self.snapshot(now).remove(key)
    }

    /// # Errors
    /// Geçersiz anahtar ya da anahtar tavanı.
    pub fn set(&self, key: &str, data: Value, ttl: Option<i64>, now: i64) -> Result<i64, String> {
        if !valid_key(key) {
            return Err("key must be 1-64 characters: letters, digits, '.', '_' or '-'".to_owned());
        }
        let v = {
            let mut g = lock(&self.inner);
            g.purge(now);
            if !g.items.contains_key(key) && g.items.len() >= MAX_ITEMS {
                return Err(format!("too many keys (max {MAX_ITEMS}); delete some first"));
            }
            let ttl = ttl.or_else(|| data.get("ttl").and_then(Value::as_i64));
            let expires_at = ttl.filter(|t| *t > 0).map(|t| now.saturating_add(t.saturating_mul(1000)));
            g.items.insert(key.to_owned(), Item { data, at: now, expires_at });
            g.changed(now);
            g.version
        };
        self.bump(true);
        Ok(v)
    }

    pub fn remove(&self, key: &str, now: i64) -> bool {
        let ok = {
            let mut g = lock(&self.inner);
            let ok = g.items.remove(key).is_some();
            if ok {
                g.changed(now);
            }
            ok
        };
        if ok {
            self.bump(true);
        }
        ok
    }

    pub fn clear(&self, now: i64) {
        {
            let mut g = lock(&self.inner);
            g.items.clear();
            g.changed(now);
        }
        self.bump(true);
    }

    pub fn emit(&self, kind: &str, data: Option<Value>, now: i64) -> Event {
        let e = {
            let mut g = lock(&self.inner);
            g.seq += 1;
            let e = Event { seq: g.seq, kind: kind.to_owned(), data, at: now };
            g.events.push_back(e.clone());
            while g.events.len() > MAX_EVENTS {
                g.events.pop_front();
            }
            e
        };
        self.bump(false);
        e
    }

    /// `seq` sonrası, en çok bir dakikalık olaylar (uzakta kalan telefon sel almasın).
    #[must_use]
    pub fn since(&self, seq: i64, now: i64) -> Vec<Event> {
        let cutoff = now - EVENT_TTL_MS;
        lock(&self.inner).events.iter().filter(|e| e.seq > seq && e.at > cutoff).cloned().collect()
    }

    /// Şimdiki değerleri `data.json`'a atomik yaz (mod 600).
    ///
    /// # Errors
    /// Yazma hatası.
    pub fn persist(&self, now: i64) -> std::io::Result<()> {
        let snap = self.snapshot(now);
        let bytes = serde_json::to_vec(&snap).map_err(std::io::Error::other)?;
        crate::config::write_atomic(&self.data_path, &bytes)
    }

    /// Kalıcılık döngüsü: değişimden 2 sn sonra bir kez yazar (değişim yokken uyur).
    pub async fn persist_loop(self: std::sync::Arc<Self>, now: impl Fn() -> i64) {
        loop {
            self.dirty.notified().await;
            tokio::time::sleep(std::time::Duration::from_millis(PERSIST_DELAY_MS)).await;
            if let Err(e) = self.persist(now()) {
                eprintln!("phoned: itilen veri kaydedilemedi ({}): {e}", self.data_path.display());
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test")]
mod tests {
    use super::*;
    use serde_json::json;

    fn p() -> Panel {
        Panel::open(std::env::temp_dir().join(format!("phoned-panel-none-{}", std::process::id())).join("data.json"), 1_000)
    }

    #[test]
    fn tp_w2_p3_parse_text_types() {
        assert_eq!(parse_text("21.5"), json!(21.5));
        assert_eq!(parse_text("true"), json!(true));
        assert_eq!(parse_text("merhaba"), json!("merhaba"));
        assert_eq!(parse_text("NaN"), json!("NaN"));
        assert_eq!(read_body(Some("3"), None, b"x").unwrap(), json!(3.0));
        assert_eq!(read_body(None, Some("application/json"), b" {\"a\":1} ").unwrap(), json!({"a":1}));
        assert_eq!(read_body(None, Some("application/json"), b"").unwrap(), Value::Null);
        assert!(read_body(None, Some("application/json"), b"{").is_err());
        assert_eq!(read_body(None, Some("text/plain"), b"  selam \n").unwrap(), json!("selam"));
        assert!(read_body(None, None, &vec![b'a'; MAX_BYTES + 1]).is_err());
    }

    #[test]
    fn tp_w2_p3_keys_version_ttl_and_cap() {
        let s = p();
        assert!(s.set("bad key", json!(1), None, 2_000).is_err());
        assert!(s.set(&"k".repeat(65), json!(1), None, 2_000).is_err());
        let v1 = s.set("a", json!(1), None, 2_000).unwrap();
        let v2 = s.set("a", json!(2), None, 2_000).unwrap();
        assert!(v2 > v1, "aynı ms'de de sürüm artar");
        s.set("t", json!({"ttl": 5, "x": 1}), None, 3_000).unwrap();
        assert!(s.get("t", 7_999).is_some());
        let before = s.version(7_999);
        assert!(s.get("t", 8_000).is_none(), "ttl doldu");
        assert!(s.version(8_000) > before, "süre dolumu sürümü artırır");
        for i in 0..199 {
            s.set(&format!("k{i}"), json!(i), None, 9_000).unwrap();
        }
        assert!(s.set("fazla", json!(1), None, 9_000).is_err(), "200 tavanı");
        assert!(s.set("a", json!(3), None, 9_000).is_ok(), "var olan anahtar tavana takılmaz");
        assert!(s.remove("a", 9_000));
        assert!(!s.remove("a", 9_000));
    }

    #[test]
    fn tp_w2_p4_events_since_cap_and_age() {
        let s = p();
        let rx = s.watch();
        for i in 0..60 {
            s.emit("notify", Some(json!({"text": i})), 10_000);
        }
        assert!(rx.has_changed().unwrap(), "olay akışı uyandırır");
        assert_eq!(s.seq(), 60);
        let all = s.since(0, 10_000);
        assert_eq!(all.len(), 50);
        assert_eq!(all[0].seq, 11);
        assert_eq!(s.since(58, 10_000).len(), 2);
        assert!(s.since(0, 70_000).is_empty(), "bir dakikadan eski olay gönderilmez");
    }

    #[test]
    fn tp_w2_p3_persist_roundtrip() {
        let dir = std::env::temp_dir().join(format!("phoned-panel-{}-rt", std::process::id()));
        let path = dir.join("data.json");
        let s = Panel::open(path.clone(), 1);
        s.set("herdr", json!({"title": "x"}), None, 5).unwrap();
        s.persist(5).unwrap();
        let t = Panel::open(path, 6);
        assert_eq!(t.get("herdr", 6).unwrap().data, json!({"title": "x"}));
        let _ = std::fs::remove_dir_all(dir);
    }
}
