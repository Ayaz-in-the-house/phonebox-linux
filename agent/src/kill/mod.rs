//! Korumalı süreç kapatma (TP-EX-04). Tasarım: `EXPERIENCE-ARCHITECTURE.md` § Süreç kapatma · politika: `docs/KILL-POLICY.md`.
//!
//! Sıra (her istek): uç kapalı mı → hedefi taze taramada bul → `judge` (sabit ret listesi önce, sonra uygunluk) →
//! ilk istek `SIGTERM` · aynı hedefe ≥ 5 sn sonra gelen ikinci istek (= telefondaki ikinci onay) hâlâ yaşıyorsa `SIGKILL`.
//! Sinyal `pidfd` ile gönderilir ve tarama kimliği (`start`) yeniden doğrulanır: PID yeniden kullanılmışsa sinyal gitmez.
//! Her istek tek günlük satırı (`journalctl --user -u phoned`). Varsayılan KAPALI: politika dosyası yoksa her istek 403.

pub mod http;
pub mod linux;

use crate::procs::ProcInfo;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// SIGTERM'den sonra SIGKILL'e izin vermeden önce bekleme.
pub const GRACE_MS: u64 = 5_000;
/// Bekleyen SIGTERM kaydının ömrü (sonra yeni istek yine nazik başlar).
pub const PENDING_TTL_MS: u64 = 60_000;

/// Sabit ret listesi — `comm` (15 bayta kesik) ya da `argv[0]` dosya adı. Yapılandırmayla AÇILAMAZ.
/// Kaynak: tasarım belgesi (terminaller, herdr, tarayıcı ana süreci, `systemd --user`, gnome-shell, phoned, wa-gateway/chromium)
/// + 0.6.9 `UnixProc.Protected` (oturumu bozacak sistem süreçleri) + ajan oturumları (`claude`).
pub const DENY_NAMES: &[&str] = &[
    // terminal emülatörleri
    "ghostty",
    "kitty",
    "alacritty",
    "wezterm",
    "wezterm-gui",
    "foot",
    "footclient",
    "gnome-terminal-server",
    "gnome-terminal",
    "ptyxis",
    "ptyxis-agent",
    "konsole",
    "xterm",
    "tilix",
    "terminator",
    "kgx",
    "blackbox",
    "rio",
    "warp",
    // çoklayıcı / ajanlar
    "herdr",
    "tmux",
    "screen",
    "zellij",
    "claude",
    // tarayıcılar (ana süreç; sekme/renderer grubu ayrı karar → şimdilik kapsam kuralıyla da kapalı)
    "firefox",
    "firefox-bin",
    "zen",
    "zen-bin",
    "brave",
    "brave-browser",
    "chrome",
    "google-chrome",
    "chromium",
    "chromium-browser",
    // oturum
    "systemd",
    "init",
    "kthreadd",
    "Xorg",
    "Xwayland",
    "gnome-shell",
    "gnome-session-binary",
    "gnome-session",
    "plasmashell",
    "kwin_x11",
    "kwin_wayland",
    "mutter",
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
    "pulseaudio",
    "dbus-daemon",
    "dbus-broker",
    "dbus-broker-launch",
    "sshd",
    "login",
    "gdm",
    "sddm",
    "lightdm",
    "NetworkManager",
    "polkitd",
    "udisksd",
    "upowerd",
    "xdg-desktop-portal",
    // biz
    "phoned",
    "PhoneBoxAgent",
    "adb",
    "dotnet",
];

/// Kapsam (cgroup birimi) adında geçerse ret: terminal, ajan oturumu, tarayıcı, wa-gateway.
pub const DENY_UNIT_WORDS: &[&str] = &[
    "claude",
    "herdr",
    "ghostty",
    "kitty",
    "alacritty",
    "wezterm",
    "foot",
    "terminal",
    "ptyxis",
    "konsole",
    "tilix",
    "xterm",
    "firefox",
    "zen",
    "brave",
    "chrome",
    "chromium",
    "wa-gateway",
    "gnome-shell",
    "phoned",
    "phonebox",
];

/// `cmdline` içinde geçerse ret (PM-009: WhatsApp köprüsünün tarayıcısı ana tarayıcı adını taşımayabilir).
pub const DENY_CMDLINE_WORDS: &[&str] = &["wa-gateway", "wa-gateway-live"];

/// Kapatma politikası (`<ayar dizini>/kill-policy.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub allow_kill: bool,
    /// `app-*.scope` dışındaki süreçler için açık izin (ad = `comm`). Ret listesi yine kazanır.
    #[serde(default)]
    pub allow: Vec<String>,
}

impl Policy {
    /// Dosya yok → kapalı. Bozuk dosya → kapalı + günlük (sessiz hata yok, güvenli varsayılan).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!("phoned: kill: {} okunamadı ({e}); kapatma kapalı", path.display());
                Self::default()
            }
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                eprintln!("phoned: kill: {} geçersiz ({e}); kapatma kapalı", path.display());
                Self::default()
            }),
        }
    }
}

/// Karar bağlamı.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub self_pid: i32,
    pub uid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Adı ret listesinde mi (`comm` 15 bayta kesildiği için listedeki adın ilk 15 baytıyla da karşılaştırılır).
fn name_denied(p: &ProcInfo) -> Option<&'static str> {
    let argv0 = base(p.cmdline.split(' ').next().unwrap_or(""));
    DENY_NAMES.iter().copied().find(|d| p.comm == *d || p.comm == d.get(..15).unwrap_or(d) || argv0 == *d)
}

fn cmdline_denied(p: &ProcInfo) -> bool {
    let c = p.cmdline.to_ascii_lowercase();
    DENY_CMDLINE_WORDS.iter().any(|w| c.contains(w))
}

fn unit_denied(unit: &str) -> bool {
    let u = unit.to_ascii_lowercase();
    DENY_UNIT_WORDS.iter().any(|w| u.contains(w))
}

/// Uygulama kapsamı: `app-*.scope` (GNOME/systemd masaüstü uygulaması başlatması).
fn app_scope(unit: &str) -> bool {
    unit.starts_with("app-") && unit.ends_with(".scope")
}

/// Bu süreç kapatılabilir mi. `scope` = taramadaki aynı birimin üyeleri (kapsamda korumalı biri varsa ret).
/// Sıra önemlidir: ret kuralları izinlerden ÖNCE gelir; izin listesi ret listesini aşamaz.
#[must_use]
pub fn judge(p: &ProcInfo, scope: &[&ProcInfo], ctx: Ctx, allow: &[String]) -> Verdict {
    let deny = |m: String| Verdict::Deny(m);
    if p.pid <= 1 {
        return deny("init/çekirdek süreci".into());
    }
    if p.pid == ctx.self_pid {
        return deny("phoned kendisi".into());
    }
    if p.uid != ctx.uid {
        return deny("başka kullanıcının süreci".into());
    }
    if let Some(n) = name_denied(p) {
        return deny(format!("{n} korumalı (sabit ret listesi)"));
    }
    if cmdline_denied(p) {
        return deny("wa-gateway/WhatsApp köprüsü korumalı (PM-009)".into());
    }
    if unit_denied(&p.unit) {
        return deny(format!("korumalı kapsam: {}", p.unit));
    }
    if allow.contains(&p.comm) {
        return Verdict::Allow;
    }
    if !app_scope(&p.unit) {
        return deny(format!("uygulama değil (app-*.scope dışı: {})", if p.unit.is_empty() { "?" } else { &p.unit }));
    }
    if scope.iter().any(|m| m.uid != ctx.uid || name_denied(m).is_some() || cmdline_denied(m)) {
        return deny(format!("kapsamda korumalı süreç var: {}", p.unit));
    }
    Verdict::Allow
}

/// Taramadaki kapatılamaz PID'ler (panelin `locked` sütunu) — birim üyeleri bir kez gruplanır (O(n)).
#[must_use]
pub fn locked_set(all: &[ProcInfo], ctx: Ctx, allow: &[String]) -> std::collections::HashSet<i32> {
    let mut units: HashMap<&str, Vec<&ProcInfo>> = HashMap::new();
    for p in all.iter().filter(|p| !p.unit.is_empty()) {
        units.entry(p.unit.as_str()).or_default().push(p);
    }
    all.iter()
        .filter(|p| {
            let scope = units.get(p.unit.as_str()).map_or(&[][..], Vec::as_slice);
            matches!(judge(p, scope, ctx, allow), Verdict::Deny(_))
        })
        .map(|p| p.pid)
        .collect()
}

/// Taramanın tamamına göre karar.
#[must_use]
pub fn judge_in(p: &ProcInfo, all: &[ProcInfo], ctx: Ctx, allow: &[String]) -> Verdict {
    let scope: Vec<&ProcInfo> = if p.unit.is_empty() { Vec::new() } else { all.iter().filter(|m| m.unit == p.unit).collect() };
    judge(p, &scope, ctx, allow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Sig {
    #[serde(rename = "TERM")]
    Term,
    #[serde(rename = "KILL")]
    Kill,
}

/// Sinyal gönderici sınırı (testlerde kayıtçı; Linux'ta pidfd).
pub trait Signaller: Send + Sync {
    /// `pid` hâlâ `start` kimliğini taşıyorsa sinyal gönder.
    ///
    /// # Errors
    /// Süreç yok / kimlik değişti / izin yok.
    fn send(&self, pid: i32, start: u64, sig: Sig) -> std::io::Result<()>;
    /// Hâlâ yaşıyor mu (aynı kimlikle).
    fn alive(&self, pid: i32, start: u64) -> bool;
}

/// 0.6.9 `KillResult` + `signal` / `pids` (fazla anahtar paneli bozmaz).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KillResult {
    pub killed: usize,
    pub failed: usize,
    pub error: Option<String>,
    pub signal: Option<Sig>,
    pub pids: Vec<i32>,
}

/// Uç yanıtının sınıfı (HTTP durumu `app`'te seçilir).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Denied,
    Conflict,
}

/// K7 sayaçları.
#[derive(Debug, Default)]
pub struct Counters {
    pub requests: AtomicU64,
    pub denied: AtomicU64,
    pub term: AtomicU64,
    pub kill: AtomicU64,
    pub failed: AtomicU64,
}

/// Bekleyen SIGTERM'ler: (pid, start) → gönderildiği an (ms).
pub struct Killer {
    sig: Box<dyn Signaller>,
    pending: Mutex<HashMap<(i32, u64), u64>>,
    pub counters: Counters,
}

/// Hedef: tek PID ya da bir adın tüm örnekleri.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Pid(i32),
    Name(String),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pid(p) => write!(f, "pid={p}"),
            Self::Name(n) => write!(f, "name={n:?}"),
        }
    }
}

fn result(error: Option<String>, killed: usize, failed: usize, signal: Option<Sig>, pids: Vec<i32>) -> KillResult {
    KillResult { killed, failed, error, signal, pids }
}

impl Killer {
    #[must_use]
    pub fn new(sig: Box<dyn Signaller>) -> Self {
        Self { sig, pending: Mutex::new(HashMap::new()), counters: Counters::default() }
    }

    /// Bir isteği uygula. `scan` = taze tarama, `now_ms` = tekdüze saat.
    pub fn request(&self, policy: &Policy, target: &Target, scan: &[ProcInfo], ctx: Ctx, now_ms: u64) -> (Outcome, KillResult) {
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        let (outcome, r) = self.decide(policy, target, scan, ctx, now_ms);
        match outcome {
            Outcome::Denied => self.counters.denied.fetch_add(1, Ordering::Relaxed),
            Outcome::Conflict => self.counters.failed.fetch_add(1, Ordering::Relaxed),
            Outcome::Done => 0,
        };
        eprintln!(
            "phoned: kill {target} → {} signal={} killed={} failed={} pids={:?}{}",
            match outcome {
                Outcome::Done => "ok",
                Outcome::Denied => "reddedildi",
                Outcome::Conflict => "yapılmadı",
            },
            match r.signal {
                Some(Sig::Term) => "TERM",
                Some(Sig::Kill) => "KILL",
                None => "-",
            },
            r.killed,
            r.failed,
            r.pids,
            r.error.as_deref().map(|e| format!(" neden={e}")).unwrap_or_default(),
        );
        (outcome, r)
    }

    fn decide(&self, policy: &Policy, target: &Target, scan: &[ProcInfo], ctx: Ctx, now_ms: u64) -> (Outcome, KillResult) {
        if !policy.allow_kill {
            return (Outcome::Denied, result(Some("süreç kapatma kapalı (kill-policy.json allowKill=false)".into()), 0, 0, None, vec![]));
        }
        let victims: Vec<&ProcInfo> = match target {
            Target::Pid(pid) => scan.iter().filter(|p| p.pid == *pid).collect(),
            Target::Name(n) if n.trim().is_empty() => return (Outcome::Conflict, result(Some("ad verilmedi".into()), 0, 0, None, vec![])),
            Target::Name(n) => scan.iter().filter(|p| p.comm == *n).collect(),
        };
        // Kendi PID'imiz taramada olmasa bile (test kaynağı) reddedilir; init de.
        if let Target::Pid(pid) = target
            && (*pid <= 1 || *pid == ctx.self_pid)
        {
            let why = if *pid <= 1 { "init/çekirdek süreci" } else { "phoned kendisi" };
            return (Outcome::Denied, result(Some(why.into()), 0, 0, None, vec![]));
        }
        if victims.is_empty() {
            return (Outcome::Conflict, result(Some("süreç bulunamadı".into()), 0, 0, None, vec![]));
        }
        // Grupta tek bir korumalı örnek varsa hiçbirine dokunulmaz (kısmi kapatma yok).
        for v in &victims {
            if let Verdict::Deny(why) = judge_in(v, scan, ctx, &policy.allow) {
                return (Outcome::Denied, result(Some(why), 0, victims.len(), None, vec![]));
            }
        }
        self.signal_all(&victims, now_ms)
    }

    fn signal_all(&self, victims: &[&ProcInfo], now_ms: u64) -> (Outcome, KillResult) {
        let Ok(mut pending) = self.pending.lock() else {
            return (Outcome::Conflict, result(Some("iç hata (kilit)".into()), 0, victims.len(), None, vec![]));
        };
        pending.retain(|_, at| now_ms.saturating_sub(*at) < PENDING_TTL_MS);
        // Hepsi için aynı adım: hiçbiri beklemedeyse TERM; hepsi ≥ 5 sn bekledi ise KILL; arada → bekle.
        let ages: Vec<Option<u64>> = victims.iter().map(|v| pending.get(&(v.pid, v.start)).map(|at| now_ms.saturating_sub(*at))).collect();
        let sig = if ages.iter().all(Option::is_none) {
            Sig::Term
        } else if let Some(wait) = ages.iter().flatten().map(|a| GRACE_MS.saturating_sub(*a)).max().filter(|w| *w > 0) {
            return (
                Outcome::Conflict,
                result(Some(format!("SIGTERM gönderildi; zorla kapatmak için {} sn sonra yeniden onayla", wait.div_ceil(1000))), 0, 0, None, vec![]),
            );
        } else {
            Sig::Kill
        };
        let (mut ok, mut failed, mut pids, mut last_err) = (0, 0, Vec::new(), None);
        for v in victims {
            if sig == Sig::Kill && !self.sig.alive(v.pid, v.start) {
                pending.remove(&(v.pid, v.start));
                continue;
            }
            match self.sig.send(v.pid, v.start, sig) {
                Ok(()) => {
                    ok += 1;
                    pids.push(v.pid);
                    if sig == Sig::Term {
                        pending.insert((v.pid, v.start), now_ms);
                    } else {
                        pending.remove(&(v.pid, v.start));
                    }
                }
                Err(e) => {
                    failed += 1;
                    last_err = Some(e.to_string());
                }
            }
        }
        let c = if sig == Sig::Term { &self.counters.term } else { &self.counters.kill };
        c.fetch_add(u64::try_from(ok).unwrap_or(u64::MAX), Ordering::Relaxed);
        if ok == 0 && failed == 0 {
            return (Outcome::Done, result(None, 0, 0, Some(sig), pids));
        }
        let outcome = if ok > 0 { Outcome::Done } else { Outcome::Conflict };
        (outcome, result(if ok == 0 { last_err } else { None }, ok, failed, Some(sig), pids))
    }
}

#[doc(hidden)]
pub mod fake {
    //! (Test düzeneği; entegrasyon testleri de kullanır, üretimde bağlanmaz.)
    //! Kayıtçı gönderici: hiçbir gerçek sinyal gitmez.
    use super::{Sig, Signaller};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct Recorder {
        pub sent: Arc<Mutex<Vec<(i32, Sig)>>>,
        pub dead: Arc<Mutex<Vec<i32>>>,
    }

    impl Signaller for Recorder {
        fn send(&self, pid: i32, _start: u64, sig: Sig) -> std::io::Result<()> {
            if let Ok(mut v) = self.sent.lock() {
                v.push((pid, sig));
            }
            Ok(())
        }
        fn alive(&self, pid: i32, _start: u64) -> bool {
            self.dead.lock().map(|d| !d.contains(&pid)).unwrap_or(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: u32 = 1000;
    const CTX: Ctx = Ctx { self_pid: 4242, uid: UID };

    fn p(pid: i32, comm: &str, unit: &str) -> ProcInfo {
        ProcInfo { pid, comm: comm.into(), uid: UID, unit: unit.into(), cmdline: comm.into(), start: 7, ..ProcInfo::default() }
    }

    const APP: &str = "app-gnome-org.gnome.Loupe-1234.scope";

    #[test]
    fn tp_w24_k1_fixed_deny_list() {
        for name in ["ghostty", "kitty", "herdr", "gnome-shell", "systemd", "phoned", "claude", "firefox", "zen", "chromium", "Xwayland", "pipewire"]
        {
            assert!(matches!(judge(&p(100, name, APP), &[], CTX, &[name.to_owned()]), Verdict::Deny(_)), "{name} izin listesinde bile reddedilmeli");
        }
        // comm 15 bayta kesilir.
        assert!(matches!(judge(&p(100, "gnome-terminal-", APP), &[], CTX, &[]), Verdict::Deny(_)));
        // argv[0] tam yol.
        let mut x = p(100, "MainThread", APP);
        x.cmdline = "/usr/lib64/firefox/firefox -contentproc".into();
        assert!(matches!(judge(&x, &[], CTX, &[]), Verdict::Deny(_)));
        // wa-gateway tarayıcısı başka adla.
        let mut w = p(100, "headless_shell", APP);
        w.cmdline = "/opt/x/headless_shell --user-data-dir=/home/u/projects/Wa-Gateway/data".into();
        assert!(matches!(judge(&w, &[], CTX, &["headless_shell".into()]), Verdict::Deny(_)));
    }

    #[test]
    fn tp_w24_k1_self_init_other_user_and_units() {
        assert!(matches!(judge(&p(4242, "loupe", APP), &[], CTX, &[]), Verdict::Deny(_)));
        assert!(matches!(judge(&p(1, "loupe", APP), &[], CTX, &[]), Verdict::Deny(_)));
        let mut other = p(100, "loupe", APP);
        other.uid = 0;
        assert!(matches!(judge(&other, &[], CTX, &["loupe".into()]), Verdict::Deny(_)));
        for unit in [
            "app-claude-session-1952233.scope",
            "herdr-server.service",
            "app-com.mitchellh.ghostty-9.scope",
            "app-org.chromium.Chromium-1.scope",
            "wa-gateway-core.service",
        ] {
            assert!(matches!(judge(&p(100, "zsh", unit), &[], CTX, &["zsh".into()]), Verdict::Deny(_)), "{unit}");
        }
    }

    #[test]
    fn tp_w24_k2_eligibility() {
        let me = p(100, "loupe", APP);
        assert_eq!(judge(&me, &[&me], CTX, &[]), Verdict::Allow);
        // Servis / oturum dilimi uygulama değil.
        assert!(matches!(judge(&p(100, "syncthing", "syncthing.service"), &[], CTX, &[]), Verdict::Deny(_)));
        assert!(matches!(judge(&p(100, "sleep", ""), &[], CTX, &[]), Verdict::Deny(_)));
        // İzin listesi kapsam dışını açar.
        assert_eq!(judge(&p(100, "sleep", "session-3.scope"), &[], CTX, &["sleep".into()]), Verdict::Allow);
        // Kapsamda korumalı biri varsa ret.
        let sh = p(101, "kitty", APP);
        assert!(matches!(judge(&me, &[&me, &sh], CTX, &[]), Verdict::Deny(_)));
    }

    fn on() -> Policy {
        Policy { allow_kill: true, allow: vec!["sleep".into()] }
    }

    #[test]
    fn tp_ex_04a_disabled_by_default() {
        let rec = fake::Recorder::default();
        let k = Killer::new(Box::new(rec.clone()));
        let scan = vec![p(100, "sleep", "")];
        let (o, r) = k.request(&Policy::default(), &Target::Pid(100), &scan, CTX, 0);
        assert_eq!(o, Outcome::Denied);
        assert!(r.error.is_some());
        assert!(rec.sent.lock().unwrap().is_empty());
        assert_eq!(Policy::load(Path::new("/nonexistent/kill-policy.json")), Policy::default());
    }

    #[test]
    fn tp_ex_04b_denied_targets_send_nothing() {
        let rec = fake::Recorder::default();
        let k = Killer::new(Box::new(rec.clone()));
        let scan = vec![
            p(10, "gnome-shell", "org.gnome.Shell@user.service"),
            p(11, "herdr", "herdr-server.service"),
            p(4242, "phoned", ""),
            p(1, "systemd", ""),
        ];
        for t in [Target::Name("gnome-shell".into()), Target::Name("herdr".into()), Target::Pid(4242), Target::Pid(1)] {
            let (o, _) = k.request(&on(), &t, &scan, CTX, 0);
            assert_eq!(o, Outcome::Denied, "{t}");
        }
        assert!(rec.sent.lock().unwrap().is_empty());
        assert_eq!(k.counters.denied.load(Ordering::Relaxed), 4);
        // Ad grubunda tek korumalı örnek → hiçbiri.
        let mixed = vec![p(20, "sleep", ""), ProcInfo { uid: 0, ..p(21, "sleep", "") }];
        assert_eq!(k.request(&on(), &Target::Name("sleep".into()), &mixed, CTX, 0).0, Outcome::Denied);
        assert!(rec.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn tp_ex_04d_term_then_wait_then_kill() {
        let rec = fake::Recorder::default();
        let k = Killer::new(Box::new(rec.clone()));
        let scan = vec![p(100, "sleep", "")];
        let t = Target::Pid(100);
        let (o, r) = k.request(&on(), &t, &scan, CTX, 1_000);
        assert_eq!((o, r.signal, r.killed), (Outcome::Done, Some(Sig::Term), 1));
        let (o, r) = k.request(&on(), &t, &scan, CTX, 1_000 + GRACE_MS - 1);
        assert_eq!((o, r.signal), (Outcome::Conflict, None), "5 sn dolmadan SIGKILL yok");
        let (o, r) = k.request(&on(), &t, &scan, CTX, 1_000 + GRACE_MS);
        assert_eq!((o, r.signal, r.killed), (Outcome::Done, Some(Sig::Kill), 1));
        assert_eq!(*rec.sent.lock().unwrap(), vec![(100, Sig::Term), (100, Sig::Kill)]);
        // TERM ile ölmüşse ikinci onay sinyal göndermez.
        let (_, _) = k.request(&on(), &Target::Pid(100), &scan, CTX, 100_000);
        rec.dead.lock().unwrap().push(100);
        let (o, r) = k.request(&on(), &Target::Pid(100), &scan, CTX, 100_000 + GRACE_MS);
        assert_eq!((o, r.killed), (Outcome::Done, 0));
        assert_eq!(rec.sent.lock().unwrap().len(), 3);
        // Bekleme kaydı süresi dolunca yeniden nazik başlar.
        let (_, r) = k.request(&on(), &Target::Pid(100), &scan, CTX, 300_000);
        assert_eq!(r.signal, Some(Sig::Term));
    }

    #[test]
    fn not_found_and_empty_name() {
        let k = Killer::new(Box::new(fake::Recorder::default()));
        assert_eq!(k.request(&on(), &Target::Pid(999), &[], CTX, 0).0, Outcome::Conflict);
        assert_eq!(k.request(&on(), &Target::Name(" ".into()), &[], CTX, 0).0, Outcome::Conflict);
    }

    #[test]
    fn policy_parse() {
        let d = std::env::temp_dir().join(format!("phoned-kp-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("kill-policy.json");
        std::fs::write(&f, r#"{"allowKill":true,"allow":["sleep"]}"#).unwrap();
        assert_eq!(Policy::load(&f), Policy { allow_kill: true, allow: vec!["sleep".into()] });
        std::fs::write(&f, r#"{"allowKill":true,"extra":1}"#).unwrap();
        assert_eq!(Policy::load(&f), Policy::default(), "bilinmeyen anahtar → kapalı");
        let _ = std::fs::remove_dir_all(&d);
    }
}
