//! Toplayıcı sınırı (K9): işletim sistemine özgü okuma `Collector` trait'inin arkasında. Linux: /proc + /sys, dış süreç YOK.
//! macOS/Windows sonradan ayrı modül olarak eklenir (`collect::macos`, `collect::windows`), üst katman değişmez.
//!
//! Ayrıştırıcılar saf fonksiyondur (metin → değer), fixture metinle test edilir.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub idle: u64,
    pub total: u64,
}

/// `/proc/meminfo` alanları (kB).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mem {
    pub total_kb: u64,
    pub available_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub name: String,
    pub used_gb: f64,
    pub total_gb: f64,
}

/// İşletim sistemi okuma sınırı. Her yöntem ucuzdur (tek dosya okuması); `disks` en pahalısı, seyrek çağrılır.
pub trait Collector: Send {
    fn cpu_times(&mut self) -> Option<CpuTimes>;
    fn memory(&mut self) -> Option<Mem>;
    /// Fiziksel arabirimlerin toplam (rx, tx) baytı.
    fn net_bytes(&mut self) -> Option<(u64, u64)>;
    fn cpu_temp(&mut self) -> Option<f64>;
    fn disks(&mut self) -> Vec<Disk>;
    fn uptime_sec(&mut self) -> Option<u64>;
    fn cpu_name(&self) -> &str;
}

/// .NET `Math.Round(x, 1)` (çifte yuvarlama: yarıda çifte).
#[must_use]
pub fn round1(x: f64) -> f64 {
    (x * 10.0).round_ties_even() / 10.0
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// `kB` → GB (2³⁰ bayt), 1 ondalık.
#[must_use]
pub fn kb_to_gb1(kb: u64) -> f64 {
    round1(kb as f64 / (1024.0 * 1024.0))
}

/// `/proc/stat` ilk satırı → (idle+iowait, toplam) — 0.6.9 `UnixSystem.CpuTimes`.
#[must_use]
pub fn parse_stat(s: &str) -> Option<CpuTimes> {
    let mut it = s.lines().next()?.split_ascii_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    let f: Vec<u64> = it.take(8).map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let idle = f.get(3)?.checked_add(*f.get(4)?)?;
    Some(CpuTimes { idle, total: f.iter().sum() })
}

/// İki okuma arası CPU yükü %, 1 ondalık, [0, 100].
#[must_use]
pub fn cpu_percent(a: CpuTimes, b: CpuTimes) -> Option<f64> {
    let dt = b.total.checked_sub(a.total)?;
    if dt == 0 {
        return None;
    }
    let busy = dt.saturating_sub(b.idle.saturating_sub(a.idle));
    Some(round1((100.0 * busy as f64 / dt as f64).clamp(0.0, 100.0)))
}

#[must_use]
pub fn parse_meminfo(s: &str) -> Option<Mem> {
    let mut m = Mem::default();
    let mut seen = 0u8;
    for line in s.lines() {
        let Some((key, rest)) = line.split_once(':') else { continue };
        let slot = match key {
            "MemTotal" => &mut m.total_kb,
            "MemAvailable" => &mut m.available_kb,
            "SwapTotal" => &mut m.swap_total_kb,
            "SwapFree" => &mut m.swap_free_kb,
            _ => continue,
        };
        *slot = rest.split_ascii_whitespace().next()?.parse().ok()?;
        seen += 1;
    }
    (seen == 4).then_some(m)
}

/// `/proc/net/dev` → `allow(ad)` arabirimlerinin (rx, tx) toplamı.
#[must_use]
pub fn parse_netdev(s: &str, allow: impl Fn(&str) -> bool) -> Option<(u64, u64)> {
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in s.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        if !allow(name.trim()) {
            continue;
        }
        let f: Vec<&str> = rest.split_ascii_whitespace().collect();
        rx = rx.saturating_add(f.first()?.parse().ok()?);
        tx = tx.saturating_add(f.get(8)?.parse().ok()?);
    }
    Some((rx, tx))
}

/// 0.6.9 `ReadDisks` bağlama noktası süzgeci (gerçek birimler).
#[must_use]
pub fn mount_wanted(mp: &str) -> bool {
    (mp == "/" || mp.starts_with("/home") || mp.starts_with("/mnt/") || mp.starts_with("/media/") || mp.starts_with("/data"))
        && !mp.contains("/wsl")
        && !mp.contains("docker")
        && !mp.starts_with("/mnt/c")
}

/// `/proc/mounts` kaçışları (`\040` boşluk vb.).
#[must_use]
pub fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'\\'
            && let Some(oct) = b.get(i + 1..i + 4).and_then(|d| std::str::from_utf8(d).ok()).and_then(|d| u8::from_str_radix(d, 8).ok())
        {
            out.push(oct);
            i += 4;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `/proc/mounts` → aday bağlama noktaları (aygıt `/` ile başlar = yerel blok aygıtı; ağ/sanal dosya sistemleri dışarıda).
#[must_use]
pub fn parse_mounts(s: &str) -> Vec<String> {
    s.lines()
        .filter_map(|l| {
            let mut it = l.split_ascii_whitespace();
            let dev = it.next()?;
            let mp = unescape_mount(it.next()?);
            (dev.starts_with('/') && mount_wanted(&mp)).then_some(mp)
        })
        .collect()
}

/// (bağlama noktası, toplam bayt, boş bayt) → 0.6.9 disk listesi: > 2 GB, (boyut, boş GB) tekilleştirme, en çok 6.
#[must_use]
pub fn disks_from(stats: &[(String, u64, u64)]) -> Vec<Disk> {
    let mut seen: Vec<(u64, u64)> = Vec::new();
    let mut out = Vec::new();
    for (mp, total, free) in stats {
        if (*total as f64) <= 2.0 * GIB {
            continue;
        }
        let key = (*total, free >> 30);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let name = if mp == "/" { "/".to_owned() } else { mp.trim_end_matches('/').rsplit('/').next().unwrap_or(mp).to_owned() };
        out.push(Disk {
            name,
            used_gb: (total.saturating_sub(*free) as f64 / GIB).round_ties_even(),
            total_gb: (*total as f64 / GIB).round_ties_even(),
        });
        if out.len() == 6 {
            break;
        }
    }
    out
}

/// hwmon tercih sırası (0.6.9 `UnixSystem.CpuTemp`).
pub const HWMON_PREFERRED: [&str; 6] = ["k10temp", "zenpower", "coretemp", "cpu_thermal", "soc_thermal", "acpitz"];

/// (dizin, ad) listesinden tercih edilen sensör dizini.
#[must_use]
pub fn choose_hwmon(sensors: &[(String, String)]) -> Option<&str> {
    HWMON_PREFERRED.iter().find_map(|want| sensors.iter().find(|(_, n)| n == want).map(|(d, _)| d.as_str()))
}

#[must_use]
pub fn parse_cpu_name(cpuinfo: &str) -> String {
    for key in ["model name", "Model", "Hardware"] {
        for line in cpuinfo.lines() {
            if let Some((k, v)) = line.split_once(':')
                && k.trim_end() == key
                && !v.trim().is_empty()
            {
                return v.trim().to_owned();
            }
        }
    }
    "CPU".to_owned()
}

/// Dosyayı yeniden kullanılan ara belleğe okur (çağrı başına ayırma yok). Toplayıcı ve süreç kaynağı (`procs::linux`) paylaşır.
/// Dosya yoksa / okunamazsa `None` (okuma arasında süreç ölmüş olabilir).
pub fn read_into<'a>(buf: &'a mut String, path: &str) -> Option<&'a str> {
    use std::io::Read as _;
    buf.clear();
    std::fs::File::open(path).ok()?.read_to_string(buf).ok()?;
    Some(buf)
}

pub mod linux {
    //! Linux okuyucusu: yalnız /proc ve /sys; dosya başına tek `read`, ara bellek yeniden kullanılır.
    use super::{Collector, CpuTimes, Disk, Mem, choose_hwmon, disks_from, parse_cpu_name, parse_meminfo, parse_mounts, parse_netdev, parse_stat};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    pub struct LinuxCollector {
        buf: String,
        cpu_name: String,
        hwmon: Option<PathBuf>,
        ifaces: Vec<String>,
        ifaces_at: Option<Instant>,
    }

    impl Default for LinuxCollector {
        fn default() -> Self {
            Self::new()
        }
    }

    impl LinuxCollector {
        #[must_use]
        pub fn new() -> Self {
            let cpu_name = fs::read_to_string("/proc/cpuinfo").map(|s| parse_cpu_name(&s)).unwrap_or_else(|_| "CPU".to_owned());
            Self { buf: String::with_capacity(16 * 1024), cpu_name, hwmon: find_hwmon(), ifaces: Vec::new(), ifaces_at: None }
        }

        fn read(&mut self, path: &str) -> Option<&str> {
            super::read_into(&mut self.buf, path)
        }

        /// Fiziksel arabirimler: `type == 1` (Ethernet/Wi-Fi) ve `/sys/devices/virtual/net/<ad>` YOK (veth, docker, br-, virbr, tun hariç).
        fn refresh_ifaces(&mut self) {
            if self.ifaces_at.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
                return;
            }
            self.ifaces_at = Some(Instant::now());
            self.ifaces = fs::read_dir("/sys/class/net")
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter(|n| {
                            let ether = fs::read_to_string(format!("/sys/class/net/{n}/type")).is_ok_and(|t| t.trim() == "1");
                            ether && !Path::new(&format!("/sys/devices/virtual/net/{n}")).exists()
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
    }

    fn find_hwmon() -> Option<PathBuf> {
        let sensors: Vec<(String, String)> = fs::read_dir("/sys/class/hwmon")
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let dir = e.path();
                let name = fs::read_to_string(dir.join("name")).ok()?.trim().to_owned();
                Some((dir.to_string_lossy().into_owned(), name))
            })
            .collect();
        choose_hwmon(&sensors).map(|d| Path::new(d).join("temp1_input"))
    }

    fn statvfs(path: &str) -> Option<(u64, u64)> {
        let c = std::ffi::CString::new(path).ok()?;
        // SAFETY: `libc::statvfs` düz C yapısıdır; sıfır bit deseni geçerli bir başlangıç değeridir.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` NUL ile biten geçerli bir C dizgisi, `st` yazılabilir ve doğru tipte; çağrı yalnız `st`'ye yazar.
        let r = unsafe { libc::statvfs(c.as_ptr(), &raw mut st) };
        if r != 0 {
            return None;
        }
        let fr = st.f_frsize;
        Some((st.f_blocks.saturating_mul(fr), st.f_bfree.saturating_mul(fr)))
    }

    impl Collector for LinuxCollector {
        fn cpu_times(&mut self) -> Option<CpuTimes> {
            parse_stat(self.read("/proc/stat")?)
        }

        fn memory(&mut self) -> Option<Mem> {
            parse_meminfo(self.read("/proc/meminfo")?)
        }

        fn net_bytes(&mut self) -> Option<(u64, u64)> {
            self.refresh_ifaces();
            let ifaces = std::mem::take(&mut self.ifaces);
            let r = self.read("/proc/net/dev").and_then(|s| parse_netdev(s, |n| ifaces.iter().any(|i| i == n)));
            self.ifaces = ifaces;
            r
        }

        fn cpu_temp(&mut self) -> Option<f64> {
            let path = self.hwmon.clone()?;
            let milli: i64 = self.read(path.to_str()?)?.trim().parse().ok()?;
            Some(super::round1(milli as f64 / 1000.0))
        }

        fn disks(&mut self) -> Vec<Disk> {
            let mounts = self.read("/proc/mounts").map(parse_mounts).unwrap_or_default();
            let stats: Vec<(String, u64, u64)> = mounts.into_iter().filter_map(|mp| statvfs(&mp).map(|(t, f)| (mp, t, f))).collect();
            disks_from(&stats)
        }

        fn uptime_sec(&mut self) -> Option<u64> {
            let secs: f64 = self.read("/proc/uptime")?.split_ascii_whitespace().next()?.parse().ok()?;
            Some(secs.max(0.0) as u64)
        }

        fn cpu_name(&self) -> &str {
            &self.cpu_name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8\n";

    #[test]
    fn tp_w2_s4_stat_and_percent() {
        let a = parse_stat(STAT).unwrap();
        assert_eq!(a, CpuTimes { idle: 850, total: 1000 });
        let b = parse_stat("cpu  150 0 100 850 50 0 0 0 0 0\n").unwrap();
        assert_eq!(cpu_percent(a, b), Some(66.7));
        assert_eq!(cpu_percent(a, a), None);
        assert!(parse_stat("intr 1 2\n").is_none());
        assert!(parse_stat("").is_none());
        assert!(parse_stat("cpu x y\n").is_none());
    }

    const MEMINFO: &str = "MemTotal:       32768000 kB\nMemFree:  1 kB\nMemAvailable:   16384000 kB\nSwapCached: 0 kB\nSwapTotal:      104857600 kB\nSwapFree:       92274688 kB\n";

    #[test]
    fn tp_w2_s1_swap_from_meminfo() {
        let m = parse_meminfo(MEMINFO).unwrap();
        assert_eq!(m.swap_total_kb, 104_857_600);
        assert!((kb_to_gb1(m.swap_total_kb) - 100.0).abs() < 1e-9);
        assert!((kb_to_gb1(m.swap_total_kb - m.swap_free_kb) - 12.0).abs() < 1e-9);
        assert!(parse_meminfo("MemTotal: 1 kB\n").is_none());
    }

    #[test]
    fn tp_w2_s4_netdev_filters() {
        let s = "Inter-|   Receive\n face |bytes\n    lo: 999 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0\n  eth0: 100 1 0 0 0 0 0 0 200 1 0 0 0 0 0 0\nwlan0: 10 1 0 0 0 0 0 0 20 1 0 0 0 0 0 0\ndocker0: 5000 0 0 0 0 0 0 0 5000 0 0 0 0 0 0 0\n";
        assert_eq!(parse_netdev(s, |n| n == "eth0" || n == "wlan0"), Some((110, 220)));
        assert_eq!(parse_netdev(s, |_| false), Some((0, 0)));
    }

    #[test]
    fn tp_w2_s4_mounts_and_disks() {
        let s = "/dev/nvme0n1p3 / btrfs rw 0 0\n/dev/nvme0n1p3 /home btrfs rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n/dev/sda1 /mnt/my\\040disk ext4 rw 0 0\n/dev/sdb1 /mnt/c ntfs rw 0 0\nhost:/x /mnt/nfs nfs rw 0 0\n/dev/sdc1 /boot ext4 rw 0 0\n";
        assert_eq!(parse_mounts(s), vec!["/", "/home", "/mnt/my disk"]);
        let g = 1u64 << 30;
        let d = disks_from(&[
            ("/".into(), 900 * g, 400 * g),
            ("/home".into(), 900 * g, 400 * g),
            ("/mnt/my disk".into(), 100 * g, 50 * g),
            ("/mnt/tiny".into(), g, 0),
        ]);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], Disk { name: "/".into(), used_gb: 500.0, total_gb: 900.0 });
        assert_eq!(d[1].name, "my disk");
    }

    #[test]
    fn tp_w2_s4_hwmon_preference() {
        let s = vec![("/h/0".to_owned(), "acpitz".to_owned()), ("/h/1".to_owned(), "nvme".to_owned()), ("/h/2".to_owned(), "coretemp".to_owned())];
        assert_eq!(choose_hwmon(&s), Some("/h/2"));
        assert_eq!(choose_hwmon(&[("/h/1".to_owned(), "nvme".to_owned())]), None);
    }

    #[test]
    fn cpu_name_and_round() {
        assert_eq!(parse_cpu_name("processor: 0\nmodel name\t: AMD Ryzen 9\n"), "AMD Ryzen 9");
        assert_eq!(parse_cpu_name(""), "CPU");
        assert!((round1(0.25) - 0.2).abs() < 1e-9);
        assert!((round1(0.35) - 0.4).abs() < 1e-9);
    }
}
