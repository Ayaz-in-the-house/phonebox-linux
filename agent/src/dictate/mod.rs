//! K11 dikte (PRD-DT01): telefonun basılı-tut kaydı (16 kHz mono 16 bit PCM WAV) → herdr-web `POST /api/dictate`
//! (Handy motoru, sözlük, uydurma kapısı) → `handy-smart-paste "<metin>"` (fare altındaki pencere).
//!
//! - Yetki: herdr-web her açılışta yerel token'ı `$XDG_RUNTIME_DIR/herdr-web/token` (0600) ve adresini `…/url`'e yazar
//!   (herdr-web `server.ts` PRD-90 F5, `case-api.ts caseTokenPath`); `authorise` bearer token'ı kabul eder. Token yeniden
//!   başlatmada değişir → her istekte dosyadan okunur. Değer günlüğe, argv'ye, yanıta girmez; yalnız loopback'e gönderilir.
//! - Tek kuyruk: motora gidiş + yapıştırma tek kilit altında (yapıştırmalar iç içe geçmez, sıra = istek sırası);
//!   içeride en çok `MAX_QUEUE` istek, fazlası 429.
//! - Günlük: yalnız bayt/süre/karakter sayısı; ses ve metin yazılmaz (TP-DT-08).
//! - HTTP istemcisi elle (tek istek, `Connection: close`): yeni bağımlılık yok (`audio`, `adb` kardeşleri gibi).

pub mod http;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// Gövde tavanı: 2 MiB ≈ 65 sn 16 kHz mono 16 bit (PRD TP-DT-03).
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// İçerideki (koşan + bekleyen) en çok istek; fazlası 429 (TP-DT-07).
pub const MAX_QUEUE: usize = 3;
/// herdr-web yanıtı için tavan (motor yüklenirken ilk istek birkaç sn sürebilir).
const ENGINE_TIMEOUT: Duration = Duration::from_secs(90);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const PASTE_TIMEOUT: Duration = Duration::from_secs(10);
/// herdr-web yanıt gövdesi tavanı (metin + telemetri).
const MAX_REPLY: usize = 256 * 1024;

/// WAV reddi: `Malformed` = bozuk/boş (400), `Format` = geçerli ama motorun okumadığı biçim (415).
#[derive(Debug)]
pub enum WavError {
    Malformed(String),
    Format(String),
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).and_then(|s| s.try_into().ok()).map(u16::from_le_bytes)
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).and_then(|s| s.try_into().ok()).map(u32::from_le_bytes)
}

/// herdr-web `checkDictationWav` eşi: RIFF/WAVE, `fmt ` = PCM 1 kanal 16000 Hz 16 bit, `data` boş değil.
/// Bölüm boyu dosya sonunu aşarsa (akış yazıcısı) eldeki kadarı sayılır; tek boylu bölümler bir baytla dolgulanır.
///
/// # Errors
/// `WavError` (yukarıda).
pub fn check_wav(b: &[u8]) -> Result<f64, WavError> {
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err(WavError::Malformed("WAV değil (RIFF/WAVE başlığı yok)".into()));
    }
    let mut pos = 12usize;
    let mut fmt = false;
    let mut data: Option<usize> = None;
    while let Some(start) = pos.checked_add(8).filter(|e| *e <= b.len()) {
        let (Some(id), Some(size)) = (b.get(pos..pos + 4), le32(b, pos + 4)) else { break };
        let size = size as usize;
        let available = size.min(b.len().saturating_sub(start));
        if id == b"fmt " && available >= 16 {
            let (format, channels, rate, bits) = (le16(b, start), le16(b, start + 2), le32(b, start + 4), le16(b, start + 14));
            if (format, channels, rate, bits) != (Some(1), Some(1), Some(16_000), Some(16)) {
                return Err(WavError::Format(format!(
                    "beklenen 16000 Hz mono 16 bit PCM; gelen biçim={} kanal={} örnekleme={} bit={}",
                    format.unwrap_or(0),
                    channels.unwrap_or(0),
                    rate.unwrap_or(0),
                    bits.unwrap_or(0)
                )));
            }
            fmt = true;
        } else if id == b"data" {
            data = Some(available);
        }
        pos = start.saturating_add(size).saturating_add(size & 1);
    }
    if !fmt {
        return Err(WavError::Malformed("WAV'da fmt bölümü yok".into()));
    }
    let samples = data.ok_or_else(|| WavError::Malformed("WAV'da ses verisi yok".into()))? / 2;
    if samples == 0 {
        return Err(WavError::Malformed("kayıt boş".into()));
    }
    Ok(f64::from(u32::try_from(samples).unwrap_or(u32::MAX)) / 16_000.0)
}

/// `http://127.0.0.1:<port>` ya da `http://localhost:<port>` (sonda `/` ve boşluk serbest). Başka her şey ret:
/// token yalnız bu makinenin loopback'ine gider.
///
/// # Errors
/// Biçim dışı ya da loopback olmayan adres.
pub fn loopback_addr(url: &str) -> Result<SocketAddr, String> {
    let rest = url.trim().strip_prefix("http://").ok_or("yalnız http://127.0.0.1 adresi")?;
    let rest = rest.trim_end_matches('/');
    let (host, port) = rest.rsplit_once(':').ok_or("adreste port yok")?;
    if host != "127.0.0.1" && !host.eq_ignore_ascii_case("localhost") {
        return Err("yalnız yerel (127.0.0.1) herdr-web".into());
    }
    let port: u16 = port.parse().map_err(|_| "geçersiz port")?;
    Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// Dış uçlar: herdr-web adresi (açık değer ya da adres dosyası), token dosyası, yapıştırıcı.
#[derive(Debug, Clone)]
pub struct Settings {
    /// `PHONED_DICTATE_URL`: verilirse `url_file` okunmaz.
    pub url: Option<String>,
    /// herdr-web'in yazdığı adres (`$XDG_RUNTIME_DIR/herdr-web/url`).
    pub url_file: PathBuf,
    /// herdr-web yerel token'ı (`$XDG_RUNTIME_DIR/herdr-web/token`, `PHONED_DICTATE_TOKEN_FILE`).
    pub token_file: PathBuf,
    /// `PHONED_DICTATE_PASTE` (öntanım `~/.local/bin/handy-smart-paste`).
    pub paste: PathBuf,
}

impl Settings {
    #[must_use]
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        let runtime = var("XDG_RUNTIME_DIR").map_or_else(
            || {
                use std::os::unix::fs::MetadataExt as _;
                let uid = std::fs::metadata("/proc/self").map_or(1000, |m| m.uid());
                PathBuf::from(format!("/run/user/{uid}"))
            },
            PathBuf::from,
        );
        let home = var("HOME").map(PathBuf::from).unwrap_or_default();
        Self {
            url: var("PHONED_DICTATE_URL").and_then(|v| v.into_string().ok()),
            url_file: runtime.join("herdr-web/url"),
            token_file: var("PHONED_DICTATE_TOKEN_FILE").map_or_else(|| runtime.join("herdr-web/token"), PathBuf::from),
            paste: var("PHONED_DICTATE_PASTE").map_or_else(|| home.join(".local/bin/handy-smart-paste"), PathBuf::from),
        }
    }
}

/// Günlük satırı alıcısı (üretimde stderr → journal; testte toplayıcı).
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// Bir isteğin sonucu (HTTP katmanı biçime çevirir).
#[derive(Debug)]
pub enum Outcome {
    Done { text: String, ms: u64, pasted: bool, paste_error: Option<String> },
    Busy,
    Engine(String),
}

pub struct Dictator {
    pub settings: Settings,
    log: Log,
    lane: tokio::sync::Mutex<()>,
    inside: AtomicUsize,
}

/// `inside` sayacını düşüren bekçi (iptal edilen istek de sayılmaz olur).
struct Slot<'a>(&'a AtomicUsize);
impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Dictator {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self::with_log(settings, Arc::new(|l: &str| eprintln!("{l}")))
    }

    #[must_use]
    pub fn with_log(settings: Settings, log: Log) -> Self {
        Self { settings, log, lane: tokio::sync::Mutex::new(()), inside: AtomicUsize::new(0) }
    }

    pub fn log(&self, line: &str) {
        (self.log)(line);
    }

    /// Doğrulanmış WAV'ı motora gönder, metin boş değilse ve `paste` ise bir kez yapıştır. Tümü tek kuyrukta.
    pub async fn run(&self, wav: &[u8], paste: bool) -> Outcome {
        if self.inside.fetch_add(1, Ordering::AcqRel) >= MAX_QUEUE {
            self.inside.fetch_sub(1, Ordering::AcqRel);
            return Outcome::Busy;
        }
        let _slot = Slot(&self.inside);
        let _lane = self.lane.lock().await;
        let (text, ms) = match self.transcribe(wav).await {
            Ok(r) => r,
            Err(e) => return Outcome::Engine(e),
        };
        let text = text.trim().to_owned();
        if text.is_empty() || !paste {
            return Outcome::Done { text, ms, pasted: false, paste_error: None };
        }
        let paste_error = self.paste(&text).await.err();
        Outcome::Done { pasted: paste_error.is_none(), text, ms, paste_error }
    }

    fn target(&self) -> Result<(SocketAddr, String), String> {
        let url = match &self.settings.url {
            Some(u) => u.clone(),
            None => std::fs::read_to_string(&self.settings.url_file).map_err(|e| format!("herdr-web adresi okunamadı ({e})"))?,
        };
        let addr = loopback_addr(&url)?;
        let token = std::fs::read_to_string(&self.settings.token_file).map_err(|e| format!("herdr-web yerel token'ı okunamadı ({})", e.kind()))?;
        let token = token.trim().to_owned();
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("herdr-web yerel token'ı geçersiz".into());
        }
        Ok((addr, token))
    }

    async fn transcribe(&self, wav: &[u8]) -> Result<(String, u64), String> {
        let (addr, token) = self.target()?;
        let reply = tokio::time::timeout(ENGINE_TIMEOUT, exchange(addr, &token, wav)).await.map_err(|_| "herdr-web zaman aşımı".to_owned())??;
        let (status, body) = reply;
        let v: serde_json::Value = serde_json::from_slice(&body).map_err(|_| format!("herdr-web {status}: JSON değil"))?;
        if status != 200 {
            let why = v.get("error").and_then(serde_json::Value::as_str).unwrap_or("hata");
            return Err(format!("herdr-web {status}: {why}"));
        }
        let text = v.get("text").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        let ms = v.get("ms").and_then(serde_json::Value::as_u64).unwrap_or(0);
        Ok((text, ms))
    }

    /// `handy-smart-paste <metin>`: argv, kabuk yok; çıktı atılır (betik içerik yazmaz, biz de okumayız).
    async fn paste(&self, text: &str) -> Result<(), String> {
        let mut child = tokio::process::Command::new(&self.settings.paste)
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("yapıştırıcı başlatılamadı ({})", e.kind()))?;
        match tokio::time::timeout(PASTE_TIMEOUT, child.wait()).await {
            Ok(Ok(s)) if s.success() => Ok(()),
            Ok(Ok(s)) => Err(format!("yapıştırma çıkış kodu {}", s.code().unwrap_or(-1))),
            Ok(Err(e)) => Err(format!("yapıştırma beklenemedi ({})", e.kind())),
            Err(_) => Err("yapıştırma zaman aşımı".into()),
        }
    }
}

/// Tek HTTP/1.1 isteği: `POST /api/dictate`, gövde = WAV. Dönen: (durum, gövde).
async fn exchange(addr: SocketAddr, token: &str, wav: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let mut s = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| format!("herdr-web {addr} bağlantı zaman aşımı"))?
        .map_err(|e| format!("herdr-web {addr} erişilemedi ({})", e.kind()))?;
    let head = format!(
        "POST /api/dictate HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        wav.len()
    );
    let io = |e: std::io::Error| format!("herdr-web G/Ç ({})", e.kind());
    s.write_all(head.as_bytes()).await.map_err(io)?;
    s.write_all(wav).await.map_err(io)?;
    let mut buf = Vec::new();
    s.take(u64::try_from(MAX_REPLY).unwrap_or(u64::MAX)).read_to_end(&mut buf).await.map_err(io)?;
    parse_response(&buf)
}

/// Yanıt: durum satırı + başlıklar + gövde (`Content-Length` ya da bağlantı sonuna dek). `chunked` desteklenmez → açık hata.
fn parse_response(buf: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or("herdr-web yanıtı eksik")?;
    let head = std::str::from_utf8(buf.get(..split).unwrap_or_default()).map_err(|_| "herdr-web başlığı okunamadı")?;
    let body = buf.get(split + 4..).unwrap_or_default();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|c| c.parse().ok()).ok_or("herdr-web durum satırı okunamadı")?;
    let mut len: Option<usize> = None;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let v = v.trim();
        if k.eq_ignore_ascii_case("content-length") {
            len = v.parse().ok();
        } else if k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked") {
            return Err("herdr-web parçalı yanıt verdi (desteklenmiyor)".into());
        }
    }
    let body = match len {
        Some(n) => body.get(..n).ok_or("herdr-web yanıtı kısa kesildi")?,
        None => body,
    };
    Ok((status, body.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_reads_content_length_and_refuses_chunked() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}junk";
        assert_eq!(parse_response(ok).ok(), Some((200, b"{}".to_vec())));
        let eof = b"HTTP/1.1 502 Bad\r\n\r\n{\"a\":1}";
        assert_eq!(parse_response(eof).ok(), Some((502, b"{\"a\":1}".to_vec())));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}").is_err());
        assert!(parse_response(b"garbage").is_err());
    }
}
