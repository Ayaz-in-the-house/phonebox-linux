//! `procs` konusu: süreç listesi, yalnız **çekmeyle** (TP-EX-05). Arka plan görevi yok: `/api/procs` gelmezse 0 iş.
//!
//! Panel listeyi yalnız sayfa görünürken 2 sn'de bir ister (`main.js:1341`); her istek bir tarama yapar, aynı saniyedeki
//! ikinci istek önbellekten döner. CPU % iki tarama arası tick farkıdır; taban 10 sn'den eskiyse yeniden alınır
//! (uzun uykunun ortalaması "şimdiki CPU" diye gösterilmez — `hub` ile aynı ilke).
//!
//! Okuma `ProcSource` arkasında (K9): Linux = `/proc`, dış süreç YOK. Ayrıştırıcılar saf fonksiyondur.

pub mod http;
pub mod linux;

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Aynı istekte önbellek ömrü (panel 2 sn'de bir sorar; iki widget aynı saniyede sorarsa tek tarama).
pub const CACHE_MS: u64 = 1_000;
/// CPU tabanının en uzun geçerliliği.
pub const BASELINE_MAX_MS: u64 = 10_000;

/// Tek süreç (bir taramadaki hâli).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcInfo {
    pub pid: i32,
    pub ppid: i32,
    /// `/proc/<pid>/stat` `comm` (en çok 15 bayt; 0.6.9 `Process.ProcessName` eşi).
    pub comm: String,
    pub uid: u32,
    /// utime + stime (tick).
    pub ticks: u64,
    /// Açılıştan beri başlangıç (tick) — PID yeniden kullanımına karşı kimlik.
    pub start: u64,
    pub rss_kb: u64,
    /// cgroup yolunun son parçası (`app-gnome-firefox-123.scope`, `herdr-server.service` …); okunamazsa boş.
    pub unit: String,
    /// `cmdline`, NUL → boşluk.
    pub cmdline: String,
}

/// `/proc/<pid>/stat` alanları.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub comm: String,
    pub ppid: i32,
    pub ticks: u64,
    pub start: u64,
    pub rss_pages: u64,
}

/// `/proc/<pid>/stat` → alanlar. `comm` parantez ve boşluk içerebilir: son `)` sınırdır.
#[must_use]
pub fn parse_stat(s: &str) -> Option<Stat> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_owned();
    // `)`'dan sonra: 3 state, 4 ppid, … 14 utime, 15 stime, … 22 starttime, 23 vsize, 24 rss (1-tabanlı man proc numarası).
    let f: Vec<&str> = s.get(close + 1..)?.split_ascii_whitespace().collect();
    let num = |i: usize| f.get(i).and_then(|x| x.parse::<u64>().ok());
    Some(Stat { comm, ppid: f.get(1)?.parse().ok()?, ticks: num(11)?.checked_add(num(12)?)?, start: num(19)?, rss_pages: num(21)? })
}

/// `/proc/<pid>/cgroup` (v2: `0::/yol`) → son parça.
#[must_use]
pub fn parse_unit(cgroup: &str) -> String {
    cgroup.lines().find_map(|l| l.strip_prefix("0::")).and_then(|p| p.trim_end().rsplit('/').next()).unwrap_or("").to_owned()
}

/// İşletim sistemi okuma sınırı.
pub trait ProcSource: Send {
    fn list(&mut self) -> Vec<ProcInfo>;
    /// Saniyedeki tick (`_SC_CLK_TCK`).
    fn clk_tck(&self) -> u64;
    fn cores(&self) -> u64;
    /// Açılış zamanı (unix sn, `/proc/stat` `btime`).
    fn boot_unix(&self) -> Option<u64>;
}

/// 0.6.9 `ProcGroup` (camelCase).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcGroup {
    pub name: String,
    pub title: String,
    pub count: usize,
    pub cpu: f64,
    pub mem_mb: f64,
    pub locked: bool,
    pub has_icon: bool,
}

/// 0.6.9 `ProcInstance`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcInstance {
    pub pid: i32,
    pub cpu: f64,
    pub mem_mb: f64,
    pub started_at: Option<i64>,
    pub session: Option<String>,
    pub window: Option<String>,
    pub args: Option<String>,
    pub locked: bool,
}

/// 0.6.9 `ProcDetail`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcDetail {
    pub name: String,
    pub title: String,
    pub locked: bool,
    pub instances: Vec<ProcInstance>,
}

/// Bir taramanın sonucu.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub procs: Vec<ProcInfo>,
    /// PID → CPU % (bir çekirdeğin değil, makinenin yüzdesi — 0.6.9 `/ _cores`).
    pub cpu: HashMap<i32, f64>,
    pub boot_unix: Option<u64>,
    pub clk_tck: u64,
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round_ties_even() / 10.0
}

fn mem_mb(rss_kb: u64) -> f64 {
    (rss_kb as f64 / 1024.0).round_ties_even()
}

/// Ada göre grupla, cpu ↓ sonra bellek ↓ sırala. `locked(p)` = bu süreç kapatılamaz.
pub fn group(scan: &Scan, locked: impl Fn(&ProcInfo) -> bool) -> Vec<ProcGroup> {
    let mut acc: HashMap<&str, (usize, f64, u64, bool)> = HashMap::new();
    for p in &scan.procs {
        let e = acc.entry(p.comm.as_str()).or_insert((0, 0.0, 0, false));
        e.0 += 1;
        e.1 += scan.cpu.get(&p.pid).copied().unwrap_or(0.0);
        e.2 = e.2.saturating_add(p.rss_kb);
        e.3 = e.3 || locked(p);
    }
    let mut out: Vec<ProcGroup> = acc
        .into_iter()
        .map(|(name, (count, cpu, rss, lk))| ProcGroup {
            name: name.to_owned(),
            title: name.to_owned(),
            count,
            cpu: round1(cpu),
            mem_mb: mem_mb(rss),
            locked: lk,
            has_icon: false,
        })
        .collect();
    out.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem_mb.total_cmp(&a.mem_mb)).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Bir adın örnekleri (0.6.9 `Detail`).
pub fn detail(scan: &Scan, name: &str, locked: impl Fn(&ProcInfo) -> bool) -> ProcDetail {
    let mut instances: Vec<ProcInstance> = scan
        .procs
        .iter()
        .filter(|p| p.comm == name)
        .map(|p| ProcInstance {
            pid: p.pid,
            cpu: round1(scan.cpu.get(&p.pid).copied().unwrap_or(0.0)),
            mem_mb: mem_mb(p.rss_kb),
            started_at: scan.boot_unix.and_then(|b| {
                let ms = b.saturating_mul(1000).saturating_add(p.start.saturating_mul(1000) / scan.clk_tck.max(1));
                i64::try_from(ms).ok()
            }),
            session: None,
            window: None,
            args: short_args(&p.cmdline),
            locked: locked(p),
        })
        .collect();
    instances.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem_mb.total_cmp(&a.mem_mb)));
    let any_locked = instances.iter().any(|i| i.locked);
    ProcDetail { name: name.to_owned(), title: name.to_owned(), locked: any_locked, instances }
}

/// Komut satırı bağımsız değişkenleri (yürütülebilir dosya hariç), 140 karakterle sınırlı (0.6.9 `UnixProc.Args`).
#[must_use]
pub fn short_args(cmdline: &str) -> Option<String> {
    let rest = cmdline.trim().split_once(' ').map(|(_, r)| r.trim())?;
    if rest.is_empty() {
        return None;
    }
    if rest.chars().count() > 140 { Some(rest.chars().take(137).chain(std::iter::once('…')).collect()) } else { Some(rest.to_owned()) }
}

/// PID → (start, ticks) ve alındığı an.
type Baseline = (Instant, HashMap<i32, (u64, u64)>);

struct Inner {
    src: Box<dyn ProcSource>,
    /// PID → (start, ticks) ve alındığı an.
    base: Option<Baseline>,
    cache: Option<(Instant, std::sync::Arc<Scan>)>,
}

/// Konu durumu + K7 sayaçları.
pub struct Procs {
    inner: Mutex<Inner>,
    pub runs: AtomicU64,
    pub collect_us: AtomicU64,
    pub cached_hits: AtomicU64,
    last_pull: Mutex<Option<Instant>>,
}

impl Procs {
    #[must_use]
    pub fn new(src: Box<dyn ProcSource>) -> Self {
        Self {
            inner: Mutex::new(Inner { src, base: None, cache: None }),
            runs: AtomicU64::new(0),
            collect_us: AtomicU64::new(0),
            cached_hits: AtomicU64::new(0),
            last_pull: Mutex::new(None),
        }
    }

    /// Son çekmeden bu yana geçen ms (hiç çekilmediyse `None`).
    pub fn last_pull_age_ms(&self) -> Option<u64> {
        let g = self.last_pull.lock().ok()?;
        g.map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    /// Çekme: önbellek tazeyse onu, değilse yeni tarama (eşzamanlı istekler kilitte sıralanır, tek tarama).
    pub fn pull(&self) -> std::sync::Arc<Scan> {
        if let Ok(mut g) = self.last_pull.lock() {
            *g = Some(Instant::now());
        }
        self.scan(false)
    }

    /// Kapatma kararı için her zaman taze tarama (önbellekteki PID ölmüş olabilir).
    pub fn fresh(&self) -> std::sync::Arc<Scan> {
        self.scan(true)
    }

    fn scan(&self, force: bool) -> std::sync::Arc<Scan> {
        let Ok(mut g) = self.inner.lock() else {
            return std::sync::Arc::new(Scan::default());
        };
        if !force
            && let Some((at, s)) = &g.cache
            && at.elapsed().as_millis() < u128::from(CACHE_MS)
        {
            self.cached_hits.fetch_add(1, Ordering::Relaxed);
            return std::sync::Arc::clone(s);
        }
        let t0 = Instant::now();
        let procs = g.src.list();
        let (tck, cores) = (g.src.clk_tck().max(1), g.src.cores().max(1));
        let mut cpu = HashMap::with_capacity(procs.len());
        let base = g.base.take().filter(|(at, _)| at.elapsed().as_millis() < u128::from(BASELINE_MAX_MS));
        if let Some((at, prev)) = &base {
            let secs = at.elapsed().as_secs_f64().max(0.001);
            for p in &procs {
                if let Some((start, ticks)) = prev.get(&p.pid)
                    && *start == p.start
                {
                    let busy = p.ticks.saturating_sub(*ticks) as f64 / tck as f64;
                    cpu.insert(p.pid, (busy / secs / cores as f64 * 100.0).max(0.0));
                }
            }
        }
        g.base = Some((Instant::now(), procs.iter().map(|p| (p.pid, (p.start, p.ticks))).collect()));
        let scan = std::sync::Arc::new(Scan { procs, cpu, boot_unix: g.src.boot_unix(), clk_tck: tck });
        g.cache = Some((Instant::now(), std::sync::Arc::clone(&scan)));
        self.runs.fetch_add(1, Ordering::Relaxed);
        self.collect_us.fetch_add(u64::try_from(t0.elapsed().as_micros()).unwrap_or(u64::MAX), Ordering::Relaxed);
        scan
    }

    /// İlk istekte taban yoksa CPU sütunu 0 olur; panel 2 sn sonra yeniden ister (0.6.9 da ilk anda 700 ms bekler).
    #[must_use]
    pub fn has_baseline(&self) -> bool {
        self.inner.lock().is_ok_and(|g| g.base.is_some())
    }
}

#[doc(hidden)]
pub mod fake {
    //! (Test düzeneği; entegrasyon testleri de kullanır, üretimde bağlanmaz.)
    //! Testler için deterministik süreç kaynağı.
    use super::{ProcInfo, ProcSource};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct FakeProcs(pub Arc<Mutex<Vec<ProcInfo>>>, pub Arc<Mutex<u64>>);

    impl ProcSource for FakeProcs {
        fn list(&mut self) -> Vec<ProcInfo> {
            if let Ok(mut n) = self.1.lock() {
                *n += 1;
            }
            self.0.lock().map(|v| v.clone()).unwrap_or_default()
        }
        fn clk_tck(&self) -> u64 {
            100
        }
        fn cores(&self) -> u64 {
            4
        }
        fn boot_unix(&self) -> Option<u64> {
            Some(1_000_000)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "1234 (Web (Co) x) S 77 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 30 0 98765 123456789 2048 18446744073709551615";

    #[test]
    fn tp_w24_p1_parse_stat_paren_comm() {
        let s = parse_stat(STAT).unwrap();
        assert_eq!(s, Stat { comm: "Web (Co) x".into(), ppid: 77, ticks: 300, start: 98765, rss_pages: 2048 });
        assert!(parse_stat("1 (x) S").is_none());
        assert!(parse_stat("garbage").is_none());
        assert!(parse_stat("1 (x) S notanumber 1 1 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 5 1 1").is_none());
    }

    #[test]
    fn tp_w24_p1_parse_unit() {
        assert_eq!(
            parse_unit("0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-firefox-42.scope\n"),
            "app-gnome-firefox-42.scope"
        );
        assert_eq!(parse_unit("0::/\n"), "");
        assert_eq!(parse_unit("12:cpu:/x\n"), "");
    }

    fn p(pid: i32, comm: &str, rss_kb: u64) -> ProcInfo {
        ProcInfo { pid, comm: comm.into(), rss_kb, ..ProcInfo::default() }
    }

    #[test]
    fn tp_w24_p2_group_sums_and_sorts() {
        let scan = Scan {
            procs: vec![p(1, "a", 1024), p(2, "a", 2048), p(3, "b", 10 << 20), p(4, "c", 0)],
            cpu: HashMap::from([(1, 1.25), (2, 1.0), (3, 0.5)]),
            boot_unix: None,
            clk_tck: 100,
        };
        let g = group(&scan, |x| x.pid == 2);
        assert_eq!(g.len(), 3);
        assert_eq!(g[0], ProcGroup { name: "a".into(), title: "a".into(), count: 2, cpu: 2.2, mem_mb: 3.0, locked: true, has_icon: false });
        assert_eq!(g[1].name, "b");
        assert!(!g[1].locked);
        assert!((g[1].mem_mb - 10240.0).abs() < 1e-9);
        assert_eq!(g[2].name, "c");
        let v = serde_json::to_value(&g[0]).unwrap();
        for k in ["name", "title", "count", "cpu", "memMb", "locked", "hasIcon"] {
            assert!(v.get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn tp_w24_p2_detail_instances() {
        let mut a = p(7, "sleep", 2048);
        a.cmdline = "sleep 300".into();
        a.start = 500;
        let scan = Scan { procs: vec![a, p(8, "other", 1)], cpu: HashMap::new(), boot_unix: Some(1000), clk_tck: 100 };
        let d = detail(&scan, "sleep", |_| false);
        assert_eq!(d.instances.len(), 1);
        assert_eq!(d.instances[0].args.as_deref(), Some("300"));
        assert_eq!(d.instances[0].started_at, Some(1_005_000));
        assert!(!d.locked);
        assert_eq!(short_args("x"), None);
        assert_eq!(short_args(&format!("x {}", "y".repeat(200))).map(|s| s.chars().count()), Some(138));
    }

    #[test]
    fn tp_ex_05_pull_only_and_cache() {
        let src = fake::FakeProcs::default();
        let calls = std::sync::Arc::clone(&src.1);
        let procs = Procs::new(Box::new(src));
        // Çekme yoksa hiçbir şey koşmaz (arka plan görevi yok).
        assert_eq!(procs.runs.load(Ordering::Relaxed), 0);
        assert_eq!(*calls.lock().unwrap(), 0);
        let a = procs.pull();
        let b = procs.pull();
        assert!(std::sync::Arc::ptr_eq(&a, &b), "aynı saniyedeki ikinci istek önbellekten");
        assert_eq!(procs.runs.load(Ordering::Relaxed), 1);
        assert_eq!(procs.cached_hits.load(Ordering::Relaxed), 1);
        let _ = procs.fresh();
        assert_eq!(procs.runs.load(Ordering::Relaxed), 2, "kapatma kararı taze tarama ister");
        assert!(procs.last_pull_age_ms().is_some());
    }

    #[test]
    fn cpu_from_tick_delta_same_identity_only() {
        let src = fake::FakeProcs::default();
        let list = std::sync::Arc::clone(&src.0);
        *list.lock().unwrap() =
            vec![ProcInfo { pid: 5, start: 1, ticks: 0, ..ProcInfo::default() }, ProcInfo { pid: 6, start: 1, ticks: 0, ..ProcInfo::default() }];
        let procs = Procs::new(Box::new(src));
        let first = procs.fresh();
        assert!(first.cpu.is_empty(), "taban yokken CPU uydurulmaz");
        std::thread::sleep(std::time::Duration::from_millis(50));
        // pid 5: 4 tick ilerledi; pid 6: aynı PID başka süreç (start değişti) → CPU yok.
        *list.lock().unwrap() =
            vec![ProcInfo { pid: 5, start: 1, ticks: 4, ..ProcInfo::default() }, ProcInfo { pid: 6, start: 9, ticks: 400, ..ProcInfo::default() }];
        let second = procs.fresh();
        assert!(second.cpu.get(&5).copied().unwrap_or(0.0) > 0.0);
        assert!(!second.cpu.contains_key(&6));
    }
}
