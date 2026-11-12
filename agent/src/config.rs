//! Ayar deposu — PhoneBox 0.6.9 `ConfigStore` (`Config.cs:301-383`) eşi.
//!
//! - Sürüm = dosyanın değişme zamanı, .NET `DateTime.Ticks` biçiminde (0.6.9 ile aynı büyüklük; telefon yalnız eşitlik bakar).
//! - Kayıt: doğrula → (beklenen sürüm verildiyse) uyuşmazlıkta `Conflict` → `config.json.tmp` (mod 600) yaz + fsync →
//!   eski dosyayı `.bak`'a kopyala (mod 600) → atomik `rename` → dizini fsync.
//! - İçerik `serde_json::Value` olarak tutulur: bilinmeyen anahtarlar korunur (0.6.9 ayar sayfası ne yazarsa geri okur).

use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// 0.6.9 `ConfigStore.Conflict` metni (ayar sayfası bunu gösterir).
pub const CONFLICT: &str = "The settings were changed in another tab or window after this one loaded them. Reload the page to see them; the changes made here were not saved.";

/// 0001-01-01 ile 1970-01-01 arası .NET tick sayısı.
const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;

#[derive(Debug)]
pub enum SaveError {
    /// Dosya, çağıranın yüklediği sürümden sonra değişti (409).
    Conflict,
    /// Gövde geçersiz (400), ileti 0.6.9 metinleriyle.
    Invalid(String),
    /// Yazma başarısız (500).
    Io(std::io::Error),
}

impl From<std::io::Error> for SaveError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

struct Cache {
    stamp: Option<SystemTime>,
    bad_stamp: Option<SystemTime>,
    value: Arc<Value>,
}

pub struct ConfigStore {
    path: PathBuf,
    cache: Mutex<Cache>,
}

/// Değişme zamanı → .NET tick.
#[must_use]
pub fn ticks(t: SystemTime) -> i64 {
    let nanos = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    UNIX_EPOCH_TICKS.saturating_add(i64::try_from(nanos / 100).unwrap_or(i64::MAX - UNIX_EPOCH_TICKS))
}

impl ConfigStore {
    #[must_use]
    pub fn open(path: PathBuf) -> Self {
        let store = Self { path, cache: Mutex::new(Cache { stamp: None, bad_stamp: None, value: Arc::new(Value::Object(Map::new())) }) };
        let _ = store.snapshot();
        store
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn stamp(&self) -> Option<SystemTime> {
        fs::metadata(&self.path).and_then(|m| m.modified()).ok()
    }

    /// Güncel içerik + sürüm; dosya değiştiyse yeniden okur (0.6.9 `Get`). Okunamazsa eski içerik kalır, bir kez günlüğe yazılır.
    pub fn snapshot(&self) -> (Arc<Value>, i64) {
        let mut c = self.lock();
        let stamp = self.stamp();
        if stamp != c.stamp && stamp != c.bad_stamp {
            match fs::read(&self.path).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string())) {
                Ok(v) if v.is_object() => {
                    c.value = Arc::new(v);
                    c.stamp = stamp;
                    c.bad_stamp = None;
                }
                Ok(_) => {
                    eprintln!("phoned: ayar dosyası nesne değil ({}), eski içerik kullanılıyor", self.path.display());
                    c.bad_stamp = stamp;
                }
                Err(e) => {
                    eprintln!("phoned: ayar dosyası okunamadı ({}): {e}", self.path.display());
                    c.bad_stamp = stamp;
                }
            }
        }
        (Arc::clone(&c.value), c.stamp.map_or(0, ticks))
    }

    #[must_use]
    pub fn version(&self) -> i64 {
        self.snapshot().1
    }

    /// Doğrula ve kaydet; yeni sürümü döndürür.
    ///
    /// # Errors
    /// `Conflict` (beklenen sürüm tutmuyor) · `Invalid` (0.6.9 doğrulama metinleri) · `Io`.
    pub fn save(&self, body: &str, expected: Option<i64>) -> Result<i64, SaveError> {
        let value: Value = serde_json::from_str(body).map_err(|e| SaveError::Invalid(format!("Invalid JSON: {e}")))?;
        validate(&value).map_err(SaveError::Invalid)?;
        let bytes = serde_json::to_vec_pretty(&value).map_err(|e| SaveError::Invalid(format!("Invalid JSON: {e}")))?;
        let mut c = self.lock();
        if let (Some(exp), Some(cur)) = (expected, self.stamp())
            && ticks(cur) != exp
        {
            return Err(SaveError::Conflict);
        }
        write_atomic(&self.path, &bytes)?;
        let stamp = self.stamp();
        c.stamp = stamp;
        c.bad_stamp = None;
        c.value = Arc::new(value);
        Ok(stamp.map_or(0, ticks))
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(path.as_os_str());
    s.push(suffix);
    PathBuf::from(s)
}

/// Geçici dosya + fsync + `.bak` + atomik rename + dizin fsync; tüm dosyalar mod 600 (dosya Wi-Fi anahtarı taşır).
///
/// # Errors
/// G/Ç hatası.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = sibling(path, ".tmp");
    let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.set_permissions(Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    if path.exists() {
        let bak = sibling(path, ".bak");
        fs::copy(path, &bak)?;
        fs::set_permissions(&bak, Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, path)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn hh_mm(s: &str) -> bool {
    let b = s.as_bytes();
    let [h1, h2, b':', m1, m2] = b else { return false };
    let digit = |c: &u8| c.is_ascii_digit();
    if ![h1, h2, m1, m2].into_iter().all(digit) {
        return false;
    }
    let hour = (h1 - b'0') * 10 + (h2 - b'0');
    hour <= 23 && *m1 <= b'5'
}

/// 0.6.9 `Save` doğrulaması (`Config.cs:366-369`), aynı ileti metinleriyle.
///
/// # Errors
/// İnsan okunur ileti.
pub fn validate(v: &Value) -> Result<(), String> {
    let Some(o) = v.as_object() else {
        return Err("Invalid JSON: the settings must be an object".into());
    };
    if let Some(list) = o.get("shortcuts").and_then(Value::as_array) {
        for s in list {
            let cmd = s.get("cmd").and_then(Value::as_str).unwrap_or("");
            if cmd.trim().is_empty() {
                let label = s.get("label").and_then(Value::as_str).unwrap_or("").trim();
                return Err(format!("Shortcut '{label}' has no command"));
            }
        }
    }
    if let Some(w) = o.get("weather") {
        let lat = w.get("lat").and_then(Value::as_f64).unwrap_or(0.0);
        let lon = w.get("lon").and_then(Value::as_f64).unwrap_or(0.0);
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return Err("Invalid weather location".into());
        }
    }
    if let Some(n) = o.get("night") {
        for key in ["start", "end"] {
            if let Some(t) = n.get(key)
                && !t.as_str().is_some_and(hh_mm)
            {
                return Err("Night mode times must be in HH:MM format".into());
            }
        }
    }
    Ok(())
}

fn field(v: &Value, key: &str) -> Value {
    v.get(key).cloned().unwrap_or(Value::Null)
}

/// 0.6.9 `PagesFor`: telefonun seçili sayfaları, yoksa hepsi.
fn pages_for<'a>(cfg: &'a Value, device: Option<&str>) -> Vec<&'a Value> {
    let all: Vec<&Value> = cfg.get("pages").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let chosen_ids: Vec<&str> = device
        .and_then(|d| cfg.get("phones")?.as_array()?.iter().find(|p| p.get("id").and_then(Value::as_str) == Some(d)))
        .and_then(|p| p.get("pages")?.as_array())
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if chosen_ids.is_empty() {
        return all;
    }
    let chosen: Vec<&Value> =
        all.iter().copied().filter(|p| p.get("id").and_then(Value::as_str).is_some_and(|id| chosen_ids.contains(&id))).collect();
    if chosen.is_empty() { all } else { chosen }
}

fn public_widget(w: &Value) -> Value {
    let kind = w.get("type").and_then(Value::as_str).unwrap_or("");
    let options = if kind == "command" {
        match w.get("options").and_then(Value::as_object) {
            Some(o) => Value::Object(
                o.iter().filter(|(k, _)| k.as_str() == "title" || k.as_str() == "interval").map(|(k, v)| (k.clone(), v.clone())).collect(),
            ),
            None => Value::Null,
        }
    } else {
        field(w, "options")
    };
    json!({ "id": field(w, "id"), "type": field(w, "type"), "x": field(w, "x"), "y": field(w, "y"), "w": field(w, "w"), "h": field(w, "h"), "options": options })
}

/// `display` + `homePage` (0.6.9 `Normalize` boşu `null` yapar, `ToPublic` null'u da yazar).
fn public_display(cfg: &Value) -> Value {
    let mut d = field(cfg, "display");
    if let Some(o) = d.as_object_mut() {
        let home = o.get("homePage").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        o.insert("homePage".into(), home.map_or(Value::Null, Value::String));
    }
    d
}

/// 0.6.9 `AgentConfig.ToPublic` (`Config.cs:41-60`): telefona giden kısım; komutlar, betikler ve Wi-Fi anahtarı GİTMEZ.
#[must_use]
pub fn to_public(cfg: &Value, device: Option<&str>) -> Value {
    let pages: Vec<Value> = pages_for(cfg, device)
        .into_iter()
        .map(|p| {
            let widgets: Vec<Value> = p.get("widgets").and_then(Value::as_array).map(|ws| ws.iter().map(public_widget).collect()).unwrap_or_default();
            json!({ "id": field(p, "id"), "title": field(p, "title"), "icon": field(p, "icon"), "enabled": field(p, "enabled"), "idleSec": field(p, "idleSec"), "widgets": widgets })
        })
        .collect();
    let list = |key: &str| cfg.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let shortcuts: Vec<Value> = list("shortcuts")
        .iter()
        .map(|s| {
            let toggle = s.get("toggle").and_then(Value::as_bool).unwrap_or(false) || s.get("cmd").and_then(Value::as_str) == Some("builtin:mute");
            json!({ "id": field(s, "id"), "label": field(s, "label"), "icon": field(s, "icon"), "color": field(s, "color"), "toggle": toggle })
        })
        .collect();
    let rules: Vec<Value> = list("rules")
        .iter()
        .map(|r| json!({ "id": field(r, "id"), "name": field(r, "name"), "enabled": field(r, "enabled"), "when": field(r, "when") }))
        .collect();
    let charge = cfg.get("charge").map_or(Value::Null, |c| json!({ "enabled": field(c, "enabled"), "min": field(c, "min"), "max": field(c, "max") }));
    json!({
        "weather": field(cfg, "weather"),
        "night": field(cfg, "night"),
        "display": public_display(cfg),
        "pages": pages,
        "shortcuts": shortcuts,
        "rules": rules,
        "charge": charge,
    })
}

/// `state.toggles`: `toggle` ya da `builtin:mute` kısayolları → durum (W2.4'e kadar ses/push durumu yok → `false`). Yoksa `null`.
#[must_use]
pub fn toggles(cfg: &Value) -> Value {
    let map: Map<String, Value> = cfg
        .get("shortcuts")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|s| s.get("toggle").and_then(Value::as_bool).unwrap_or(false) || s.get("cmd").and_then(Value::as_str) == Some("builtin:mute"))
                .filter_map(|s| Some((s.get("id")?.as_str()?.to_owned(), Value::Bool(false))))
                .collect()
        })
        .unwrap_or_default();
    if map.is_empty() { Value::Null } else { Value::Object(map) }
}

/// `night.enabled` ve yerel dakika `[start, end)` penceresinde mi (gece yarısını aşan pencere dahil).
#[must_use]
pub fn night_active(cfg: &Value, minute_of_day: u32) -> bool {
    let Some(n) = cfg.get("night") else { return false };
    if !n.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        return false;
    }
    let parse = |k: &str| -> Option<u32> {
        let s = n.get(k)?.as_str()?;
        let (h, m) = s.split_once(':')?;
        Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
    };
    let (Some(start), Some(end)) = (parse("start"), parse("end")) else { return false };
    if start <= end { (start..end).contains(&minute_of_day) } else { minute_of_day >= start || minute_of_day < end }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hh_mm_matches_dotnet_regex() {
        for ok in ["00:00", "09:30", "23:59", "19:05"] {
            assert!(hh_mm(ok), "{ok}");
        }
        for bad in ["24:00", "9:30", "25:00", "12:60", "ab:cd", "12:5", ""] {
            assert!(!hh_mm(bad), "{bad}");
        }
    }

    #[test]
    fn night_window() {
        let cfg = json!({"night": {"enabled": true, "start": "23:00", "end": "07:00"}});
        assert!(night_active(&cfg, 23 * 60));
        assert!(night_active(&cfg, 3 * 60));
        assert!(!night_active(&cfg, 7 * 60));
        assert!(!night_active(&cfg, 12 * 60));
        let day = json!({"night": {"enabled": true, "start": "08:00", "end": "10:00"}});
        assert!(night_active(&day, 9 * 60));
        assert!(!night_active(&day, 10 * 60));
        let off = json!({"night": {"enabled": false, "start": "00:00", "end": "23:59"}});
        assert!(!night_active(&off, 60));
    }

    #[test]
    fn ticks_matches_dotnet_epoch() {
        assert_eq!(ticks(UNIX_EPOCH), UNIX_EPOCH_TICKS);
        assert_eq!(ticks(UNIX_EPOCH + std::time::Duration::from_secs(1)), UNIX_EPOCH_TICKS + 10_000_000);
    }
}
