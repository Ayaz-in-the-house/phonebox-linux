//! Linux süreç kaynağı: yalnız `/proc` okuması, dış süreç yok. Süreç başına 3 küçük dosya (`stat`, `cgroup`, `cmdline`);
//! sahiplik dizinin `uid`'inden (ek dosya yok). Okuma arasında süreç ölebilir → o satır atlanır.

use super::{ProcInfo, ProcSource, parse_stat, parse_unit};
use std::fs;
use std::os::unix::fs::MetadataExt as _;

pub struct LinuxProcs {
    buf: String,
    page_kb: u64,
    clk_tck: u64,
    cores: u64,
}

impl Default for LinuxProcs {
    fn default() -> Self {
        Self::new()
    }
}

fn sysconf(name: libc::c_int, fallback: u64) -> u64 {
    // SAFETY: `sysconf` yan etkisiz bir sorgudur; geçersiz ad -1 döndürür (aşağıda ele alınır).
    let v = unsafe { libc::sysconf(name) };
    u64::try_from(v).ok().filter(|v| *v > 0).unwrap_or(fallback)
}

impl LinuxProcs {
    #[must_use]
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| u64::try_from(n.get()).unwrap_or(1));
        Self { buf: String::with_capacity(4096), page_kb: sysconf(libc::_SC_PAGESIZE, 4096) / 1024, clk_tck: sysconf(libc::_SC_CLK_TCK, 100), cores }
    }

    fn read(&mut self, path: &str) -> Option<&str> {
        crate::collect::read_into(&mut self.buf, path)
    }

    fn one(&mut self, pid: i32) -> Option<ProcInfo> {
        let uid = fs::metadata(format!("/proc/{pid}")).ok()?.uid();
        let st = parse_stat(self.read(&format!("/proc/{pid}/stat"))?)?;
        let unit = self.read(&format!("/proc/{pid}/cgroup")).map(parse_unit).unwrap_or_default();
        let cmdline = self.read(&format!("/proc/{pid}/cmdline")).map(|s| s.replace('\0', " ").trim().to_owned()).unwrap_or_default();
        Some(ProcInfo {
            pid,
            ppid: st.ppid,
            comm: st.comm,
            uid,
            ticks: st.ticks,
            start: st.start,
            rss_kb: st.rss_pages.saturating_mul(self.page_kb),
            unit,
            cmdline,
        })
    }
}

impl ProcSource for LinuxProcs {
    fn list(&mut self) -> Vec<ProcInfo> {
        let pids: Vec<i32> = fs::read_dir("/proc")
            .map(|rd| rd.filter_map(Result::ok).filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok()).collect())
            .unwrap_or_default();
        // Çekirdek iş parçacıkları (ppid 2 / pid 2) panelde anlamsız ve kapatılamaz: 0.6.9'da da RSS 0 ile görünür, burada atlanır.
        pids.into_iter().filter_map(|pid| self.one(pid)).filter(|p| p.pid != 2 && p.ppid != 2).collect()
    }

    fn clk_tck(&self) -> u64 {
        self.clk_tck
    }

    fn cores(&self) -> u64 {
        self.cores
    }

    fn boot_unix(&self) -> Option<u64> {
        let s = fs::read_to_string("/proc/stat").ok()?;
        s.lines().find_map(|l| l.strip_prefix("btime ")).and_then(|v| v.trim().parse().ok())
    }
}

/// Tek süreci taze oku (kapatmadan hemen önce kimlik doğrulaması için).
#[must_use]
pub fn read_one(pid: i32) -> Option<ProcInfo> {
    LinuxProcs::new().one(pid)
}
