//! W2.4b ses: ana çıkışın düzeyi + sessiz (`state.audio`, 0.6.9 `AudioSnapshot {volume, muted}`) ve `POST /api/volume`.
//!
//! Kaynak: PipeWire'ın PulseAudio uyumlu yerel soketi (`$XDG_RUNTIME_DIR/pulse/native`). Oturum veri yolunda PipeWire'ın ana
//! düzey için D-Bus arayüzü YOK (w24 ölçümü) ve `wpctl`/`pactl` süreç başlatmak yasak (HP2) → saf-Rust protokol alt kümesi
//! (`proto`), C kütüphanesi yok. Olay güdümlü: sink + sunucu aboneliği; olay gelince tek yeniden sorgu, yoklama yok.
//!
//! Katman: `app` (uç, akış) → `audio` (durum + saf karar) → `audio::client` (soket G/Ç, tek görev, tek yazıcı) → `proto` (saf).
//! Kim yazar: anlık görüntüyü yalnız `client::run`; kim okur: `/api/state`, akış (`watch` tiki), `/api/debug/topics`.

pub mod client;
pub mod http;
pub mod proto;

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// Soket yoksa / koptuysa yeniden deneme aralığı (sıcak döngü yok).
pub const RETRY_MS: u64 = 5_000;
/// Tek istek–yanıt için en uzun bekleme.
pub const CALL_MS: u64 = 3_000;
/// `up` / `down` adımı (0.6.9 `Step(±5)`).
pub const STEP: i64 = 5;

/// 0.6.9 `AudioSnapshot(int Volume, bool Muted)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AudioSnapshot {
    pub volume: i64,
    pub muted: bool,
}

impl AudioSnapshot {
    #[must_use]
    pub fn of(sink: &proto::Sink) -> Self {
        Self { volume: proto::percent(&sink.volumes), muted: sink.mute }
    }
}

/// `POST /api/volume` işlemi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Level(i64),
    Up,
    Down,
    ToggleMute,
}

/// 0.6.9 sırası: `op` önce (`mute` · `up` · `down`), yoksa `level`; ikisi de yoksa / bilinmeyen `op` → `None` (400).
#[must_use]
pub fn parse_op(level: Option<i64>, op: Option<&str>) -> Option<Op> {
    match op {
        Some("mute") => Some(Op::ToggleMute),
        Some("up") => Some(Op::Up),
        Some("down") => Some(Op::Down),
        _ => level.map(Op::Level),
    }
}

/// Sunucuya gidecek değişiklik: tüm kanallara yeni düzey ve/veya sessiz.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub volumes: Option<Vec<u32>>,
    pub mute: Option<bool>,
}

/// İşlem → değişiklik (saf). Düzey verilince 0.6.9 wpctl yolu gibi `level > 0` sessizi kaldırır; `pactl set-sink-volume N%`
/// gibi tüm kanallar aynı düzeye çekilir.
#[must_use]
pub fn plan(op: Op, sink: &proto::Sink) -> Plan {
    let level = match op {
        Op::ToggleMute => return Plan { volumes: None, mute: Some(!sink.mute) },
        Op::Level(l) => l,
        Op::Up => proto::percent(&sink.volumes).saturating_add(STEP),
        Op::Down => proto::percent(&sink.volumes).saturating_sub(STEP),
    };
    let level = level.clamp(0, 100);
    let n = sink.volumes.len().max(1);
    Plan { volumes: Some(vec![proto::from_percent(level); n]), mute: (level > 0 && sink.mute).then_some(false) }
}

/// Soket + çerez yeri.
#[derive(Debug, Clone)]
pub struct Settings {
    pub socket: PathBuf,
    /// Yoksa / okunamazsa sıfır çerez (pipewire-pulse çerezi denetlemez; gerçek PulseAudio için libpulse ile aynı dosya).
    pub cookie: Option<PathBuf>,
}

impl Settings {
    /// `$PULSE_SERVER=unix:<yol>` → o; yoksa `$XDG_RUNTIME_DIR/pulse/native`. Çerez `$PULSE_COOKIE` ya da
    /// `$XDG_CONFIG_HOME/pulse/cookie` (yoksa `~/.config/pulse/cookie`).
    #[must_use]
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let socket = std::env::var("PULSE_SERVER")
            .ok()
            .and_then(|s| s.strip_prefix("unix:").map(PathBuf::from))
            .or_else(|| var("XDG_RUNTIME_DIR").map(|d| d.join("pulse/native")))
            .unwrap_or_else(|| {
                use std::os::unix::fs::MetadataExt as _;
                let uid = std::fs::metadata("/proc/self").map_or(0, |m| m.uid());
                PathBuf::from(format!("/run/user/{uid}/pulse/native"))
            });
        let cookie = var("PULSE_COOKIE")
            .or_else(|| var("XDG_CONFIG_HOME").map(|d| d.join("pulse/cookie")))
            .or_else(|| var("HOME").map(|h| h.join(".config/pulse/cookie")));
        Self { socket, cookie }
    }
}

type Cmd = (Op, oneshot::Sender<Option<AudioSnapshot>>);

/// Paylaşılan ses durumu. Tek yazıcı: `client::run`.
pub struct Audio {
    pub settings: Settings,
    snap: Mutex<Option<AudioSnapshot>>,
    tick: watch::Sender<u64>,
    cmds: mpsc::Sender<Cmd>,
    rx: Mutex<Option<mpsc::Receiver<Cmd>>>,
    pub connected: AtomicBool,
    /// Bağlantı denemesi · ilgili olay · sink sorgusu · değiştirme isteği (K7 `/api/debug/topics`).
    pub connects: AtomicU64,
    pub events: AtomicU64,
    pub queries: AtomicU64,
    pub sets: AtomicU64,
    /// Son bağlantı hatası (boş = bağlı ya da hiç denenmedi).
    pub error: Mutex<String>,
}

impl Audio {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        let (cmds, rx) = mpsc::channel(8);
        Self {
            settings,
            snap: Mutex::new(None),
            tick: watch::channel(0).0,
            cmds,
            rx: Mutex::new(Some(rx)),
            connected: AtomicBool::new(false),
            connects: AtomicU64::new(0),
            events: AtomicU64::new(0),
            queries: AtomicU64::new(0),
            sets: AtomicU64::new(0),
            error: Mutex::new(String::new()),
        }
    }

    /// `state.audio`; kaynak yoksa `None` (0.6.9 `null`).
    #[must_use]
    pub fn snapshot(&self) -> Option<AudioSnapshot> {
        self.snap.lock().map_or(None, |g| *g)
    }

    /// Değişim tiki (akış bekleyen tik olmadan uyanır).
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.tick.subscribe()
    }

    /// Yalnız değiştiyse yazar ve tiki artırır.
    pub fn set(&self, s: Option<AudioSnapshot>) {
        let changed = self.snap.lock().is_ok_and(|mut g| {
            let c = *g != s;
            *g = s;
            c
        });
        if changed {
            self.tick.send_modify(|t| *t = t.wrapping_add(1));
        }
    }

    /// İstemci görevine işlem gönderir; bağlı değilse, kuyruk doluysa ya da yanıt gelmezse `None` (400).
    pub async fn apply(&self, op: Op) -> Option<AudioSnapshot> {
        if !self.connected.load(Ordering::Relaxed) {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        self.cmds.try_send((op, tx)).ok()?;
        tokio::time::timeout(Duration::from_millis(CALL_MS * 2), rx).await.ok()?.ok()?
    }

    /// İstemci görevi alıcıyı bir kez alır (ikinci `run` hiçbir şey yapmaz).
    fn take_rx(&self) -> Option<mpsc::Receiver<Cmd>> {
        self.rx.lock().ok()?.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(v: &[u32], mute: bool) -> proto::Sink {
        proto::Sink { index: 7, name: "s".into(), volumes: v.to_vec(), mute }
    }

    #[test]
    fn tp_w24b_a5_parse_op_order() {
        assert_eq!(parse_op(Some(30), Some("mute")), Some(Op::ToggleMute));
        assert_eq!(parse_op(None, Some("up")), Some(Op::Up));
        assert_eq!(parse_op(None, Some("down")), Some(Op::Down));
        assert_eq!(parse_op(Some(30), None), Some(Op::Level(30)));
        assert_eq!(parse_op(Some(30), Some("yok")), Some(Op::Level(30)));
        assert_eq!(parse_op(None, Some("yok")), None);
        assert_eq!(parse_op(None, None), None);
    }

    #[test]
    fn tp_w24b_a5_plan_level_clamp_and_unmute() {
        let s = sink(&[0, 0], true);
        assert_eq!(plan(Op::Level(40), &s), Plan { volumes: Some(vec![26_214, 26_214]), mute: Some(false) });
        assert_eq!(plan(Op::Level(0), &s), Plan { volumes: Some(vec![0, 0]), mute: None }, "0 sessizi kaldırmaz");
        assert_eq!(plan(Op::Level(150), &sink(&[1], false)), Plan { volumes: Some(vec![proto::NORM]), mute: None });
        assert_eq!(plan(Op::Level(-5), &sink(&[], false)), Plan { volumes: Some(vec![0]), mute: None });
    }

    #[test]
    fn tp_w24b_a5_plan_step_and_toggle() {
        let s = sink(&[proto::from_percent(50), proto::from_percent(30)], false);
        assert_eq!(plan(Op::Up, &s).volumes, Some(vec![proto::from_percent(55); 2]));
        assert_eq!(plan(Op::Down, &s).volumes, Some(vec![proto::from_percent(45); 2]));
        assert_eq!(plan(Op::Up, &sink(&[proto::NORM], false)).volumes, Some(vec![proto::NORM]));
        assert_eq!(plan(Op::Down, &sink(&[proto::from_percent(3)], false)).volumes, Some(vec![0]));
        assert_eq!(plan(Op::ToggleMute, &s), Plan { volumes: None, mute: Some(true) });
        assert_eq!(plan(Op::ToggleMute, &sink(&[0], true)), Plan { volumes: None, mute: Some(false) });
    }

    #[test]
    fn set_ticks_only_on_change() {
        let a = Audio::new(Settings { socket: "/yok".into(), cookie: None });
        let rx = a.watch();
        a.set(None);
        assert_eq!(*rx.borrow(), 0);
        let v = Some(AudioSnapshot { volume: 3, muted: false });
        a.set(v);
        a.set(v);
        assert_eq!(*rx.borrow(), 1);
        assert_eq!(a.snapshot(), v);
    }

    #[tokio::test]
    async fn apply_without_connection_is_none() {
        let a = Audio::new(Settings { socket: "/yok".into(), cookie: None });
        assert_eq!(a.apply(Op::Level(10)).await, None);
    }
}
