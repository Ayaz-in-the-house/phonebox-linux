//! `pc` konusu: örnekleyici + abone kapısı (deckd D11 deseni: abone yoksa iş yok).
//!
//! Örnekleyici yalnız şunlardan biri doğruyken uyanık: akışa bağlı en az bir istemci var · son `/api/state` çekmesinden
//! bu yana < 5 sn. Aksi hâlde `Notify` üzerinde uyur (0 sistem çağrısı). Uyanınca hız tabanı yeniden alınır
//! (uzun uykunun ortalaması "şimdiki CPU" diye gösterilmez).

use crate::collect::{Collector, CpuTimes, Disk, cpu_percent, kb_to_gb1};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};

/// Çekme aboneliğinin ömrü.
pub const PULL_LINGER_MS: u64 = 5_000;
/// Örnek aralığı (K3: bakılırken 1 Hz).
pub const SAMPLE_MS: u64 = 1_000;

/// 0.6.9 `PcSnapshot` + K5 swap alanları. `gpu`: W2.5 `crate::gpu` (abonelik kapılı tek `nvidia-smi` çocuğu); GPU yoksa `null`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcSnapshot {
    pub cpu: Option<f64>,
    pub cpu_temp: Option<f64>,
    pub cpu_name: String,
    pub ram_used_gb: f64,
    pub ram_total_gb: f64,
    pub gpu: Option<serde_json::Value>,
    pub net_down_bps: f64,
    pub net_up_bps: f64,
    pub disks: Vec<Disk>,
    pub uptime_sec: u64,
    pub swap_total_gb: f64,
    pub swap_used_gb: f64,
}

/// Okumaları anlık görüntüye çevirir; yavaş alanlar seyrek: sıcaklık 5 örnekte bir, disk 15 örnekte bir (0.6.9: 15 sn).
pub struct Sampler {
    col: Box<dyn Collector>,
    prev_cpu: Option<CpuTimes>,
    prev_net: Option<(u64, u64, u64)>,
    temp: Option<f64>,
    disks: Vec<Disk>,
    n: u64,
    gpu: Option<Arc<crate::gpu::Gpu>>,
}

impl Sampler {
    #[must_use]
    pub fn new(col: Box<dyn Collector>) -> Self {
        Self { col, prev_cpu: None, prev_net: None, temp: None, disks: Vec::new(), n: 0, gpu: None }
    }

    /// Hız tabanını sıfırla ve yeniden al (uykudan sonra).
    pub fn rebase(&mut self, now_ms: u64) {
        self.prev_cpu = self.col.cpu_times();
        self.prev_net = self.col.net_bytes().map(|(r, t)| (r, t, now_ms));
    }

    /// GPU kaynağını bağla (W2.5): her örnek son `nvidia-smi` satırını taşır.
    #[must_use]
    pub fn with_gpu(mut self, gpu: Arc<crate::gpu::Gpu>) -> Self {
        self.gpu = Some(gpu);
        self
    }

    pub fn sample(&mut self, now_ms: u64) -> PcSnapshot {
        let cpu_now = self.col.cpu_times();
        let cpu = match (self.prev_cpu, cpu_now) {
            (Some(a), Some(b)) => cpu_percent(a, b),
            _ => None,
        };
        self.prev_cpu = cpu_now.or(self.prev_cpu);

        let (mut down, mut up) = (0.0, 0.0);
        if let Some((rx, tx)) = self.col.net_bytes() {
            if let Some((rx0, tx0, t0)) = self.prev_net {
                let secs = (now_ms.saturating_sub(t0) as f64 / 1000.0).max(0.001);
                down = (rx.saturating_sub(rx0) as f64 / secs).round_ties_even();
                up = (tx.saturating_sub(tx0) as f64 / secs).round_ties_even();
            }
            self.prev_net = Some((rx, tx, now_ms));
        }

        if self.n.is_multiple_of(5) {
            self.temp = self.col.cpu_temp();
        }
        if self.n.is_multiple_of(15) {
            self.disks = self.col.disks();
        }
        self.n = self.n.wrapping_add(1);

        let mem = self.col.memory().unwrap_or_default();
        PcSnapshot {
            cpu,
            cpu_temp: self.temp,
            cpu_name: self.col.cpu_name().to_owned(),
            ram_used_gb: kb_to_gb1(mem.total_kb.saturating_sub(mem.available_kb)),
            ram_total_gb: kb_to_gb1(mem.total_kb),
            gpu: self.gpu.as_ref().and_then(|g| g.snapshot()),
            net_down_bps: down,
            net_up_bps: up,
            disks: self.disks.clone(),
            uptime_sec: self.col.uptime_sec().unwrap_or(0),
            swap_total_gb: kb_to_gb1(mem.swap_total_kb),
            swap_used_gb: kb_to_gb1(mem.swap_total_kb.saturating_sub(mem.swap_free_kb)),
        }
    }
}

/// `pc` konusu yayın noktası + sayaçlar (K7: `/api/debug/topics`).
pub struct Hub {
    pub tx: watch::Sender<Option<Arc<PcSnapshot>>>,
    notify: Notify,
    subscribers: AtomicUsize,
    last_pull_ms: AtomicU64,
    pub runs: AtomicU64,
    pub collect_us: AtomicU64,
    /// Sessiz kipte olayı tetikleyen alan sayaçları (`stream::CHANGE_FIELDS` sırası).
    pub changes: [AtomicU64; 7],
    epoch: Instant,
}

/// Akış aboneliği; düşünce sayaç azalır.
pub struct Subscription {
    hub: Arc<Hub>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.subscribers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Hub {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tx: watch::Sender::new(None),
            notify: Notify::new(),
            subscribers: AtomicUsize::new(0),
            last_pull_ms: AtomicU64::new(0),
            runs: AtomicU64::new(0),
            collect_us: AtomicU64::new(0),
            changes: Default::default(),
            epoch: Instant::now(),
        })
    }

    /// Süreç başından beri ms (tekdüze saat).
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    #[must_use]
    pub fn subscribers(&self) -> usize {
        self.subscribers.load(Ordering::Relaxed)
    }

    /// `/api/state` çekmesi: 5 sn boyunca örnekleyici uyanık.
    pub fn pull(&self) {
        self.last_pull_ms.store(self.now_ms().max(1), Ordering::Relaxed);
        self.notify.notify_one();
    }

    pub fn subscribe(self: &Arc<Self>) -> Subscription {
        self.subscribers.fetch_add(1, Ordering::Relaxed);
        self.notify.notify_one();
        Subscription { hub: Arc::clone(self) }
    }

    #[must_use]
    pub fn active(&self, now_ms: u64) -> bool {
        let pulled = self.last_pull_ms.load(Ordering::Relaxed);
        self.subscribers() > 0 || (pulled > 0 && now_ms.saturating_sub(pulled) < PULL_LINGER_MS)
    }

    fn record(&self, started: Instant) {
        self.runs.fetch_add(1, Ordering::Relaxed);
        self.collect_us.fetch_add(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    /// Tek örnek al ve yayınla (testler ve döngü ortak yolu).
    pub fn sample_once(&self, s: &mut Sampler) {
        let t0 = Instant::now();
        let snap = s.sample(self.now_ms());
        self.record(t0);
        self.tx.send_replace(Some(Arc::new(snap)));
    }
}

/// Örnekleyici döngüsü (tek iş parçacıklı çalışma zamanında tek görev).
pub async fn run_sampler(hub: Arc<Hub>, mut s: Sampler) {
    loop {
        let woke = hub.notify.notified();
        tokio::pin!(woke);
        woke.as_mut().enable();
        if !hub.active(hub.now_ms()) {
            woke.await;
            s.rebase(hub.now_ms());
            tokio::time::sleep(Duration::from_millis(SAMPLE_MS)).await;
            continue;
        }
        hub.sample_once(&mut s);
        tokio::time::sleep(Duration::from_millis(SAMPLE_MS)).await;
    }
}

#[cfg(test)]
pub mod fake {
    //! Testler için deterministik toplayıcı.
    use crate::collect::{Collector, CpuTimes, Disk, Mem};

    #[derive(Default)]
    pub struct Fake {
        pub cpu: CpuTimes,
        pub mem: Mem,
        pub net: (u64, u64),
        pub calls: u64,
    }

    impl Collector for Fake {
        fn cpu_times(&mut self) -> Option<CpuTimes> {
            self.calls += 1;
            Some(self.cpu)
        }
        fn memory(&mut self) -> Option<Mem> {
            Some(self.mem)
        }
        fn net_bytes(&mut self) -> Option<(u64, u64)> {
            Some(self.net)
        }
        fn cpu_temp(&mut self) -> Option<f64> {
            Some(45.0)
        }
        fn disks(&mut self) -> Vec<Disk> {
            vec![Disk { name: "/".into(), used_gb: 1.0, total_gb: 2.0 }]
        }
        fn uptime_sec(&mut self) -> Option<u64> {
            Some(100)
        }
        fn cpu_name(&self) -> &str {
            "Fake CPU"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;
    use crate::collect::Mem;

    #[test]
    fn tp_w2_s1_snapshot_has_swap_and_rates() {
        let mem = Mem { total_kb: 32 << 20, available_kb: 16 << 20, swap_total_kb: 100 << 20, swap_free_kb: 88 << 20 };
        let mut s = Sampler::new(Box::new(Fake { mem, ..Fake::default() }));
        let first = s.sample(0);
        assert_eq!(first.cpu, None);
        assert!((first.swap_total_gb - 100.0).abs() < 1e-9);
        assert!((first.swap_used_gb - 12.0).abs() < 1e-9);
        assert!((first.ram_used_gb - 16.0).abs() < 1e-9);
        let v = serde_json::to_value(&first).unwrap();
        for k in [
            "cpu",
            "cpuTemp",
            "cpuName",
            "ramUsedGb",
            "ramTotalGb",
            "gpu",
            "netDownBps",
            "netUpBps",
            "disks",
            "uptimeSec",
            "swapTotalGb",
            "swapUsedGb",
        ] {
            assert!(v.get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn tp_w2_s2_pull_lingers_then_sleeps() {
        let hub = Hub::new();
        assert!(!hub.active(10));
        hub.pull();
        let t = hub.now_ms();
        assert!(hub.active(t + PULL_LINGER_MS - 1));
        assert!(!hub.active(t.max(1) + PULL_LINGER_MS));
        let sub = hub.subscribe();
        assert!(hub.active(t + 60_000));
        drop(sub);
        assert!(!hub.active(t + 60_000));
    }

    #[tokio::test(start_paused = true)]
    async fn tp_w2_s2_sampler_does_not_run_without_subscribers() {
        let hub = Hub::new();
        let task = tokio::spawn(run_sampler(Arc::clone(&hub), Sampler::new(Box::new(Fake::default()))));
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(hub.runs.load(Ordering::Relaxed), 0);
        let sub = hub.subscribe();
        tokio::time::sleep(Duration::from_millis(10_500)).await;
        let runs = hub.runs.load(Ordering::Relaxed);
        assert!((9..=11).contains(&runs), "runs={runs}");
        drop(sub);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let after = hub.runs.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(hub.runs.load(Ordering::Relaxed), after, "abone gidince örnekleme durmalı");
        task.abort();
    }
}
