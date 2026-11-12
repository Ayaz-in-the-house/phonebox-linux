//! `media` konusu: MPRIS (oturum veri yolu, zbus) — olay güdümlü, yoklama yok, dış süreç (`playerctl`) yok (TP-W2-07).
//!
//! Kaynaklar: `org.freedesktop.DBus.Properties.PropertiesChanged` (yol `/org/mpris/MediaPlayer2`) · `Player.Seeked` ·
//! `NameOwnerChanged` (oyuncu açıldı/kapandı). Bir sinyal gelince yalnız o oyuncunun `GetAll`'u okunur. Konum (Position)
//! PropertiesChanged ile gelmez (MPRIS belirtimi): son okunan konum + çalıyorsa geçen süre (yerel saat) — 0.6.9'un 1 sn
//! yoklamasıyla aynı görünür sonuç.
//!
//! Ses (`state.audio`, `POST /api/volume`) bu modülde değil: PipeWire'ın ana düzey için D-Bus arayüzü yok (ölçüldü 2026-11-10)
//! → W2.4b `crate::audio` (pulse yerel soketi, olay güdümlü).

pub mod dbus;
pub mod http;
pub mod yt;

use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// MPRIS veri yolu adı öneki.
pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// 0.6.9 `MediaSnapshot` (camelCase).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSnapshot {
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub pos_ms: i64,
    pub dur_ms: i64,
    pub art_id: u64,
    pub can_seek: bool,
    /// K8: `"mpris"` | `"yt"` (deckd Zen eklentisi).
    pub source: String,
    /// K8: konum gerçek kaynaktan mı (Position + position state, Seeked ya da eklenti). `false` → panel konum göstermez.
    pub pos_known: bool,
    /// K8 (yalnız `yt`): sıradaki video.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<MediaItem>,
    /// K8 (yalnız `yt`): oynatma listesi penceresi (seçiliden 2 önce, en çok 30).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<MediaItem>,
    /// K8: listede seçili sıra (0 tabanlı) ve liste uzunluğu.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_index: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_len: Option<u64>,
    /// K8: YouTube reklamı gösteriliyor (konuma gitme kapalı).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ad: bool,
}

/// Sıradaki / liste öğesi; `artId` 0 → görsel yok (henüz gelmedi).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaItem {
    pub title: String,
    pub art_id: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cur: bool,
}

/// Bir oyuncunun son okunan özellikleri (D-Bus'tan bağımsız düz veri; saf eşleme bunun üstünde test edilir).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerProps {
    /// `Playing` | `Paused` | `Stopped`.
    pub status: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub length_us: i64,
    pub position_us: i64,
    pub can_seek: bool,
    pub art_url: String,
    pub track_id: String,
    /// `xesam:url` (Zen: sekme adresi) — parça kimliği.
    pub url: String,
    /// Kök arayüz `org.mpris.MediaPlayer2.DesktopEntry` (ör. `zen`); boş olabilir.
    pub desktop_entry: String,
    /// Kök arayüz `org.mpris.MediaPlayer2.Identity` (ör. `Mozilla zen`); boş olabilir.
    pub identity: String,
}

struct Player {
    props: PlayerProps,
    /// Son GERÇEK konum okuması; `None` → konum bilinmiyor.
    anchor: Option<Anchor>,
}

/// Konum çapası: `pos_us` anında `at`; `playing` iken gerçek zamanla ilerler (MPRIS: Rate 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub pos_us: i64,
    pub at: Instant,
    pub playing: bool,
}

impl Anchor {
    /// `now` anındaki konum (µs), `len_us > 0` ise onunla sınırlı.
    #[must_use]
    pub fn pos_at(&self, now: Instant, len_us: i64) -> i64 {
        let mut pos = self.pos_us.max(0);
        if self.playing {
            let el = i64::try_from(now.saturating_duration_since(self.at).as_micros()).unwrap_or(i64::MAX);
            pos = pos.saturating_add(el);
        }
        if len_us > 0 { pos.min(len_us) } else { pos }
    }
}

/// Parça kimliği: `xesam:url` → `mpris:trackid` → başlık (boşsa kimlik yok, hiçbir şey taşınmaz).
#[must_use]
pub fn track_key(p: &PlayerProps) -> &str {
    [&p.url, &p.track_id, &p.title].into_iter().find(|s| !s.is_empty()).map_or("", String::as_str)
}

/// K8: yeni okuma + önceki durum → yeni durum (saf).
/// Firefox/Zen `mpris:length` ve `Position`'ı yalnız position state varken verir (MPRISServiceHandler); yokken Position = 0
/// sahte bir değerdir. Bu yüzden: uzunluk > 0 olan okuma = gerçek konum (çapa yenilenir); uzunluksuz okumada aynı parça ise
/// son uzunluk ve çapa korunur (çalma durumu değiştiyse çapa o ana taşınır), başka parça ise konum bilinmiyor.
fn merge(prev: Option<&Player>, mut p: PlayerProps, now: Instant) -> Player {
    let playing = p.status == "Playing";
    let key = track_key(&p);
    let same = prev.filter(|o| !key.is_empty() && track_key(&o.props) == key);
    let anchor = if p.length_us > 0 {
        Some(Anchor { pos_us: p.position_us.max(0), at: now, playing })
    } else if let Some(o) = same {
        p.length_us = o.props.length_us;
        o.anchor.map(|a| if a.playing == playing { a } else { Anchor { pos_us: a.pos_at(now, p.length_us), at: now, playing } })
    } else {
        None
    };
    Player { props: p, anchor }
}

/// "org.mpris.MediaPlayer2.firefox.instance_1_42" → "Firefox" (0.6.9: playerctl `playerName`, ilk nokta parçası, Baş Harf).
#[must_use]
pub fn app_name(bus: &str) -> String {
    let short = bus.strip_prefix(MPRIS_PREFIX).unwrap_or(bus);
    let first = short.split('.').next().unwrap_or(short);
    let mut c = first.chars();
    c.next().map(|h| h.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
}

/// Panelde görünen oyuncu adı: `DesktopEntry` (son nokta sonrası, `org.mozilla.firefox` → Firefox) → `Identity`
/// (`Mozilla ` öneki atılır) → veri yolu adı. Zen MPRIS'i `firefox.instance_…` adıyla yayımlar (ölçüldü 2026-11-10).
#[must_use]
pub fn app_label(bus: &str, p: &PlayerProps) -> String {
    let title = |w: &str| {
        let mut c = w.chars();
        c.next().map(|h| h.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
    };
    if let Some(last) = p.desktop_entry.rsplit('.').next().filter(|w| !w.is_empty()) {
        return title(last);
    }
    let id = p.identity.trim();
    let id = id.strip_prefix("Mozilla ").unwrap_or(id).trim();
    if !id.is_empty() {
        return title(id);
    }
    app_name(bus)
}

/// Gösterilecek oyuncu: çalan önce, sonra duraklatılmış; `Stopped` yok sayılır (0.6.9 playerctl de durmuş oyuncuyu göstermez).
#[must_use]
pub fn choose<'a>(players: impl IntoIterator<Item = (&'a str, &'a PlayerProps)>) -> Option<(&'a str, &'a PlayerProps)> {
    let mut paused = None;
    for (name, p) in players {
        match p.status.as_str() {
            "Playing" => return Some((name, p)),
            "Paused" if paused.is_none() => paused = Some((name, p)),
            _ => {}
        }
    }
    paused
}

/// Düz özellik + konum (ms; `None` = bilinmiyor) → panel şekli.
#[must_use]
pub fn to_snapshot(bus: &str, p: &PlayerProps, pos_ms: Option<i64>, art_id: u64) -> MediaSnapshot {
    let playing = p.status == "Playing";
    let dur_ms = p.length_us.max(0) / 1000;
    let mut pos = pos_ms.unwrap_or(0).max(0);
    if dur_ms > 0 {
        pos = pos.min(dur_ms);
    }
    MediaSnapshot {
        app: app_label(bus, p),
        title: p.title.clone(),
        artist: p.artists.join(", "),
        album: p.album.clone(),
        playing,
        pos_ms: pos,
        dur_ms,
        art_id,
        can_seek: p.can_seek && dur_ms > 0,
        source: "mpris".to_owned(),
        pos_known: pos_ms.is_some(),
        next: None,
        queue: Vec::new(),
        queue_index: None,
        queue_len: None,
        ad: false,
    }
}

/// Kapak: yalnız yerel `file://` görüntü dosyası (≤ 4 MB). `http(s)` kapaklar indirilmez (ağ istemcisi yok) → 404.
#[must_use]
pub fn art_path(url: &str) -> Option<std::path::PathBuf> {
    let raw = url.strip_prefix("file://")?;
    let decoded = percent_decode(raw)?;
    let lower = decoded.to_ascii_lowercase();
    let image = [".png", ".jpg", ".jpeg", ".webp"].iter().any(|e| lower.ends_with(e));
    (image && decoded.starts_with('/') && !decoded.contains("/../")).then(|| std::path::PathBuf::from(decoded))
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub const ART_MAX: u64 = 4 << 20;

/// MIME türü dosya adından.
#[must_use]
pub fn art_mime(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    }
}

/// Konu durumu + K7 sayaçları.
pub struct Media {
    players: Mutex<BTreeMap<String, Player>>,
    art: Mutex<(String, u64)>,
    /// Kaynak durumu: `"connecting" | "mpris" | "unavailable: …"`.
    pub source: Mutex<String>,
    pub events: AtomicU64,
    pub refreshes: AtomicU64,
    pub(crate) conn: std::sync::OnceLock<zbus::Connection>,
    /// Değişim tiki (W2.5): akış sessiz kipte de bekleyen tik olmadan `state` yollar (ses W2.4b eşi).
    tick: tokio::sync::watch::Sender<u64>,
    /// K8: deckd Zen eklentisinin YouTube durumu + görselleri (`yt.rs`).
    yt: Mutex<Option<yt::YtNow>>,
    yt_art: Mutex<std::collections::VecDeque<(u64, Vec<u8>)>>,
}

impl Default for Media {
    fn default() -> Self {
        Self::new()
    }
}

impl Media {
    #[must_use]
    pub fn new() -> Self {
        Self {
            players: Mutex::new(BTreeMap::new()),
            art: Mutex::new((String::new(), 0)),
            source: Mutex::new("connecting".to_owned()),
            events: AtomicU64::new(0),
            refreshes: AtomicU64::new(0),
            conn: std::sync::OnceLock::new(),
            tick: tokio::sync::watch::channel(0).0,
            yt: Mutex::new(None),
            yt_art: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    /// Değişim tiki aboneliği.
    #[must_use]
    pub fn watch(&self) -> tokio::sync::watch::Receiver<u64> {
        self.tick.subscribe()
    }

    /// Oyuncuyu yaz (D-Bus okuması ya da test).
    pub fn put(&self, bus: &str, props: PlayerProps) {
        self.put_at(bus, props, Instant::now());
    }

    pub fn put_at(&self, bus: &str, props: PlayerProps, now: Instant) {
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        let changed = self.players.lock().is_ok_and(|mut g| {
            let next = merge(g.get(bus), props, now);
            let changed = g.get(bus).is_none_or(|p| p.props != next.props);
            g.insert(bus.to_owned(), next);
            changed
        });
        self.bump(changed);
    }

    pub fn remove(&self, bus: &str) {
        let changed = self.players.lock().is_ok_and(|mut g| g.remove(bus).is_some());
        self.bump(changed);
    }

    fn bump(&self, changed: bool) {
        if changed {
            self.tick.send_modify(|t| *t = t.wrapping_add(1));
        }
    }

    #[must_use]
    pub fn player_count(&self) -> usize {
        self.players.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Seçilen oyuncunun veri yolu adı ve parça kimliği (denetim çağrıları için).
    #[must_use]
    pub fn current(&self) -> Option<(String, PlayerProps)> {
        let g = self.players.lock().ok()?;
        choose(g.iter().map(|(k, v)| (k.as_str(), &v.props))).map(|(k, p)| (k.to_owned(), p.clone()))
    }

    /// `state.media` (oyuncu yoksa `None` → JSON `null`).
    #[must_use]
    pub fn snapshot(&self) -> Option<MediaSnapshot> {
        self.snapshot_at(Instant::now())
    }

    #[must_use]
    pub fn snapshot_at(&self, now: Instant) -> Option<MediaSnapshot> {
        let mp = self.mpris_at(now);
        match self.yt_now() {
            Some(y) if yt::prefer(&y.s, mp.as_ref().map(|(s, url)| (s.playing, url.as_str()))) => Some(self.yt_snapshot(&y, now)),
            _ => mp.map(|(s, _)| s),
        }
    }

    /// MPRIS seçimi + oyuncunun `xesam:url`'si (YouTube ile aynı sekme mi kararı için).
    fn mpris_at(&self, now: Instant) -> Option<(MediaSnapshot, String)> {
        let g = self.players.lock().ok()?;
        let (bus, p) = choose(g.iter().map(|(k, v)| (k.as_str(), &v.props)))?;
        let pos = g.get(bus).and_then(|pl| pl.anchor).map(|a| a.pos_at(now, p.length_us) / 1000);
        let art_id = self.art_id(&p.art_url);
        Some((to_snapshot(bus, p, pos, art_id), p.url.clone()))
    }

    /// Kapak URL'si değişince kimlik artar (panel `artId` değişince `/api/art`'ı yeniden ister); yüklenemeyen kapak 0.
    fn art_id(&self, url: &str) -> u64 {
        if art_path(url).is_none() {
            return 0;
        }
        let Ok(mut a) = self.art.lock() else { return 0 };
        if a.0 != url {
            a.0 = url.to_owned();
            a.1 = a.1.wrapping_add(1).max(1);
        }
        a.1
    }

    /// Şimdiki kapağın dosya yolu.
    #[must_use]
    pub fn art_file(&self) -> Option<std::path::PathBuf> {
        art_path(&self.current()?.1.art_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(status: &str, title: &str) -> PlayerProps {
        PlayerProps {
            status: status.into(),
            title: title.into(),
            artists: vec!["A".into(), "B".into()],
            length_us: 200_000_000,
            position_us: 10_000_000,
            can_seek: true,
            ..PlayerProps::default()
        }
    }

    /// TP-W2-07b (kullanıcı 2026-11-10: "muzik su an zen den acik, onu gormuyor"): Zen MPRIS'i `firefox.instance_…`
    /// adıyla yayımlar; panel "Firefox" yazıyordu. Ad önce `DesktopEntry`, sonra `Identity` (Mozilla öneki atılır), en son veri yolu adı.
    #[test]
    fn tp_w2_07b_app_label_prefers_desktop_entry() {
        let bus = "org.mpris.MediaPlayer2.firefox.instance_1_429496";
        let mut p = props("Playing", "t");
        assert_eq!(app_label(bus, &p), "Firefox");
        p.identity = "Mozilla zen".into();
        assert_eq!(app_label(bus, &p), "Zen");
        p.desktop_entry = "zen".into();
        assert_eq!(app_label(bus, &p), "Zen");
        p.desktop_entry = "org.mozilla.firefox".into();
        p.identity = "Mozilla Firefox".into();
        assert_eq!(app_label(bus, &p), "Firefox");
        p.desktop_entry = "spotify".into();
        assert_eq!(app_label("org.mpris.MediaPlayer2.spotify", &p), "Spotify");
        assert_eq!(to_snapshot(bus, &PlayerProps { desktop_entry: "zen".into(), ..props("Playing", "t") }, Some(0), 1).app, "Zen");
    }

    fn yt_props(status: &str, url: &str, len_us: i64, pos_us: i64) -> PlayerProps {
        PlayerProps {
            status: status.into(),
            title: "v".into(),
            url: url.into(),
            length_us: len_us,
            position_us: pos_us,
            can_seek: true,
            ..PlayerProps::default()
        }
    }

    const ZEN: &str = "org.mpris.MediaPlayer2.firefox.instance_1_429496";

    /// TP-K8-M1 (kullanıcı: "ilk actigimda bildiriyordu"): Firefox position state düşünce Metadata'dan `mpris:length` düşer
    /// (MPRISServiceHandler::GetMetadataAsGVariant). Aynı parça (xesam:url) → son bilinen uzunluk korunur.
    #[test]
    fn tp_k8_m1_same_track_keeps_length() {
        let m = Media::new();
        let t0 = Instant::now();
        m.put_at(ZEN, yt_props("Playing", "https://www.youtube.com/watch?v=a", 200_000_000, 10_000_000), t0);
        m.put_at(ZEN, yt_props("Playing", "https://www.youtube.com/watch?v=a", 0, 0), t0);
        let s = m.snapshot_at(t0).unwrap();
        assert_eq!(s.dur_ms, 200_000, "aynı parça: uzunluk korunur");
        assert!(s.can_seek);
    }

    /// TP-K8-M2: başka parça (url değişti) uzunluksuz gelirse eski parçanın uzunluğu TAŞINMAZ.
    #[test]
    fn tp_k8_m2_new_track_does_not_inherit_length() {
        let m = Media::new();
        let t0 = Instant::now();
        m.put_at(ZEN, yt_props("Playing", "https://www.youtube.com/watch?v=a", 200_000_000, 10_000_000), t0);
        m.put_at(ZEN, yt_props("Playing", "https://www.youtube.com/watch?v=b", 0, 0), t0);
        let s = m.snapshot_at(t0).unwrap();
        assert_eq!(s.dur_ms, 0);
        assert!(!s.pos_known, "yeni parçada konum bilinmiyor");
        assert!(!s.can_seek);
    }

    /// TP-K8-M3 (koordinatör ölçümü: phoned yeniden başlayınca 3 sn): position state yokken Firefox Position=0 döner →
    /// bu gerçek konum değildir; panel 0:00'dan saymasın, `posKnown=false`.
    #[test]
    fn tp_k8_m3_position_without_state_is_unknown() {
        let m = Media::new();
        let t0 = Instant::now();
        m.put_at(ZEN, yt_props("Playing", "https://www.youtube.com/watch?v=a", 0, 0), t0);
        let s = m.snapshot_at(t0 + std::time::Duration::from_secs(3)).unwrap();
        assert!(!s.pos_known);
        assert_eq!(s.pos_ms, 0);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v.get("posKnown"), Some(&serde_json::Value::Bool(false)));
        assert_eq!(v.get("source").and_then(|x| x.as_str()), Some("mpris"));
    }

    /// TP-K8-M4: uzunluk düştükten sonra konum son GERÇEK okumadan sürer; duraklatınca donar; yenileme saati sıfırlamaz.
    #[test]
    fn tp_k8_m4_anchor_survives_and_freezes_on_pause() {
        let m = Media::new();
        let t0 = Instant::now();
        let sec = |n: u64| t0 + std::time::Duration::from_secs(n);
        let u = "https://www.youtube.com/watch?v=a";
        m.put_at(ZEN, yt_props("Playing", u, 200_000_000, 10_000_000), t0);
        m.put_at(ZEN, yt_props("Playing", u, 0, 0), sec(5));
        let s = m.snapshot_at(sec(8)).unwrap();
        assert!(s.pos_known);
        assert_eq!(s.pos_ms, 18_000, "10 sn + 8 sn (5. sn'deki uzunluksuz yenileme saati sıfırlamaz)");
        m.put_at(ZEN, yt_props("Paused", u, 0, 0), sec(10));
        assert_eq!(m.snapshot_at(sec(30)).unwrap().pos_ms, 20_000, "duraklatınca donar");
        m.put_at(ZEN, yt_props("Playing", u, 0, 0), sec(40));
        assert_eq!(m.snapshot_at(sec(41)).unwrap().pos_ms, 21_000, "devam edince donduğu yerden");
    }

    /// TP-K8-M5: gerçek okuma (uzunluk var) her zaman çapayı yeniler; üst sınır uzunluk.
    #[test]
    fn tp_k8_m5_real_read_reanchors() {
        let m = Media::new();
        let t0 = Instant::now();
        let u = "https://www.youtube.com/watch?v=a";
        m.put_at(ZEN, yt_props("Playing", u, 200_000_000, 10_000_000), t0);
        m.put_at(ZEN, yt_props("Playing", u, 200_000_000, 50_000_000), t0 + std::time::Duration::from_secs(1));
        assert_eq!(m.snapshot_at(t0 + std::time::Duration::from_secs(2)).unwrap().pos_ms, 51_000);
        assert_eq!(m.snapshot_at(t0 + std::time::Duration::from_secs(900)).unwrap().pos_ms, 200_000);
    }

    #[test]
    fn tp_w24_m1_app_name() {
        assert_eq!(app_name("org.mpris.MediaPlayer2.spotify"), "Spotify");
        assert_eq!(app_name("org.mpris.MediaPlayer2.firefox.instance_1_42"), "Firefox");
        assert_eq!(app_name("org.mpris.MediaPlayer2.VLC"), "Vlc");
        assert_eq!(app_name(""), "");
    }

    #[test]
    fn tp_w24_m1_choose_playing_over_paused() {
        let a = props("Paused", "a");
        let b = props("Playing", "b");
        let c = props("Stopped", "c");
        assert_eq!(choose([("x", &a), ("y", &b)]).map(|(n, _)| n), Some("y"));
        assert_eq!(choose([("x", &a), ("z", &c)]).map(|(n, _)| n), Some("x"));
        assert_eq!(choose([("z", &c)]), None);
    }

    #[test]
    fn tp_w24_m1_snapshot_shape_and_position() {
        let s = to_snapshot("org.mpris.MediaPlayer2.spotify", &props("Playing", "Song"), Some(11_500), 3);
        assert_eq!(
            s,
            MediaSnapshot {
                app: "Spotify".into(),
                title: "Song".into(),
                artist: "A, B".into(),
                album: String::new(),
                playing: true,
                pos_ms: 11_500,
                dur_ms: 200_000,
                art_id: 3,
                can_seek: true,
                source: "mpris".into(),
                pos_known: true,
                next: None,
                queue: vec![],
                queue_index: None,
                queue_len: None,
                ad: false
            }
        );
        let t0 = Instant::now();
        let a = Anchor { pos_us: 10_000_000, at: t0, playing: false };
        assert_eq!(a.pos_at(t0 + std::time::Duration::from_secs(99), 200_000_000), 10_000_000, "duraklatılmışken konum ilerlemez");
        let over = to_snapshot("x", &props("Playing", "S"), Some(999_000_000), 0);
        assert_eq!(over.pos_ms, 200_000, "süreyi aşmaz");
        let v = serde_json::to_value(&s).unwrap();
        for k in ["app", "title", "artist", "album", "playing", "posMs", "durMs", "artId", "canSeek", "source", "posKnown"] {
            assert!(v.get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn tp_w24_m1_media_state_and_art() {
        let m = Media::new();
        assert_eq!(m.snapshot(), None, "oyuncu yok → null");
        let mut p = props("Playing", "S");
        p.art_url = "file:///tmp/cover%20a.png".into();
        m.put("org.mpris.MediaPlayer2.spotify", p.clone());
        let s = m.snapshot().unwrap();
        assert_eq!(s.art_id, 1);
        assert_eq!(m.snapshot().unwrap().art_id, 1, "aynı kapak aynı kimlik");
        p.art_url = "file:///tmp/b.jpg".into();
        m.put("org.mpris.MediaPlayer2.spotify", p);
        assert_eq!(m.snapshot().unwrap().art_id, 2);
        assert_eq!(art_path("file:///tmp/cover%20a.png").unwrap().to_str(), Some("/tmp/cover a.png"));
        assert_eq!(art_path("https://i.scdn.co/x"), None);
        assert_eq!(art_path("file:///etc/passwd"), None);
        assert_eq!(art_path("file:///a/../etc/x.png"), None);
        m.remove("org.mpris.MediaPlayer2.spotify");
        assert_eq!(m.snapshot(), None);
    }
}
