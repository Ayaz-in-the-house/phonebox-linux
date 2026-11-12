#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! W2.5 GPU (`state.pc.gpu`): sahte `nvidia-smi` betiği ile — gerçek sürücüye dokunulmaz.
//! Betik ad sorgusuna "Sahte GPU" yazar; döngü kipinde kendi PID'ini dosyaya yazıp 100 ms'de bir satır basar.

mod common;

use phoned::gpu::{self, Gpu};
use phoned::hub::{Hub, Sampler};
use serde_json::json;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fake_smi(dir: &Path) -> PathBuf {
    let p = dir.join("nvidia-smi");
    let pidf = dir.join("loop.pid");
    let starts = dir.join("starts");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\ncase \"$1\" in --query-gpu=name) echo 'Sahte GPU'; exit 0;; esac\necho x >> '{}'\necho $$ > '{}'\nwhile :; do echo '37, 45, 21.50, [N/A], 1024, 8192, [N/A], 1500'; sleep 0.1; done\n",
            starts.display(),
            pidf.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn alive(pid: i32) -> bool {
    // Zombi de "yok" sayılır (ebeveyn toplamadıysa bile süreç ölü).
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.rsplit_once(')').is_some_and(|(_, r)| r.trim_start().starts_with('Z')))
}

async fn until(max: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < max {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

/// TP-W25-G1 — ne: `nvidia-smi` CSV satırı → 0.6.9 `GpuInfo` şekli · beklenen: `[N/A]` → null, bellek MiB/1024, eksik alanlı satır
/// yok sayılır · neden: kullanıcının panelinde GPU stat + GPU grafiği var; 0.6.9 aynı eşlemeyi yapar (`SystemMonitor.cs:96`).
#[test]
fn tp_w25_g1_parse_line() {
    let v = gpu::parse_line("Sahte GPU", "37, 45, 21.50, [N/A], 1024, 8192, [N/A], 1500").unwrap();
    assert_eq!(
        v,
        json!({"name": "Sahte GPU", "util": 37.0, "temp": 45.0, "power": 21.5, "powerLimit": null,
               "vramUsed": 1.0, "vramTotal": 8.0, "fan": null, "clock": 1500.0})
    );
    assert_eq!(gpu::parse_line("x", "1, 2, 3"), None, "eksik alan");
    assert_eq!(gpu::parse_line("x", ""), None);
    let mut errs = Vec::new();
    common::compare(&common::fixture("api_state")["pc"]["gpu"], &v, "gpu", &common::Rules::default(), &mut errs);
    assert!(errs.is_empty(), "{errs:?}");
}

/// TP-W25-G2 — ne: abonelik kapısı · beklenen: örnekleyici uyurken çocuk süreç YOK; örnek akmaya başlayınca tek çocuk başlar ve
/// `state.pc.gpu` dolar; örnekler durunca (kimse bakmıyor) çocuk sonlanır, değer null olur · neden: HP4 (abone yokken monitör uyur),
/// 0.6.9 nvidia-smi'yi sürekli koşturuyordu (cgroup'ta görülen çocuk).
#[tokio::test]
async fn tp_w25_g2_child_only_while_sampling() {
    let d = common::temp_dir();
    let smi = fake_smi(&d.0);
    let g = Arc::new(Gpu::new(gpu::Settings { program: smi, idle_stop: Duration::from_millis(600), retry: Duration::from_secs(30) }));
    let hub = Hub::new();
    let task = tokio::spawn(gpu::run(Arc::clone(&g), hub.tx.subscribe()));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!d.0.join("loop.pid").exists(), "örnek yokken çocuk başlamamalı");
    assert_eq!(g.snapshot(), None);

    let mut s = Sampler::new(Box::new(common::FakeCol(0))).with_gpu(Arc::clone(&g));
    hub.sample_once(&mut s);
    assert!(until(Duration::from_secs(3), || g.snapshot().is_some()).await, "GPU değeri gelmedi");
    let pid: i32 = std::fs::read_to_string(d.0.join("loop.pid")).unwrap().trim().parse().unwrap();
    assert!(alive(pid));
    // Örnekleyici akarken çocuk yaşar; örnek değeri snapshot'a girer.
    for _ in 0..4 {
        hub.sample_once(&mut s);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(alive(pid), "örnek akarken çocuk kapanmamalı");
    let snap = s.sample(0);
    assert_eq!(snap.gpu.as_ref().unwrap()["name"], "Sahte GPU");
    assert_eq!(std::fs::read_to_string(d.0.join("starts")).unwrap().lines().count(), 1, "tek çocuk");

    // Örnekler durdu → idle_stop sonra çocuk ölür, değer temizlenir.
    assert!(until(Duration::from_secs(3), || !alive(pid)).await, "boşta çocuk sonlanmadı");
    assert!(until(Duration::from_secs(1), || g.snapshot().is_none()).await, "bayat GPU değeri kaldı");
    task.abort();
}

/// TP-W25-G3 — ne: `nvidia-smi` yok · beklenen: değer null, ikinci örnekte yeniden fork YOK (kalıcı vazgeçme) · neden: NVIDIA'sız
/// makinede saniyede bir başarısız exec (0.6.9: 10 sn'de bir) olmasın.
#[tokio::test]
async fn tp_w25_g3_missing_program_gives_up() {
    let g = Arc::new(Gpu::new(gpu::Settings {
        program: "/yok/nvidia-smi".into(),
        idle_stop: Duration::from_millis(300),
        retry: Duration::from_millis(50),
    }));
    let hub = Hub::new();
    let task = tokio::spawn(gpu::run(Arc::clone(&g), hub.tx.subscribe()));
    let mut s = Sampler::new(Box::new(common::FakeCol(0))).with_gpu(Arc::clone(&g));
    for _ in 0..5 {
        hub.sample_once(&mut s);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(g.snapshot(), None);
    assert_eq!(g.spawns(), 1, "bulunamayan program bir kez denenir");
    task.abort();
}
