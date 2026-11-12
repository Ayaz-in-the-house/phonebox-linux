//! adb: telefonu `host:track-devices` kalıcı soketiyle izle (5037), görünen cihaza `reverse` kur — süreç başlatma yok (HP2).
//!
//! 0.6.9 `AdbReverse.cs` her 4 sn'de `adb devices` + cihaz başına `adb reverse --list` başlatıyordu (≈ 0,5–0,75 fork/sn).
//! Burada adb sunucusunun akıllı soket protokolü doğrudan konuşulur: istek = 4 onaltılık uzunluk + yük; yanıt `OKAY`/`FAIL`.
//! İzleme bağlantısı açık kalır; sunucu yalnız cihaz listesi değişince çerçeve yazar → kararlı hâlde 0 iş.
//!
//! Paket adı yapılandırmadan gelir (K2 apk bulgusu: 0.6.9 sabit kodluyor, `AdbReverse.cs:11,93`): kurulu ilk paket seçilir,
//! etkinlik tam nitelikli sınıfla açılır (`<applicationId>/net.oxog.phonebox.deskpanel.MainActivity`) → farklı
//! `applicationId` ile yan yana kurulan kendi APK'mız da çalışır.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

pub const DEFAULT_ADDR: &str = "127.0.0.1:5037";
pub const DEFAULT_PACKAGE: &str = "net.oxog.phonebox.deskpanel";
/// Paneli açan etkinliğin sınıfı (paket adından bağımsız: `namespace` sabit, `applicationId` değişebilir).
pub const ACTIVITY_CLASS: &str = "net.oxog.phonebox.deskpanel.MainActivity";
/// Sunucu yoksa yeniden deneme aralığı (sıcak döngü yok, TP-W2-07f).
pub const RETRY: Duration = Duration::from_secs(5);
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REPLY: u64 = 64 * 1024;

/// Ayarlar (komut satırından).
#[derive(Debug, Clone)]
pub struct Settings {
    /// adb sunucusu (`127.0.0.1:5037`); `None` = kapalı.
    pub addr: Option<String>,
    /// Aday paketler, sırayla.
    pub packages: Vec<String>,
    /// Telefon tarafı port (APK bunu çağırır).
    pub phone_port: u16,
    /// Bu ajanın portu.
    pub agent_port: u16,
    /// Yeni takılan cihazda ekranı uyandır + paneli aç (0.6.9 davranışı).
    pub open_panel: bool,
}

/// `/api/info` için durum.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Bağlı ve (varsa `deviceSerial` süzgecinden geçen) cihazlar.
    pub devices: Vec<String>,
    /// Uygulaması kurulu olmayan cihazlar.
    pub missing: Vec<String>,
    /// adb sunucusuna bağlı mı.
    pub server: bool,
}

pub type Shared = Arc<Mutex<Status>>;

#[must_use]
pub fn status(s: &Shared) -> Status {
    s.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn set_status(s: &Shared, f: impl FnOnce(&mut Status)) {
    f(&mut s.lock().unwrap_or_else(PoisonError::into_inner));
}

/// `--packages a,b` → liste (boş öğeler atılır; hiç yoksa varsayılan).
#[must_use]
pub fn parse_packages(s: &str) -> Vec<String> {
    let v: Vec<String> = s.split(',').map(str::trim).filter(|p| valid_package(p)).map(str::to_owned).collect();
    if v.is_empty() { vec![DEFAULT_PACKAGE.to_owned()] } else { v }
}

/// Android paket adı biçimi (kabuk komutuna gider → yalnız `[A-Za-z0-9_.]`).
#[must_use]
pub fn valid_package(p: &str) -> bool {
    !p.is_empty() && p.len() <= 255 && p.contains('.') && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.'))
}

#[must_use]
pub fn activity(pkg: &str) -> String {
    format!("{pkg}/{ACTIVITY_CLASS}")
}

/// İstek çerçevesi: 4 onaltılık uzunluk + yük.
#[must_use]
pub fn encode(req: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", req.len()).into_bytes();
    v.extend_from_slice(req.as_bytes());
    v
}

/// `track-devices` yükü → `device` durumundaki seriler (`offline`/`unauthorized`/`recovery` hariç); `serial` doluysa yalnız o.
#[must_use]
pub fn parse_devices(payload: &str, serial: Option<&str>) -> Vec<String> {
    payload
        .lines()
        .filter_map(|l| {
            let mut it = l.trim().split('\t');
            match (it.next(), it.next()) {
                (Some(s), Some("device")) if !s.is_empty() => Some(s.to_owned()),
                _ => None,
            }
        })
        .filter(|s| serial.is_none_or(|want| want.is_empty() || want == s))
        .collect()
}

fn hex4(b: &[u8; 4]) -> Result<usize, String> {
    let t = std::str::from_utf8(b).map_err(|_| "adb: uzunluk ASCII değil".to_owned())?;
    usize::from_str_radix(t, 16).map_err(|_| format!("adb: bozuk uzunluk {t:?}"))
}

async fn read_frame(r: &mut (impl AsyncRead + Unpin)) -> Result<String, String> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await.map_err(|e| format!("adb: okunamadı: {e}"))?;
    let n = hex4(&len)?;
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await.map_err(|e| format!("adb: okunamadı: {e}"))?;
    String::from_utf8(buf).map_err(|_| "adb: yük UTF-8 değil".to_owned())
}

async fn read_status(r: &mut (impl AsyncRead + Unpin)) -> Result<(), String> {
    let mut st = [0u8; 4];
    r.read_exact(&mut st).await.map_err(|e| format!("adb: durum okunamadı: {e}"))?;
    match &st {
        b"OKAY" => Ok(()),
        b"FAIL" => Err(format!("adb: {}", read_frame(r).await.unwrap_or_default())),
        other => Err(format!("adb: beklenmeyen yanıt {:?}", String::from_utf8_lossy(other))),
    }
}

/// Cihaz servisi yanıtının gövdesi: baştaki `OKAY`'ler düşer, `FAIL` hatadır, kalan isteğe bağlı 4-hex uzunluk önekli.
///
/// # Errors
/// `FAIL` yanıtı.
pub fn service_body(raw: &str) -> Result<String, String> {
    let mut t = raw;
    while let Some(rest) = t.strip_prefix("OKAY") {
        t = rest;
    }
    if let Some(rest) = t.strip_prefix("FAIL") {
        let msg = rest.get(4..).unwrap_or(rest);
        return Err(format!("adb: {msg}"));
    }
    if let (Some(h), Some(rest)) = (t.get(..4), t.get(4..))
        && h.bytes().all(|b| b.is_ascii_hexdigit())
        && usize::from_str_radix(h, 16).is_ok_and(|n| n == rest.len())
    {
        return Ok(rest.to_owned());
    }
    Ok(t.to_owned())
}

/// Bir cihaza tek servis isteği (`host:transport:<s>` → servis → EOF'a kadar oku).
///
/// # Errors
/// Bağlantı, `FAIL`, zaman aşımı.
pub async fn device_query(addr: &str, serial: &str, service: &str) -> Result<String, String> {
    let q = async {
        let mut s = TcpStream::connect(addr).await.map_err(|e| format!("adb sunucusu {addr}: {e}"))?;
        s.write_all(&encode(&format!("host:transport:{serial}"))).await.map_err(|e| e.to_string())?;
        read_status(&mut s).await?;
        s.write_all(&encode(service)).await.map_err(|e| e.to_string())?;
        read_status(&mut s).await?;
        let mut raw = Vec::new();
        (&mut s).take(MAX_REPLY).read_to_end(&mut raw).await.map_err(|e| e.to_string())?;
        service_body(&String::from_utf8_lossy(&raw))
    };
    tokio::time::timeout(QUERY_TIMEOUT, q).await.map_err(|_| format!("adb: {service}: zaman aşımı"))?
}

/// Bir cihazın hazırlanma sonucu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ready {
    /// Kural zaten vardı.
    Already,
    /// Kural kuruldu; `pkg` paneli açıldı (ya da açılması istenmedi).
    Set { pkg: String },
    /// Aday paketlerin hiçbiri kurulu değil (reverse kurulmadı).
    Missing,
}

/// 0.6.9 `Ensure` eşi, tek cihaz için.
///
/// # Errors
/// adb hatası.
pub async fn ensure(cfg: &Settings, addr: &str, serial: &str) -> Result<Ready, String> {
    let rule = format!("tcp:{} tcp:{}", cfg.phone_port, cfg.agent_port);
    let list = device_query(addr, serial, "reverse:list-forward").await?;
    if list.lines().any(|l| l.contains(&rule)) {
        return Ok(Ready::Already);
    }
    let mut chosen = None;
    for p in &cfg.packages {
        if device_query(addr, serial, &format!("shell:pm path {p}")).await?.contains("package:") {
            chosen = Some(p.clone());
            break;
        }
    }
    let Some(pkg) = chosen else { return Ok(Ready::Missing) };
    device_query(addr, serial, &format!("reverse:forward:tcp:{};tcp:{}", cfg.phone_port, cfg.agent_port)).await?;
    if cfg.open_panel {
        device_query(addr, serial, "shell:input keyevent KEYCODE_WAKEUP").await?;
        device_query(addr, serial, &format!("shell:am start -n {}", activity(&pkg))).await?;
    }
    Ok(Ready::Set { pkg })
}

/// Cihaz kümesi değişince yeni gelenleri hazırla; durum yaz.
async fn on_devices(cfg: &Settings, addr: &str, st: &Shared, devices: Vec<String>, known: &mut BTreeSet<String>) {
    let now: BTreeSet<String> = devices.iter().cloned().collect();
    known.retain(|d| now.contains(d));
    let mut missing: Vec<String> = status(st).missing.into_iter().filter(|d| now.contains(d)).collect();
    for d in &devices {
        if known.contains(d) {
            continue;
        }
        match ensure(cfg, addr, d).await {
            Ok(Ready::Missing) => {
                eprintln!("phoned: {d}: uygulama kurulu değil (adaylar: {})", cfg.packages.join(","));
                if !missing.contains(d) {
                    missing.push(d.clone());
                }
            }
            Ok(r) => {
                if let Ready::Set { pkg } = &r {
                    eprintln!("phoned: {d}: adb reverse tcp:{} → {} kuruldu ({pkg})", cfg.phone_port, cfg.agent_port);
                }
                missing.retain(|m| m != d);
                known.insert(d.clone());
            }
            Err(e) => eprintln!("phoned: {d}: {e}"),
        }
    }
    set_status(st, |s| {
        s.devices = devices;
        s.missing = missing;
        s.server = true;
    });
}

/// İzleme döngüsü: kalıcı `track-devices` bağlantısı; koparsa 5 sn sonra yeniden.
pub async fn run(cfg: Settings, st: Shared, serial: impl Fn() -> Option<String>) {
    let Some(addr) = cfg.addr.clone() else { return };
    let mut last_err = String::new();
    loop {
        let mut known = BTreeSet::new();
        let r = track(&cfg, &addr, &st, &serial, &mut known).await;
        set_status(&st, |s| *s = Status::default());
        let msg = r.err().unwrap_or_else(|| "adb izleme bağlantısı kapandı".to_owned());
        if msg != last_err {
            eprintln!("phoned: {msg}; {} sn sonra yeniden", RETRY.as_secs());
            last_err = msg;
        }
        tokio::time::sleep(RETRY).await;
    }
}

async fn track(cfg: &Settings, addr: &str, st: &Shared, serial: &impl Fn() -> Option<String>, known: &mut BTreeSet<String>) -> Result<(), String> {
    let mut s = TcpStream::connect(addr).await.map_err(|e| format!("adb sunucusu {addr}: {e}"))?;
    s.write_all(&encode("host:track-devices")).await.map_err(|e| e.to_string())?;
    read_status(&mut s).await?;
    set_status(st, |x| x.server = true);
    loop {
        let payload = read_frame(&mut s).await?;
        let want = serial();
        on_devices(cfg, addr, st, parse_devices(&payload, want.as_deref()), known).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test")]
mod tests {
    use super::*;

    #[test]
    fn tp_w2_07a_devices_parse_and_filter() {
        let p = "7aa6e0a\tdevice\nemu-1\toffline\nX9\tunauthorized\nZ\tdevice\n";
        assert_eq!(parse_devices(p, None), ["7aa6e0a", "Z"]);
        assert_eq!(parse_devices(p, Some("Z")), ["Z"]);
        assert_eq!(parse_devices(p, Some("")), ["7aa6e0a", "Z"]);
        assert!(parse_devices("", None).is_empty());
        assert_eq!(encode("host:track-devices"), b"0012host:track-devices");
        assert!(hex4(b"zz00").is_err());
        assert_eq!(hex4(b"001a").unwrap(), 26);
    }

    #[test]
    fn tp_w2_07c_packages_and_activity() {
        assert_eq!(parse_packages(""), [DEFAULT_PACKAGE]);
        assert_eq!(parse_packages("com.example.panel, net.oxog.phonebox.deskpanel"), ["com.example.panel", "net.oxog.phonebox.deskpanel"]);
        assert_eq!(parse_packages("kötü;rm -rf,x"), [DEFAULT_PACKAGE], "kabuğa giden ad süzülür");
        assert_eq!(activity("com.example.panel"), "com.example.panel/net.oxog.phonebox.deskpanel.MainActivity");
        assert_eq!(activity(DEFAULT_PACKAGE), "net.oxog.phonebox.deskpanel/net.oxog.phonebox.deskpanel.MainActivity");
    }

    #[test]
    fn tp_w2_07b_service_body_forms() {
        assert_eq!(service_body("OKAY0017abc tcp:18766 tcp:18766").unwrap(), "abc tcp:18766 tcp:18766");
        assert_eq!(service_body("OKAY").unwrap(), "");
        assert_eq!(service_body("package:/data/app/x/base.apk\n").unwrap(), "package:/data/app/x/base.apk\n");
        assert!(service_body("FAIL000bno such dev").is_err());
    }
}
