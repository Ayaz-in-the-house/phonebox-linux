//! `state.pc.gpu` (W2.5): NVIDIA GPU, 0.6.9 `SystemMonitor.GpuLoop` eşi — tek kalıcı `nvidia-smi … -lms 1000` çocuğu, satır başına bir örnek.
//!
//! 0.6.9'dan farkı abonelik kapısı (HP4): çocuk yalnız örnekleyici akarken (biri bakıyor) yaşar; örnek `idle_stop` boyunca gelmezse
//! çocuk sonlandırılır (kendi çocuğu, PID'i elde) ve değer `null` olur. Kararlı hâlde fork+exec 0 (TP-W2-07): oturum başına iki exec
//! (ad sorgusu yalnız ilk oturumda + döngü). Program bulunamazsa bir kez denenir, bir daha fork yok (NVIDIA'sız makine).
//! Eşleme `SystemMonitor.cs:96`: `[N/A]` → null, bellek MiB/1024 (GB).

use crate::hub::PcSnapshot;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::Command;
use tokio::sync::watch;
use tokio::time::Instant;

/// Döngü sorgusu (0.6.9 ile aynı alanlar ve sıra).
pub const QUERY: &str = "--query-gpu=utilization.gpu,temperature.gpu,power.draw,power.limit,memory.used,memory.total,fan.speed,clocks.gr";
const NAME_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Settings {
    /// `nvidia-smi` (PATH'ten) ya da test betiği.
    pub program: PathBuf,
    /// Son örnekten bu kadar sonra çocuk kapanır.
    pub idle_stop: Duration,
    /// Çocuk kendiliğinden ölürse yeniden deneme aralığı.
    pub retry: Duration,
}

impl Settings {
    #[must_use]
    pub fn system() -> Self {
        Self { program: "nvidia-smi".into(), idle_stop: Duration::from_secs(3), retry: Duration::from_secs(10) }
    }
}

/// Paylaşılan GPU değeri. Tek yazıcı: `run`.
pub struct Gpu {
    pub settings: Settings,
    value: Mutex<Option<Value>>,
    spawns: AtomicU64,
}

impl Gpu {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self { settings, value: Mutex::new(None), spawns: AtomicU64::new(0) }
    }

    /// Son örnek (`None` = GPU yok / kimse bakmıyor).
    #[must_use]
    pub fn snapshot(&self) -> Option<Value> {
        self.value.lock().map_or(None, |g| g.clone())
    }

    /// Başlatılan süreç sayısı (ad sorgusu + döngü).
    #[must_use]
    pub fn spawns(&self) -> u64 {
        self.spawns.load(Ordering::Relaxed)
    }

    fn set(&self, v: Option<Value>) {
        if let Ok(mut g) = self.value.lock() {
            *g = v;
        }
    }
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// CSV satırı → 0.6.9 `GpuInfo` (camelCase). 8 alandan az → `None`.
#[must_use]
pub fn parse_line(name: &str, line: &str) -> Option<Value> {
    let f: Vec<Option<f64>> = line.split(',').map(num).collect();
    let [util, temp, power, limit, used, total, fan, clock] = f.get(..8)? else { return None };
    Some(json!({
        "name": name,
        "util": util,
        "temp": temp,
        "power": power,
        "powerLimit": limit,
        "vramUsed": used.map(|v| v / 1024.0),
        "vramTotal": total.map(|v| v / 1024.0),
        "fan": fan,
        "clock": clock,
    }))
}

enum End {
    /// Örnek gelmedi → bekle.
    Idle,
    /// Çocuk kendiliğinden bitti → `retry` sonra yeniden.
    Exited,
    /// Program yok → bir daha deneme.
    Missing,
}

fn spawn_err(e: &std::io::Error) -> End {
    if e.kind() == std::io::ErrorKind::NotFound { End::Missing } else { End::Exited }
}

async fn query_name(gpu: &Gpu) -> Result<String, End> {
    gpu.spawns.fetch_add(1, Ordering::Relaxed);
    let mut c = Command::new(&gpu.settings.program);
    c.args(["--query-gpu=name", "--format=csv,noheader"]).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(NAME_TIMEOUT, c.output()).await.map_err(|_| End::Exited)?.map_err(|e| spawn_err(&e))?;
    let name = String::from_utf8_lossy(&out.stdout).lines().next().map(str::trim).unwrap_or("").to_owned();
    Ok(if name.is_empty() { "GPU".to_owned() } else { name })
}

/// Bir oturum: çocuğu başlat, satırları oku, örnek durunca çocuğu kapat.
async fn session(gpu: &Gpu, rx: &mut watch::Receiver<Option<Arc<PcSnapshot>>>, name: &mut Option<String>) -> End {
    let nm = match name.clone() {
        Some(n) => n,
        None => match query_name(gpu).await {
            Ok(n) => name.insert(n).clone(),
            Err(e) => return e,
        },
    };
    gpu.spawns.fetch_add(1, Ordering::Relaxed);
    let mut c = Command::new(&gpu.settings.program);
    c.args([QUERY, "--format=csv,noheader,nounits", "-lms", "1000"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return spawn_err(&e),
    };
    let Some(out) = child.stdout.take() else {
        let _ = child.kill().await;
        return End::Exited;
    };
    let mut lines = BufReader::new(out).lines();
    let mut deadline = Instant::now() + gpu.settings.idle_stop;
    loop {
        tokio::select! {
            l = lines.next_line() => match l {
                Ok(Some(l)) => {
                    if let Some(v) = parse_line(&nm, &l) {
                        gpu.set(Some(v));
                    }
                }
                _ => {
                    let _ = child.wait().await;
                    return End::Exited;
                }
            },
            c = rx.changed() => {
                if c.is_err() {
                    let _ = child.kill().await;
                    return End::Idle;
                }
                deadline = Instant::now() + gpu.settings.idle_stop;
            }
            () = tokio::time::sleep_until(deadline) => {
                // Kendi çocuğumuz (PID elde): kill + wait → zombi yok.
                let _ = child.kill().await;
                return End::Idle;
            }
        }
    }
}

/// GPU görevi: örnekleyici uyurken hiçbir şey yapmaz (watch değişmez → uyanma yok).
pub async fn run(gpu: Arc<Gpu>, mut rx: watch::Receiver<Option<Arc<PcSnapshot>>>) {
    let mut name = None;
    loop {
        if rx.changed().await.is_err() {
            return;
        }
        let end = session(&gpu, &mut rx, &mut name).await;
        gpu.set(None);
        match end {
            End::Idle => {}
            End::Exited => tokio::time::sleep(gpu.settings.retry).await,
            End::Missing => return,
        }
    }
}
