//! K8 YouTube kaynağı (kullanıcı 2026-11-10: "gerekirse youtube clienti gibi de degerlendirebilmeliyiz bu page i").
//!
//! deckd Zen eklentisi 0.7.0+ YouTube sekmesinden video öğesini okur → native host (deckd) `deckd-zen-tabs.sock`'a
//! satır yayınlar: `{"yt":{..}}` (sekme gidince `{"yt":null}`) + görseller `{"art":{"k","c","d"}}`. phoned bu sokete salt
//! istemci olarak bağlanır (süreç başlatma yok, yoklama yok; host yoksa 2 sn'de bir bağlanmayı dener).
//! Denetim: `deckd-zen.sock`'a `yt <op> [ms]` — yalnız host ikilisi `deckd-host-proto:yt` taşıyorsa (eski host bilinmeyen
//! satırı adres sanıp sekme açar: deckd E18).
//! Seçim: YouTube çalıyorsa ya da MPRIS'te çalan başka oyuncu yoksa YouTube; aynı sekmenin MPRIS'i (Zen) YouTube'a yenilir.

use super::{Media, MediaItem, MediaSnapshot};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

pub const YT_MARK: &[u8] = b"deckd-host-proto:yt";
const TEXT_MAX: usize = 120;
const QUEUE_MAX: usize = 30;
/// Görsel önbelleği: en çok 48 görsel, her biri ≤ 256 KiB (eklenti 128×72 JPEG yollar, ~4 KB).
const ART_KEEP: usize = 48;
const ART_BYTES_MAX: usize = 256 << 10;
/// Yalnız konum değiştiyse ve beklenen konumdan sapma bundan küçükse akış uyandırılmaz (panel kendisi sayar).
const DRIFT_MS: i64 = 1_500;
const WAIT: Duration = Duration::from_millis(1_500);
const RETRY: Duration = Duration::from_secs(2);

/// Sıradaki / liste öğesi (eklentinin alanları).
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(default)]
pub struct YtItem {
    pub title: String,
    pub id: String,
    pub k: String,
    pub cur: bool,
}

/// Eklentinin YouTube durumu (`MEDIA-YT-2026-11-10.md` § Sözleşme). Saniye cinsinden zaman.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(default)]
pub struct YtState {
    pub tab: u64,
    pub id: String,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub cur: f64,
    pub dur: f64,
    pub paused: bool,
    pub rate: f64,
    pub ad: bool,
    pub k: String,
    pub next: Option<YtItem>,
    pub queue: Vec<YtItem>,
    pub qi: i64,
    pub qn: u64,
}

fn clip(s: &mut String) {
    if let Some((i, _)) = s.char_indices().nth(TEXT_MAX) {
        s.truncate(i);
    }
}

fn secs(x: f64) -> f64 {
    if x.is_finite() && x > 0.0 { x } else { 0.0 }
}

impl YtState {
    /// Dış veri girişte bir kez düzeltilir: sonlu ve ≥ 0 zaman, hız (0, 16] (yoksa 1), metin ≤ 120 karakter, liste ≤ 30.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.cur = secs(self.cur);
        self.dur = secs(self.dur);
        if !(self.rate.is_finite() && self.rate > 0.0 && self.rate <= 16.0) {
            self.rate = 1.0;
        }
        self.queue.truncate(QUEUE_MAX);
        for s in [&mut self.title, &mut self.channel] {
            clip(s);
        }
        for it in self.next.iter_mut().chain(self.queue.iter_mut()) {
            clip(&mut it.title);
        }
        self
    }

    /// `now` anındaki konum (ms): çalıyorsa geçen süre × hız; süreyle sınırlı.
    #[must_use]
    pub fn pos_ms(&self, at: Instant, now: Instant) -> i64 {
        let mut ms = self.cur * 1000.0;
        if !self.paused {
            ms += now.saturating_duration_since(at).as_secs_f64() * 1000.0 * self.rate;
        }
        if self.dur > 0.0 {
            ms = ms.min(self.dur * 1000.0);
        }
        ms as i64
    }

    fn fixed(&self) -> Self {
        Self { cur: 0.0, ..self.clone() }
    }
}

/// Host satırı.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant, reason = "satır başına bir kez kurulup hemen tüketilir; saklanmaz")]
pub enum Line {
    Yt(Option<YtState>),
    Art(u64, Vec<u8>),
}

/// Görsel anahtarı (eklenti FNV-1a, ≤ 16 onaltılık) → kimlik; 0 kimlik değildir.
#[must_use]
pub fn key_id(k: &str) -> Option<u64> {
    (!k.is_empty() && k.len() <= 16).then(|| u64::from_str_radix(k, 16).ok()).flatten().filter(|&n| n != 0)
}

/// Standart base64 (dolgu isteğe bağlı); bozuksa `None`.
#[must_use]
pub fn b64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let body = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in body {
        acc = (acc << 6) | u32::from(val(c)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    (bits < 6).then_some(out)
}

/// Host satırını ayrıştır; ilgisiz satır (sekme görüntüsü) `None`.
#[must_use]
pub fn parse_line(line: &str) -> Option<Line> {
    if line.starts_with(r#"{"yt""#) {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let y = v.get("yt")?;
        if y.is_null() {
            return Some(Line::Yt(None));
        }
        let s: YtState = serde_json::from_value(y.clone()).ok()?;
        return Some(Line::Yt(Some(s.sanitized())));
    }
    if line.starts_with(r#"{"art""#) {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let a = v.get("art")?;
        let id = key_id(a.get("k")?.as_str()?)?;
        let d = a.get("d")?.as_str()?;
        let bytes = b64(d).filter(|b| !b.is_empty() && b.len() <= ART_BYTES_MAX)?;
        return Some(Line::Art(id, bytes));
    }
    None
}

/// YouTube mu gösterilsin: YouTube çalıyorsa evet; değilse MPRIS'te çalan başka oyuncu yoksa evet;
/// MPRIS oyuncusu aynı video (Zen'in kendi MPRIS'i) ise her zaman YouTube.
#[must_use]
pub fn prefer(s: &YtState, mpris: Option<(bool, &str)>) -> bool {
    if !s.paused {
        return true;
    }
    match mpris {
        None => true,
        Some((playing, url)) => same_video(url, &s.id) || !playing,
    }
}

#[must_use]
pub fn same_video(url: &str, id: &str) -> bool {
    !id.is_empty() && url.contains(&format!("v={id}"))
}

/// YouTube durumu + an.
#[derive(Debug, Clone)]
pub struct YtNow {
    pub s: YtState,
    pub at: Instant,
}

impl Media {
    /// Eklenti durumu (ya da `None`: sekme/host gitti). Akış yalnız kesikli değişimde ya da konum sapınca uyanır.
    pub fn put_yt_at(&self, s: Option<YtState>, now: Instant) {
        let changed = self.yt.lock().is_ok_and(|mut g| {
            let changed = match (g.as_ref(), s.as_ref()) {
                (None, None) => false,
                (Some(o), Some(n)) => o.s.fixed() != n.fixed() || (o.s.pos_ms(o.at, now) - (n.cur * 1000.0) as i64).abs() > DRIFT_MS,
                _ => true,
            };
            *g = s.map(|s| YtNow { s, at: now });
            changed
        });
        self.bump(changed);
    }

    /// Görsel: en çok `ART_KEEP`, en eski düşer. Şimdiki durumun başvurduğu görsel geldiyse akış uyanır (`artId` 0 → kimlik).
    pub fn put_yt_art(&self, id: u64, bytes: Vec<u8>) {
        if let Ok(mut a) = self.yt_art.lock() {
            a.retain(|(k, _)| *k != id);
            a.push_back((id, bytes));
            while a.len() > ART_KEEP {
                a.pop_front();
            }
        }
        let used = self.yt.lock().ok().and_then(|g| g.as_ref().map(|y| refs(&y.s).contains(&id))).unwrap_or(false);
        self.bump(used);
    }

    #[must_use]
    pub fn yt_art(&self, id: u64) -> Option<Vec<u8>> {
        self.yt_art.lock().ok()?.iter().find(|(k, _)| *k == id).map(|(_, b)| b.clone())
    }

    fn art_or_zero(&self, k: &str) -> u64 {
        key_id(k).filter(|&id| self.yt_art.lock().is_ok_and(|a| a.iter().any(|(x, _)| *x == id))).unwrap_or(0)
    }

    /// Host satırı (okuma döngüsü ve test).
    pub fn on_host_line(&self, line: &str, now: Instant) {
        match parse_line(line) {
            Some(Line::Yt(s)) => self.put_yt_at(s, now),
            Some(Line::Art(id, b)) => self.put_yt_art(id, b),
            None => {}
        }
    }

    pub(super) fn yt_now(&self) -> Option<YtNow> {
        self.yt.lock().ok()?.clone()
    }

    pub(super) fn yt_snapshot(&self, y: &YtNow, now: Instant) -> MediaSnapshot {
        let s = &y.s;
        let item = |it: &YtItem| MediaItem { title: it.title.clone(), art_id: self.art_or_zero(&it.k), cur: it.cur };
        let dur_ms = (s.dur * 1000.0) as i64;
        MediaSnapshot {
            app: "YouTube".to_owned(),
            title: s.title.clone(),
            artist: s.channel.clone(),
            album: String::new(),
            playing: !s.paused,
            pos_ms: s.pos_ms(y.at, now),
            dur_ms,
            art_id: self.art_or_zero(&s.k),
            can_seek: dur_ms > 0 && !s.ad,
            source: "yt".to_owned(),
            pos_known: true,
            next: s.next.as_ref().map(item),
            queue: s.queue.iter().map(item).collect(),
            queue_index: (s.qn > 0).then_some(s.qi),
            queue_len: (s.qn > 0).then_some(s.qn),
            ad: s.ad,
        }
    }
}

fn refs(s: &YtState) -> Vec<u64> {
    std::iter::once(&s.k).chain(s.next.iter().map(|n| &n.k)).chain(s.queue.iter().map(|q| &q.k)).filter_map(|k| key_id(k)).collect()
}

#[must_use]
pub fn runtime(name: &str) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from).join(name)
}

/// Okuma döngüsü: host varken satır satır; host gidince durum temizlenir, 2 sn sonra yeniden bağlanılır.
pub async fn run(media: Arc<Media>) {
    let path = runtime("deckd-zen-tabs.sock");
    loop {
        follow_once(&media, &path).await;
        tokio::time::sleep(RETRY).await;
    }
}

/// Tek bağlantı (test edilebilir): bağlanamazsa hemen döner.
pub async fn follow_once(media: &Media, path: &Path) {
    let Ok(s) = tokio::net::UnixStream::connect(path).await else { return };
    let mut lines = tokio::io::BufReader::new(s).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        media.on_host_line(&l, Instant::now());
    }
    media.put_yt_at(None, Instant::now());
}

/// Panel eylemi → host satırı; bilinmeyen/bozuk → `None`.
#[must_use]
pub fn control_line(action: &str, ms: Option<i64>) -> Option<String> {
    match (action, ms) {
        ("playpause" | "play" | "pause" | "next" | "prev", _) => Some(format!("yt {action}\n")),
        ("seek", Some(ms)) if ms >= 0 => Some(format!("yt seek {ms}\n")),
        ("skip", Some(ms)) => Some(format!("yt skip {ms}\n")),
        _ => None,
    }
}

#[must_use]
pub fn bytes_support_yt(b: &[u8]) -> bool {
    b.windows(YT_MARK.len()).any(|w| w == YT_MARK)
}

static CAPS: std::sync::Mutex<Option<(i32, bool)>> = std::sync::Mutex::new(None);

fn host_has_yt(pid: i32) -> bool {
    let mut g = CAPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match *g {
        Some((p, c)) if p == pid => c,
        _ => {
            let c = std::fs::read(format!("/proc/{pid}/exe")).is_ok_and(|b| bytes_support_yt(&b));
            *g = Some((pid, c));
            c
        }
    }
}

/// YouTube denetimi deckd Zen soketinden. Yanıt `yt <id>` başarıdır.
///
/// # Errors
/// Geçersiz eylem · soket yok · host `yt` bilmiyor (eski ikili; satır YAZILMAZ) · yanıt yok ya da başarısız.
pub async fn control(sock: &Path, action: &str, ms: Option<i64>) -> Result<String, String> {
    let line = control_line(action, ms).ok_or_else(|| format!("geçersiz eylem {action}"))?;
    let mut s = tokio::net::UnixStream::connect(sock).await.map_err(|e| format!("Zen soketi yok ({e})"))?;
    let pid = s.peer_cred().ok().and_then(|c| c.pid()).ok_or("Zen host kimliği okunamadı")?;
    if !host_has_yt(pid) {
        return Err("Zen host'u yt bilmiyor (eski deckd ikilisi) — satır yazılmadı".to_owned());
    }
    s.write_all(line.as_bytes()).await.map_err(|e| format!("Zen soketine yazılamadı: {e}"))?;
    let mut reply = String::new();
    let mut r = tokio::io::BufReader::new(s.take(4_096));
    match tokio::time::timeout(WAIT, r.read_line(&mut reply)).await {
        Ok(Ok(n)) if n > 0 => {}
        _ => return Err("Zen yanıt vermedi".to_owned()),
    }
    let reply = reply.trim().to_owned();
    if reply.split_whitespace().next() == Some("yt") { Ok(reply) } else { Err(format!("Zen: {reply}")) }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
mod tests {
    use super::*;

    const S: &str = r#"{"yt":{"tab":7,"id":"gp66JCvxJtU","url":"https://www.youtube.com/watch?v=gp66JCvxJtU","title":"Parça","channel":"Kanal","cur":61.3,"dur":2701.4,"paused":false,"rate":1,"ad":false,"k":"0a1b2c3d","next":{"title":"Üç","id":"bbbbbbbbbbb","k":"0000beef"},"queue":[{"title":"İki","id":"gp66JCvxJtU","k":"0a1b2c3d","cur":true},{"title":"Üç","id":"bbbbbbbbbbb","k":"0000beef"}],"qi":1,"qn":3}}"#;

    fn st() -> YtState {
        match parse_line(S) {
            Some(Line::Yt(Some(s))) => s,
            other => panic!("{other:?}"),
        }
    }

    /// TP-K8-Y1: satır ayrıştırma + giriş düzeltmesi.
    #[test]
    fn tp_k8_y1_parse_and_sanitize() {
        let s = st();
        assert_eq!((s.tab, s.cur, s.dur, s.rate, s.qi, s.qn), (7, 61.3, 2701.4, 1.0, 1, 3));
        assert_eq!(s.next.as_ref().unwrap().title, "Üç");
        assert_eq!(parse_line(r#"{"yt":null}"#), Some(Line::Yt(None)));
        assert_eq!(parse_line(r#"{"tabs":[],"win":1}"#), None);
        let bad = parse_line(&format!(
            r#"{{"yt":{{"cur":-5,"dur":-3,"rate":0,"title":"{}","queue":[{}]}}}}"#,
            "ş".repeat(300),
            vec![r#"{"title":"x"}"#; 50].join(",")
        ));
        let Some(Line::Yt(Some(b))) = bad else { panic!("{bad:?}") };
        assert_eq!((b.cur, b.dur, b.rate), (0.0, 0.0, 1.0));
        assert_eq!(b.title.chars().count(), 120);
        assert_eq!(b.queue.len(), 30);
        assert_eq!(parse_line(r#"{"yt":{"cur":"x"}}"#), None, "yanlış tür → yok sayılır");
    }

    /// TP-K8-Y2: panel şekli — kaynak yt, konum hızla ilerler / duraklatınca donar / süreyle sınırlı; sıradaki + liste;
    /// kapak gelene kadar artId 0, gelince anahtar kimliği.
    #[test]
    fn tp_k8_y2_snapshot() {
        let m = Media::new();
        let t0 = Instant::now();
        m.put_yt_at(Some(st()), t0);
        let s = m.snapshot_at(t0 + Duration::from_millis(1_500)).unwrap();
        assert_eq!((s.source.as_str(), s.app.as_str(), s.title.as_str(), s.artist.as_str()), ("yt", "YouTube", "Parça", "Kanal"));
        assert!(s.playing && s.pos_known && s.can_seek);
        assert_eq!((s.pos_ms, s.dur_ms), (62_800, 2_701_400));
        assert_eq!(s.art_id, 0, "kapak henüz yok");
        assert_eq!(s.next.as_ref().unwrap().title, "Üç");
        assert_eq!(s.queue.len(), 2);
        assert!(s.queue[0].cur);
        assert_eq!((s.queue_index, s.queue_len), (Some(1), Some(3)));
        m.on_host_line(r#"{"art":{"k":"0a1b2c3d","c":true,"d":"/9j/"}}"#, t0);
        let s = m.snapshot_at(t0).unwrap();
        assert_eq!(s.art_id, 0x0a1b_2c3d);
        assert_eq!(m.yt_art(0x0a1b_2c3d).unwrap(), vec![0xff, 0xd8, 0xff]);
        let mut p = st();
        p.paused = true;
        p.rate = 2.0;
        m.put_yt_at(Some(p.clone()), t0);
        assert_eq!(m.snapshot_at(t0 + Duration::from_secs(60)).unwrap().pos_ms, 61_300, "duraklatınca donar");
        p.paused = false;
        m.put_yt_at(Some(p), t0);
        assert_eq!(m.snapshot_at(t0 + Duration::from_secs(1)).unwrap().pos_ms, 63_300, "hız 2");
        assert_eq!(m.snapshot_at(t0 + Duration::from_secs(99_999)).unwrap().pos_ms, 2_701_400);
        let v = serde_json::to_value(m.snapshot_at(t0).unwrap()).unwrap();
        for k in ["source", "posKnown", "next", "queue", "queueIndex", "queueLen"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["next"]["artId"], 0);
    }

    /// TP-K8-Y3: kaynak seçimi.
    #[test]
    fn tp_k8_y3_prefer() {
        let mut s = st();
        assert!(prefer(&s, Some((true, "spotify:x"))), "YouTube çalıyor → YouTube");
        s.paused = true;
        assert!(!prefer(&s, Some((true, "spotify:x"))), "YouTube duraklatılmış, başka oyuncu çalıyor → MPRIS");
        assert!(prefer(&s, Some((false, "spotify:x"))), "hiçbiri çalmıyor → YouTube");
        assert!(prefer(&s, Some((true, "https://www.youtube.com/watch?v=gp66JCvxJtU"))), "aynı video (Zen MPRIS) → YouTube");
        assert!(prefer(&s, None));
        assert!(!same_video("https://www.youtube.com/watch?v=x", ""));
        // Media üstünden: yt yokken MPRIS; yt gelince yt; null gelince yine MPRIS.
        let m = Media::new();
        let t0 = Instant::now();
        m.put_at(
            "org.mpris.MediaPlayer2.spotify",
            super::super::PlayerProps { status: "Paused".into(), title: "sp".into(), ..Default::default() },
            t0,
        );
        assert_eq!(m.snapshot_at(t0).unwrap().source, "mpris");
        m.put_yt_at(Some(st()), t0);
        assert_eq!(m.snapshot_at(t0).unwrap().source, "yt");
        let mut p = st();
        p.paused = true;
        m.put_yt_at(Some(p), t0);
        m.put_at(
            "org.mpris.MediaPlayer2.spotify",
            super::super::PlayerProps { status: "Playing".into(), title: "sp".into(), ..Default::default() },
            t0,
        );
        assert_eq!(m.snapshot_at(t0).unwrap().source, "mpris", "YouTube duraklatılmış + Spotify çalıyor → Spotify");
        m.put_yt_at(None, t0);
        assert_eq!(m.snapshot_at(t0).unwrap().title, "sp");
    }

    /// TP-K8-Y4: görsel önbelleği (base64, anahtar, sınır).
    #[test]
    fn tp_k8_y4_art_cache() {
        assert_eq!(b64("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(b64("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(b64("a$=="), None);
        assert_eq!(key_id("0a1b2c3d"), Some(0x0a1b_2c3d));
        assert_eq!((key_id(""), key_id("0"), key_id("zz"), key_id("11112222333344445")), (None, None, None, None));
        assert_eq!(parse_line(r#"{"art":{"k":"../x","d":"AAAA"}}"#), None);
        let m = Media::new();
        for i in 1..=60u64 {
            m.put_yt_art(i, vec![1]);
        }
        assert!(m.yt_art(1).is_none() && m.yt_art(12).is_none() && m.yt_art(13).is_some() && m.yt_art(60).is_some(), "en çok 48");
    }

    /// TP-K8-Y5: sessiz kip — yalnız konum ilerlemesi (sapma ≤ 1,5 sn) akışı uyandırmaz; duraklatma ve büyük sıçrama uyandırır.
    #[test]
    fn tp_k8_y5_quiet() {
        let m = Media::new();
        let mut rx = m.watch();
        let t0 = Instant::now();
        m.put_yt_at(Some(st()), t0);
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        let mut s = st();
        s.cur = 62.9;
        m.put_yt_at(Some(s.clone()), t0 + Duration::from_secs(1));
        assert!(!rx.has_changed().unwrap(), "beklenen konumdan 0,6 sn sapma: uyandırmaz");
        s.cur = 300.0;
        m.put_yt_at(Some(s.clone()), t0 + Duration::from_secs(2));
        assert!(rx.has_changed().unwrap(), "sıçrama (seek) uyandırır");
        rx.mark_unchanged();
        s.paused = true;
        m.put_yt_at(Some(s), t0 + Duration::from_secs(2));
        assert!(rx.has_changed().unwrap(), "duraklatma uyandırır");
        rx.mark_unchanged();
        m.put_yt_art(0x9999, vec![1]);
        assert!(!rx.has_changed().unwrap(), "başvurulmayan görsel uyandırmaz");
        m.put_yt_art(0x0a1b_2c3d, vec![1]);
        assert!(rx.has_changed().unwrap(), "kapak geldi → artId değişir → uyandırır");
    }

    /// TP-K8-Y6: denetim satırı + yetenek işareti; sahte host soketi üstünden uçtan uca (test ikilisi işareti taşır).
    #[tokio::test]
    async fn tp_k8_y6_control_over_fake_host() {
        assert_eq!(control_line("skip", Some(-10_000)).as_deref(), Some("yt skip -10000\n"));
        assert_eq!(control_line("seek", Some(61_000)).as_deref(), Some("yt seek 61000\n"));
        assert_eq!(control_line("playpause", None).as_deref(), Some("yt playpause\n"));
        assert_eq!((control_line("seek", Some(-1)), control_line("seek", None), control_line("rm", None)), (None, None, None));
        assert!(bytes_support_yt(b"..deckd-host-proto:yt.."));
        assert!(!bytes_support_yt(b"deckd-host-proto:route"));
        let dir = std::env::temp_dir().join(format!("phoned-yt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("z.sock");
        let _ = std::fs::remove_file(&sock);
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut r = tokio::io::BufReader::new(s);
            let mut got = String::new();
            r.read_line(&mut got).await.unwrap();
            r.get_mut().write_all(b"yt 3\n").await.unwrap();
            got
        });
        assert_eq!(control(&sock, "skip", Some(10_000)).await.unwrap(), "yt 3");
        assert_eq!(srv.await.unwrap(), "yt skip 10000\n");
        assert!(control(&dir.join("yok.sock"), "next", None).await.is_err());
        assert!(control(&sock, "rm", None).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// TP-K8-Y7: okuma döngüsü — sahte sekme soketi yt + görsel yazar → durum; host gidince durum temizlenir.
    #[tokio::test]
    async fn tp_k8_y7_follow_fake_tabs_socket() {
        let dir = std::env::temp_dir().join(format!("phoned-ytt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock");
        let _ = std::fs::remove_file(&sock);
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        let m = Arc::new(Media::new());
        let (close_tx, close_rx) = tokio::sync::oneshot::channel::<()>();
        let srv = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            s.write_all(br#"{"tabs":[],"win":1}"#).await.unwrap();
            s.write_all(format!("\n{S}\n").as_bytes()).await.unwrap();
            s.write_all(b"{\"art\":{\"k\":\"0a1b2c3d\",\"d\":\"/9j/\"}}\n").await.unwrap();
            let _ = close_rx.await; // host bağlantıyı test söyleyene kadar açık tutar
        });
        let mut rx = m.watch();
        let m2 = Arc::clone(&m);
        let sock2 = sock.clone();
        let reader = tokio::spawn(async move { follow_once(&m2, &sock2).await });
        let deadline = Instant::now() + Duration::from_secs(5);
        while m.yt_art(0x0a1b_2c3d).is_none() && Instant::now() < deadline {
            let _ = tokio::time::timeout(Duration::from_millis(200), rx.changed()).await;
        }
        let s = m.snapshot().unwrap();
        assert_eq!((s.source.as_str(), s.title.as_str(), s.art_id), ("yt", "Parça", 0x0a1b_2c3d));
        close_tx.send(()).unwrap();
        reader.await.unwrap();
        srv.await.unwrap();
        assert_eq!(m.snapshot(), None, "host gitti → yt temizlendi, MPRIS yok → null");
        let _ = std::fs::remove_dir_all(dir);
    }
}
