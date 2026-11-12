//! Zen'e yönlendirme: deckd'nin Zen soketi (`$XDG_RUNTIME_DIR/deckd-zen.sock`) — salt istemci, süreç başlatma yok.
//!
//! Kapı deckd `zenhost::route` ile aynı (PRD-D11): soketin karşısındaki host ikilisi `deckd-host-proto:route` taşıyorsa
//! satır `route <url>`, taşımıyorsa düz adres (eski host: herdr sekmesi öne gelir, pane değişmez). Yanıt tek satır;
//! `routed`/`focused`/`created` başarıdır. Yetenek okuması host süreci başına bir kez (pid önbelleği).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

pub const ROUTE_MARK: &[u8] = b"deckd-host-proto:route";
const WAIT: Duration = Duration::from_millis(1_500);

#[must_use]
pub fn sock_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    dir.join("deckd-zen.sock")
}

/// Host ikilisinin baytları `route` protokolünü taşıyor mu (saf).
#[must_use]
pub fn bytes_support_route(b: &[u8]) -> bool {
    b.windows(ROUTE_MARK.len()).any(|w| w == ROUTE_MARK)
}

#[must_use]
pub fn line(url: &str, can_route: bool) -> String {
    if can_route { format!("route {url}\n") } else { format!("{url}\n") }
}

/// Yanıtın ilk sözcüğü başarı mı.
#[must_use]
pub fn reply_ok(reply: &str) -> bool {
    matches!(reply.split_whitespace().next(), Some("routed" | "focused" | "created"))
}

/// Yetenek taraması parça boyu (işaret deckd ikilisinde ~320 KB'ta: birkaç parçada biter).
pub const CHUNK: usize = 64 * 1024;

/// Akıştan işareti ara; bulunduğu parçada dur (TP-AP-19). Dönen: (bulundu mu, okunan bayt).
///
/// # Errors
/// Okuma hatası.
pub fn has_mark(mut r: impl std::io::Read, mark: &[u8]) -> std::io::Result<(bool, u64)> {
    let k = mark.len();
    if k == 0 {
        return Ok((true, 0));
    }
    // Parçalar arasında son k-1 bayt taşınır: sınıra düşen işaret kaçmaz.
    let mut buf = vec![0u8; CHUNK + k];
    let (mut keep, mut total) = (0usize, 0u64);
    loop {
        let Some(dst) = buf.get_mut(keep..keep + CHUNK) else { return Ok((false, total)) };
        let n = match r.read(dst) {
            Ok(0) => return Ok((false, total)),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        total += n as u64;
        let end = keep + n;
        if buf.get(..end).is_some_and(|b| b.windows(k).any(|w| w == mark)) {
            return Ok((true, total));
        }
        let tail = end.min(k - 1);
        buf.copy_within(end - tail..end, 0);
        keep = tail;
    }
}

static CAPS: Mutex<Option<(i32, bool)>> = Mutex::new(None);

/// Önbellekte varsa (yetenek, true); yoksa `None`.
fn cached_caps(pid: i32) -> Option<bool> {
    let g = CAPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    g.and_then(|(p, c)| (p == pid).then_some(c))
}

/// Host ikilisini oku (engelleyici; `spawn_blocking` içinde) ve önbelleğe yaz.
fn read_caps(pid: i32) -> bool {
    let c = std::fs::File::open(format!("/proc/{pid}/exe")).and_then(|f| has_mark(f, ROUTE_MARK)).is_ok_and(|(b, _)| b);
    *CAPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((pid, c));
    c
}

/// (yetenek, önbellekten mi).
async fn can_route(pid: i32) -> (bool, bool) {
    if let Some(c) = cached_caps(pid) {
        return (c, true);
    }
    (tokio::task::spawn_blocking(move || read_caps(pid)).await.unwrap_or(false), false)
}

/// Halka süreleri (TP-AP-21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
    pub conn_us: u64,
    pub caps_us: u64,
    pub host_us: u64,
    pub caps_cached: bool,
}

fn us(t: std::time::Instant) -> u64 {
    u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Sonuç: (host yanıtı, host `route` biliyor mu).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub reply: String,
    pub can_route: bool,
    pub timing: Timing,
}

/// `url`'yi Zen'e gönder.
///
/// # Errors
/// Soket yok / host kimliği okunamadı / yanıt gelmedi ya da başarısız.
pub async fn route(sock: &Path, url: &str) -> Result<Routed, String> {
    let t = std::time::Instant::now();
    let mut s = tokio::net::UnixStream::connect(sock).await.map_err(|e| format!("Zen soketi yok ({e}) — Zen açık mı, deckd eklentisi kurulu mu?"))?;
    let pid = s.peer_cred().ok().and_then(|c| c.pid()).ok_or("Zen host kimliği okunamadı")?;
    let mut timing = Timing { conn_us: us(t), ..Timing::default() };
    let t = std::time::Instant::now();
    let (can, cached) = can_route(pid).await;
    timing.caps_us = us(t);
    timing.caps_cached = cached;
    let t = std::time::Instant::now();
    s.write_all(line(url, can).as_bytes()).await.map_err(|e| format!("Zen soketine yazılamadı: {e}"))?;
    let mut reply = String::new();
    let mut r = tokio::io::BufReader::new(s.take(4_096));
    match tokio::time::timeout(WAIT, r.read_line(&mut reply)).await {
        Ok(Ok(n)) if n > 0 => {}
        Ok(Ok(_) | Err(_)) | Err(_) => return Err("Zen yanıt vermedi".to_owned()),
    }
    timing.host_us = us(t);
    let reply = reply.trim().to_owned();
    if reply_ok(&reply) { Ok(Routed { reply, can_route: can, timing }) } else { Err(format!("Zen: {reply}")) }
}

/// Isıtma (TP-AP-20): host'a bağlan, kimliğini al, yeteneğini önbelleğe al, **hiçbir şey yazmadan** kapat.
/// Host boş satırı (EOF) yok sayar: kimlik numarası artmaz, eklentiye ileti gitmez, odak değişmez.
/// Dönen: host `route` biliyor mu.
///
/// # Errors
/// Soket yok / host kimliği okunamadı.
pub async fn warm(sock: &Path) -> Result<bool, String> {
    let s = tokio::net::UnixStream::connect(sock).await.map_err(|e| format!("Zen soketi yok ({e})"))?;
    let pid = s.peer_cred().ok().and_then(|c| c.pid()).ok_or("Zen host kimliği okunamadı")?;
    // Önce kapat: host tek iş parçacığıyla sırayla kabul eder; okuma sürerken bağlantıyı tutmak gerçek rotayı bekletir.
    drop(s);
    Ok(can_route(pid).await.0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test")]
mod tests {
    use super::*;

    #[test]
    fn tp_w2_09_line_and_caps() {
        assert_eq!(line("https://h/herdr/#/t/wC:p1", true), "route https://h/herdr/#/t/wC:p1\n");
        assert_eq!(line("https://h/herdr/#/t/wC:p1", false), "https://h/herdr/#/t/wC:p1\n");
        assert!(bytes_support_route(b"xx\0deckd-host-proto:route\0yy"));
        assert!(!bytes_support_route(b"deckd-host-proto:nav"));
        assert!(reply_ok("routed 3") && reply_ok("focused") && reply_ok("created 9"));
        assert!(!reply_ok("zaman-aşımı") && !reply_ok(""));
    }

    /// Sayaçlı okuyucu: kaç bayt istendi.
    struct Count<R>(R, std::rc::Rc<std::cell::Cell<u64>>);
    impl<R: std::io::Read> std::io::Read for Count<R> {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.read(b)?;
            self.1.set(self.1.get() + n as u64);
            Ok(n)
        }
    }

    fn blob(len: usize, at: Option<usize>) -> Vec<u8> {
        let mut v: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
        if let Some(a) = at {
            v[a..a + ROUTE_MARK.len()].copy_from_slice(ROUTE_MARK);
        }
        v
    }

    #[test]
    fn tp_ap_19_has_mark_stops_at_first_hit() {
        let v = blob(8 << 20, Some(320_740));
        let n = std::rc::Rc::new(std::cell::Cell::new(0));
        let (found, read) = has_mark(Count(std::io::Cursor::new(&v), n.clone()), ROUTE_MARK).unwrap();
        assert!(found, "işaret 320 KB'ta");
        assert!(read <= 320_740 + 2 * CHUNK as u64, "erken durmalı, okunan {read}");
        assert_eq!(read, n.get());
        assert!(n.get() < 1 << 20, "8 MB'ın tamamı okunmamalı: {}", n.get());
    }

    #[test]
    fn tp_ap_19_mark_across_chunk_boundary_and_absent() {
        for at in [CHUNK - 1, CHUNK - ROUTE_MARK.len() + 1, CHUNK, 3 * CHUNK - 5, 0] {
            let v = blob(4 * CHUNK, Some(at));
            assert!(has_mark(std::io::Cursor::new(&v), ROUTE_MARK).unwrap().0, "sınırda işaret kaçtı: {at}");
        }
        let v = blob(4 * CHUNK, None);
        let (found, read) = has_mark(std::io::Cursor::new(&v), ROUTE_MARK).unwrap();
        assert!(!found);
        assert_eq!(read, 4 * CHUNK as u64, "işaretsiz dosya sonuna kadar okunur");
        assert!(!has_mark(std::io::Cursor::new(b"deckd-host-proto:nav"), ROUTE_MARK).unwrap().0);
    }

    /// TP-AP-20: ısıtma 0 bayt yazar, yeteneği önbelleğe alır; ardından `route` dosya okumaz.
    #[tokio::test]
    async fn tp_ap_20_warm_writes_nothing_then_route_is_cached() {
        let dir = std::env::temp_dir().join(format!("phoned-zenw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("w.sock");
        let _ = std::fs::remove_file(&sock);
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        let srv = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).await.unwrap();
            let (s2, _) = l.accept().await.unwrap();
            let mut r = tokio::io::BufReader::new(s2);
            let mut line = String::new();
            r.read_line(&mut line).await.unwrap();
            r.get_mut().write_all(b"routed 1\n").await.unwrap();
            (got, line)
        });
        assert!(warm(&sock).await.unwrap(), "test ikilisi işareti taşır");
        let out = route(&sock, "https://h/herdr/#/t/wC:p1").await.unwrap();
        let (warm_bytes, line) = srv.await.unwrap();
        assert!(warm_bytes.is_empty(), "ısıtma host'a bayt yazdı: {warm_bytes:?}");
        assert_eq!(line, "route https://h/herdr/#/t/wC:p1\n", "ısıtma kimlik numarası tüketmedi, ilk satır gerçek rota");
        assert!(out.timing.caps_cached, "ısıtmadan sonra route ikiliyi yeniden okumamalı");
        assert!(warm(&dir.join("yok.sock")).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Kendi sürecimiz host: test ikilisi işareti taşır (bu dosyadaki sabit) → `route` satırı gider.
    #[tokio::test]
    async fn tp_w2_09_route_over_fake_host_socket() {
        let dir = std::env::temp_dir().join(format!("phoned-zen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("z.sock");
        let _ = std::fs::remove_file(&sock);
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut r = tokio::io::BufReader::new(s);
            let mut got = String::new();
            r.read_line(&mut got).await.unwrap();
            r.get_mut().write_all(b"routed 7\n").await.unwrap();
            got
        });
        let out = route(&sock, "https://h/herdr/#/t/wC:p1").await.unwrap();
        assert_eq!(srv.await.unwrap(), "route https://h/herdr/#/t/wC:p1\n");
        assert!(out.can_route);
        assert_eq!(out.reply, "routed 7");
        assert!(route(&dir.join("yok.sock"), "u").await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
