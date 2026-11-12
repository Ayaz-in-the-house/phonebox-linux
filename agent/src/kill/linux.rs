//! Linux sinyal gönderici: `pidfd_open` + kimlik doğrulama + `pidfd_send_signal`. Dış süreç (`kill`, `systemctl`) yok.
//!
//! PID yeniden kullanım yarışı: pidfd açıldıktan sonra o PID'in ait olduğu süreç sabitlenir; ardından `/proc/<pid>/stat`
//! `starttime` taramadaki kimlikle karşılaştırılır. Eşleşmezse (başka süreç aynı PID'i almış) sinyal GÖNDERİLMEZ.

use super::{Sig, Signaller};
use crate::procs::parse_stat;
use std::io;
use std::os::fd::{FromRawFd as _, OwnedFd};

pub struct PidfdSignaller;

fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    // SAFETY: `pidfd_open(pid, 0)` yalnız yeni bir dosya tanıtıcısı döndürür; bellek erişimi yok.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = i32::try_from(fd).map_err(|_| io::Error::other("pidfd aralık dışı"))?;
    // SAFETY: `fd` az önce çekirdekten alınan, başka hiçbir yerde sahiplenilmemiş geçerli bir tanıtıcıdır.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn same_identity(pid: i32, start: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|s| parse_stat(&s)).is_some_and(|s| s.start == start)
}

impl Signaller for PidfdSignaller {
    fn send(&self, pid: i32, start: u64, sig: Sig) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let fd = pidfd_open(pid)?;
        if !same_identity(pid, start) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "süreç değişti (PID yeniden kullanıldı)"));
        }
        let signo = match sig {
            Sig::Term => libc::SIGTERM,
            Sig::Kill => libc::SIGKILL,
        };
        // SAFETY: `fd` geçerli bir pidfd; `info` NULL (çekirdek kendisi doldurur); bayrak 0.
        let r = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), signo, std::ptr::null::<libc::siginfo_t>(), 0) };
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    fn alive(&self, pid: i32, start: u64) -> bool {
        // Zombi (state Z) ölü sayılır: sinyal anlamsız, ebeveyni biçecek.
        let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
        let zombie = s.rfind(')').and_then(|i| s.get(i + 1..)).is_some_and(|r| r.trim_start().starts_with('Z'));
        !zombie && parse_stat(&s).is_some_and(|st| st.start == start)
    }
}

#[cfg(test)]
mod tests {
    //! Gerçek sinyal YALNIZ testin kendi başlattığı kukla süreçlere (PID ile). Ad ile hedef yok.
    use super::*;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    fn start_of(pid: i32) -> u64 {
        parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap()).unwrap().start
    }

    fn wait_exit(c: &mut Child, max: Duration) -> Option<std::process::ExitStatus> {
        let t = Instant::now();
        while t.elapsed() < max {
            if let Ok(Some(s)) = c.try_wait() {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    #[test]
    fn tp_ex_04c_real_term_on_own_dummy() {
        use std::os::unix::process::ExitStatusExt as _;
        let mut c = Command::new("sleep").arg("300").spawn().unwrap();
        let pid = i32::try_from(c.id()).unwrap();
        let start = start_of(pid);
        assert!(PidfdSignaller.alive(pid, start));
        PidfdSignaller.send(pid, start, Sig::Term).unwrap();
        let st = wait_exit(&mut c, Duration::from_secs(5)).expect("SIGTERM ile ölmeli");
        assert_eq!(st.signal(), Some(libc::SIGTERM));
        assert!(!PidfdSignaller.alive(pid, start));
    }

    #[test]
    fn tp_ex_04d_real_kill_after_ignored_term() {
        use std::os::unix::process::ExitStatusExt as _;
        // SIG_IGN exec'ten sonra korunur: `sleep` TERM'i yok sayar.
        let mut c = Command::new("sh").args(["-c", "trap '' TERM; exec sleep 300"]).spawn().unwrap();
        let pid = i32::try_from(c.id()).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let start = start_of(pid);
        PidfdSignaller.send(pid, start, Sig::Term).unwrap();
        assert!(wait_exit(&mut c, Duration::from_millis(500)).is_none(), "TERM yok sayılmalı");
        PidfdSignaller.send(pid, start, Sig::Kill).unwrap();
        let st = wait_exit(&mut c, Duration::from_secs(5)).expect("SIGKILL ile ölmeli");
        assert_eq!(st.signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn wrong_identity_sends_nothing() {
        let mut c = Command::new("sleep").arg("300").spawn().unwrap();
        let pid = i32::try_from(c.id()).unwrap();
        let start = start_of(pid);
        assert!(PidfdSignaller.send(pid, start + 1, Sig::Term).is_err());
        assert!(wait_exit(&mut c, Duration::from_millis(300)).is_none(), "yanlış kimlikle sinyal gitmemeli");
        PidfdSignaller.send(pid, start, Sig::Kill).unwrap();
        assert!(wait_exit(&mut c, Duration::from_secs(5)).is_some());
    }
}
