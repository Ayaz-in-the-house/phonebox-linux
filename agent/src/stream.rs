//! `/api/stream` temposu: ne zaman tam `state`, ne zaman `:` kalp atışı, ne zaman hiçbir şey.
//!
//! - **Canlı** (oturum açık, gece modu değil — biri bakıyor): her örnekte tam `state` (K3: 1 Hz, grafik örneği olaydan gelir).
//! - **Sessiz** (oturum kilitli ya da gece modu): yalnız nicelenmiş durum değişince tam `state` (alan farkı DEĞİL — 0.6.9
//!   `applyState` durumu tamamen değiştirir) ve en çok `QUIET_STATE_MS`'de bir (ADR-11: etkin makinede eşikler her saniye
//!   aşılır); arada en çok `HEARTBEAT_MS`'de bir `:\n\n` (APK 8 sn sessizliği ölü sayar). Tavan içindeki değişim kaybolmaz.
//! - İstemci yoksa akış yoktur; örnekleyici de uyur (`hub`).

use crate::hub::PcSnapshot;

/// Sessiz kipte kalp atışı aralığı: 60 sn'de 15 satır (TP-W2-05: 12–15, aralık ≤ 5 sn), ≈ 0,75 B/sn.
pub const HEARTBEAT_MS: u64 = 4_000;
/// Sessiz kipte iki tam `state` arası en az (ADR-11): 60 sn'de en çok 2.
pub const QUIET_STATE_MS: u64 = 30_000;
/// Kalp atışı baytları (SSE yorum satırı; okuyucu yok sayar ama okuma zaman aşımını sıfırlar).
pub const HEARTBEAT: &[u8] = b":\n\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Quiet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Out {
    State,
    Heartbeat,
    Nothing,
}

/// Ekranda görünen çözünürlükte nicelenmiş görüntü (EXPERIENCE-ARCHITECTURE aşama 2).
/// `Quantized::changed_fields` bit adları.
pub const CHANGE_FIELDS: [&str; 7] = ["cpu", "cpuTemp", "ramUsedGb", "swapUsedGb", "netDownBps", "netUpBps", "disks"];

#[derive(Debug, Clone, PartialEq)]
pub struct Quantized {
    cpu: Option<i64>,
    temp: Option<i64>,
    ram_tenths: i64,
    swap_tenths: i64,
    net_down: f64,
    net_up: f64,
    disks: Vec<(String, i64)>,
}

fn q(x: f64, scale: f64) -> i64 {
    (x * scale).round_ties_even() as i64
}

impl Quantized {
    #[must_use]
    pub fn of(p: &PcSnapshot) -> Self {
        Self {
            cpu: p.cpu.map(|c| q(c, 1.0)),
            temp: p.cpu_temp.map(|t| q(t, 1.0)),
            ram_tenths: q(p.ram_used_gb, 10.0),
            swap_tenths: q(p.swap_used_gb, 10.0),
            net_down: p.net_down_bps,
            net_up: p.net_up_bps,
            disks: p.disks.iter().map(|d| (d.name.clone(), q(d.used_gb, 1.0))).collect(),
        }
    }

    /// Ağ: %5'ten büyük VE 1 KB/sn'den büyük değişim (boşta gürültü olay üretmesin).
    fn net_moved(a: f64, b: f64) -> bool {
        (a - b).abs() > (0.05 * a.max(b)).max(1024.0)
    }

    /// `self` (son gönderilen) ile `cur` görünür biçimde farklı mı. CPU/sıcaklık: 1'den FAZLA birim (eşik altı titreme olay değil).
    #[must_use]
    pub fn differs(&self, cur: &Self) -> bool {
        self.changed_fields(cur) != 0
    }

    /// Hangi alanlar değişti (bit `i` = `CHANGE_FIELDS[i]`) — K7 izlenebilirlik: sessiz kipte olayı kim tetikliyor.
    #[must_use]
    pub fn changed_fields(&self, cur: &Self) -> u8 {
        let step = |a: Option<i64>, b: Option<i64>| match (a, b) {
            (Some(x), Some(y)) => (x - y).abs() > 1,
            (None, None) => false,
            _ => true,
        };
        let bits = [
            step(self.cpu, cur.cpu),
            step(self.temp, cur.temp),
            self.ram_tenths != cur.ram_tenths,
            self.swap_tenths != cur.swap_tenths,
            Self::net_moved(self.net_down, cur.net_down),
            Self::net_moved(self.net_up, cur.net_up),
            self.disks != cur.disks,
        ];
        bits.iter().enumerate().fold(0u8, |m, (i, b)| if *b { m | (1 << i) } else { m })
    }
}

/// Bağlantı başına tempo durumu. Saat dışarıdan verilir (test: sanal saat).
#[derive(Debug, Default)]
pub struct Pacer {
    /// Bağlantının ilk tiki (ilk `state` en geç `born + HEARTBEAT_MS`'de, veri olmasa da).
    born_ms: Option<u64>,
    last_write_ms: Option<u64>,
    last_state_ms: Option<u64>,
    last_sent: Option<Quantized>,
    /// Son yazılan `state`'i doğuran alanlar (bit `i` = `CHANGE_FIELDS[i]`; 0 = değişimsiz `state`, ör. canlı kip).
    pub last_changes: u8,
    /// Son `state`'ten beri biriken değişen alanlar (sessiz kipte tavan dolana kadar bekler, kaybolmaz).
    pending: u8,
    /// Son `state`'ten beri görünür değişim oldu mu.
    dirty: bool,
}

impl Pacer {
    /// Bir örnek (ya da örnekleyici sessiz kaldıysa zaman aşımı → `snap = None`) geldiğinde ne yazılacak.
    ///
    /// Sessiz kip (ADR-11): değişim en çok `QUIET_STATE_MS`'de bir tam `state` doğurur; arada `HEARTBEAT_MS`'de bir `:`.
    /// Canlı kip her örnekte `state` — kilit açılınca ilk örnek hemen tam `state` olur.
    pub fn on_tick(&mut self, now_ms: u64, mode: Mode, snap: Option<&PcSnapshot>) -> Out {
        let cur = snap.map(Quantized::of);
        let born = *self.born_ms.get_or_insert(now_ms);
        // İlk gerçek örnek (bağlanınca örnekleyici henüz uyanmamışsa) tavanı beklemez: istemci ≤ 1 sn'de dolu `state` görür.
        let first_data = self.last_sent.is_none() && cur.is_some();
        if let (Some(a), Some(b)) = (&self.last_sent, &cur) {
            let bits = a.changed_fields(b);
            self.pending |= bits;
            self.dirty |= bits != 0;
        }
        let since = |t: Option<u64>| t.map(|t| now_ms.saturating_sub(t));
        let cap_open = since(self.last_state_ms).is_none_or(|d| d >= QUIET_STATE_MS);
        let out = if self.last_state_ms.is_none() {
            // Bağlanınca örnekleyici uyuyor olabilir: boş + dolu iki `state` yerine ilk örneği bekle.
            if cur.is_some() || now_ms.saturating_sub(born) >= HEARTBEAT_MS { Out::State } else { Out::Nothing }
        } else if mode == Mode::Live || first_data || (self.dirty && cap_open) {
            Out::State
        } else if since(self.last_write_ms).is_some_and(|d| d >= HEARTBEAT_MS) {
            Out::Heartbeat
        } else {
            Out::Nothing
        };
        match out {
            Out::State => {
                self.last_write_ms = Some(now_ms);
                self.last_state_ms = Some(now_ms);
                self.dirty = false;
                self.last_changes = std::mem::take(&mut self.pending);
                if cur.is_some() {
                    self.last_sent = cur;
                }
            }
            Out::Heartbeat => self.last_write_ms = Some(now_ms),
            Out::Nothing => {}
        }
        out
    }
}

/// SSE çerçevesi.
#[must_use]
pub fn frame(event: &str, data: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(event.len() + data.len() + 16);
    v.extend_from_slice(b"event: ");
    v.extend_from_slice(event.as_bytes());
    v.extend_from_slice(b"\ndata: ");
    v.extend_from_slice(data.as_bytes());
    v.extend_from_slice(b"\n\n");
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::Disk;

    fn snap(cpu: f64) -> PcSnapshot {
        PcSnapshot {
            cpu: Some(cpu),
            cpu_temp: Some(50.0),
            cpu_name: "x".into(),
            ram_used_gb: 10.0,
            ram_total_gb: 32.0,
            gpu: None,
            net_down_bps: 500.0,
            net_up_bps: 200.0,
            disks: vec![Disk { name: "/".into(), used_gb: 100.0, total_gb: 900.0 }],
            uptime_sec: 1,
            swap_total_gb: 100.0,
            swap_used_gb: 12.0,
        }
    }

    /// 60 sn, saniyede bir örnek; çıktıları say.
    fn run(mode: Mode, cpu_at: impl Fn(u64) -> f64) -> Vec<(u64, Out)> {
        let mut p = Pacer::default();
        (0..=60).map(|s| (s * 1000, p.on_tick(s * 1000, mode, Some(&snap(cpu_at(s)))))).collect()
    }

    #[test]
    fn tp_w2_05_quiet_unchanged_only_heartbeats() {
        let out = run(Mode::Quiet, |_| 10.0);
        let states: Vec<_> = out.iter().filter(|(_, o)| *o == Out::State).collect();
        assert_eq!(states.len(), 1, "yalnız ilk anlık görüntü");
        let beats: Vec<u64> = out.iter().filter(|(_, o)| *o == Out::Heartbeat).map(|(t, _)| *t).collect();
        assert!((12..=15).contains(&beats.len()), "kalp atışı {}", beats.len());
        let mut last = 0;
        for t in &beats {
            assert!(t - last <= HEARTBEAT_MS, "aralık {}", t - last);
            last = *t;
        }
    }

    #[test]
    fn tp_w2_06_quiet_change_emits_one_state() {
        let out = run(Mode::Quiet, |s| if s >= 30 { 25.0 } else { 10.0 });
        let states: Vec<u64> = out.iter().filter(|(_, o)| *o == Out::State).map(|(t, _)| *t).collect();
        assert_eq!(states, vec![0, 30_000]);
        let jitter = run(Mode::Quiet, |s| if s % 2 == 0 { 10.0 } else { 11.0 });
        assert_eq!(jitter.iter().filter(|(_, o)| *o == Out::State).count(), 1, "1 puanlık titreme olay değil");
    }

    fn states_before(out: &[(u64, Out)], end_ms: u64) -> Vec<u64> {
        out.iter().filter(|(t, o)| *o == Out::State && *t < end_ms).map(|(t, _)| *t).collect()
    }

    /// ADR-11: etkin makinede her saniye eşik aşılsa bile 60 sn'de en çok 2 `state`, arada `:` ≤ 4 sn.
    #[test]
    fn tp_w2_05_quiet_busy_capped() {
        let out = run(Mode::Quiet, |s| if s.is_multiple_of(2) { 10.0 } else { 40.0 });
        assert_eq!(states_before(&out, 60_000), vec![0, QUIET_STATE_MS]);
        let writes: Vec<u64> = out.iter().filter(|(_, o)| *o != Out::Nothing).map(|(t, _)| *t).collect();
        let beats = out.iter().filter(|(t, o)| *o == Out::Heartbeat && *t < 60_000).count();
        assert!((12..=15).contains(&beats), "kalp atışı {beats}");
        assert!(writes.windows(2).all(|w| w[1] - w[0] <= HEARTBEAT_MS), "yazma boşluğu > 4 sn");
    }

    /// ADR-11 / TP-W2-06: tavan içindeki değişim kaybolmaz — en geç 30 sn içinde bir `state` taşır.
    #[test]
    fn tp_w2_06_quiet_change_delivered_within_cap() {
        let out = run(Mode::Quiet, |s| if s >= 10 { 25.0 } else { 10.0 });
        assert_eq!(states_before(&out, 61_000), vec![0, QUIET_STATE_MS]);
    }

    /// Kilit açılınca (sessiz → canlı) ilk örnekte tam `state`, sonra yine 1 Hz.
    #[test]
    fn tp_w2_05_unlock_immediate_state() {
        let mut p = Pacer::default();
        let busy = |s: u64| snap(if s.is_multiple_of(2) { 10.0 } else { 40.0 });
        for s in 0..=20 {
            p.on_tick(s * 1000, Mode::Quiet, Some(&busy(s)));
        }
        for s in 21..=25 {
            assert_eq!(p.on_tick(s * 1000, Mode::Live, Some(&busy(s))), Out::State, "canlı {s}");
        }
        // Tekrar kilitlenince tavan son `state`'ten sayılır (25 sn) → 55 sn'den önce yok.
        let quiet: Vec<u64> = (26..=60).filter(|s| p.on_tick(s * 1000, Mode::Quiet, Some(&busy(*s))) == Out::State).collect();
        assert_eq!(quiet, vec![55]);
    }

    #[test]
    fn tp_w2_13_live_state_every_sample() {
        let out = run(Mode::Live, |_| 10.0);
        let states: Vec<u64> = out.iter().filter(|(_, o)| *o == Out::State).map(|(t, _)| *t).collect();
        assert_eq!(states.len(), 61);
        assert!(states.windows(2).all(|w| w[1] - w[0] <= 1_500));
    }

    #[test]
    fn tp_w2_06a_quantization_thresholds() {
        let a = Quantized::of(&snap(10.0));
        assert!(!a.differs(&Quantized::of(&snap(11.0))));
        assert!(a.differs(&Quantized::of(&snap(12.0))));
        let mut t = snap(10.0);
        t.cpu_temp = Some(51.0);
        assert!(!a.differs(&Quantized::of(&t)));
        t.cpu_temp = Some(52.0);
        assert!(a.differs(&Quantized::of(&t)));
        let mut r = snap(10.0);
        r.ram_used_gb = 10.1;
        assert!(a.differs(&Quantized::of(&r)));
        let mut s = snap(10.0);
        s.swap_used_gb = 12.1;
        assert!(a.differs(&Quantized::of(&s)));
        let mut n = snap(10.0);
        n.net_down_bps = 1000.0;
        assert!(!a.differs(&Quantized::of(&n)), "taban 1 KB/sn altında gürültü");
        n.net_down_bps = 100_000.0;
        assert!(a.differs(&Quantized::of(&n)));
        let mut u = snap(10.0);
        u.uptime_sec = 999;
        assert!(!a.differs(&Quantized::of(&u)), "uptime sayılmaz");
        let mut both = snap(30.0);
        both.swap_used_gb = 13.0;
        assert_eq!(a.changed_fields(&Quantized::of(&both)), 0b1001, "cpu + swap bitleri");
    }

    /// Örnekleyici hiç örnek vermezse ilk `state` (boş) en geç `HEARTBEAT_MS`'de gider, sonra kalp atışı sürer.
    #[test]
    fn quiet_without_samples_keeps_heartbeat() {
        let mut p = Pacer::default();
        assert_eq!(p.on_tick(0, Mode::Quiet, None), Out::Nothing);
        assert_eq!(p.on_tick(1_500, Mode::Quiet, None), Out::Nothing);
        assert_eq!(p.on_tick(4_000, Mode::Quiet, None), Out::State);
        assert_eq!(p.on_tick(8_000, Mode::Quiet, None), Out::Heartbeat);
    }

    /// İlk istemci: örnekleyici uyurken bağlanırsa ilk `state` ilk gerçek örneği bekler (boş + dolu iki `state` değil, tek).
    #[test]
    fn tp_w2_05_first_client_one_full_state() {
        for mode in [Mode::Quiet, Mode::Live] {
            let mut p = Pacer::default();
            assert_eq!(p.on_tick(0, mode, None), Out::Nothing, "{mode:?}: boş state yok");
            assert_eq!(p.on_tick(1_000, mode, Some(&snap(10.0))), Out::State, "{mode:?}: ilk örnek ≤ 1 sn");
        }
    }

    /// Örnekleyici geç kaldıysa (boş `state` gitti) ilk gerçek örnek tavanı beklemez.
    #[test]
    fn tp_w2_05_first_sample_bypasses_cap() {
        let mut p = Pacer::default();
        assert_eq!(p.on_tick(0, Mode::Quiet, None), Out::Nothing);
        assert_eq!(p.on_tick(4_000, Mode::Quiet, None), Out::State);
        assert_eq!(p.on_tick(5_000, Mode::Quiet, Some(&snap(10.0))), Out::State);
        assert_eq!(p.on_tick(6_000, Mode::Quiet, Some(&snap(40.0))), Out::Nothing, "sonrası tavanda");
        assert_eq!(p.last_changes, 0, "değişim bekliyor, sayılmadı");
        assert_eq!(p.on_tick(34_000, Mode::Quiet, Some(&snap(10.0))), Out::Heartbeat, "tavan 5 sn'den sayılır");
        // 35 sn'de değer geri döndü: bekleyen değişim yine teslim edilir ve kimin doğurduğu kaybolmaz (K7).
        assert_eq!(p.on_tick(35_000, Mode::Quiet, Some(&snap(10.0))), Out::State);
        assert_eq!(p.last_changes, 0b1, "bekleyen cpu değişimi bu `state`'e yazıldı");
    }

    #[test]
    fn frame_format() {
        assert_eq!(frame("state", "{}"), b"event: state\ndata: {}\n\n");
    }
}
