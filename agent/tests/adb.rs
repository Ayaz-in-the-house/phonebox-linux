#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! TP-W2-07b/c/d/f: sahte adb sunucusuna (geçici TCP portu) karşı izleme + reverse; gerçek 5037'ye dokunulmaz.

mod common;
use phoned::adb::{self, Ready, Settings};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// Sahte adb sunucusu durumu: cihazlar, kurulu paketler, reverse kuralları, gelen servis kayıtları.
#[derive(Default)]
struct Fake {
    devices: String,
    installed: Vec<String>,
    rules: Vec<(String, String)>,
    log: Vec<String>,
}

type F = Arc<Mutex<Fake>>;

fn hexed(s: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", s.len()).into_bytes();
    v.extend_from_slice(s.as_bytes());
    v
}

async fn read_req(s: &mut TcpStream) -> Option<String> {
    let mut l = [0u8; 4];
    s.read_exact(&mut l).await.ok()?;
    let n = usize::from_str_radix(std::str::from_utf8(&l).ok()?, 16).ok()?;
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).await.ok()?;
    String::from_utf8(b).ok()
}

async fn conn(mut s: TcpStream, f: F) {
    let Some(first) = read_req(&mut s).await else { return };
    if first == "host:track-devices" {
        s.write_all(b"OKAY").await.unwrap();
        let d = f.lock().unwrap().devices.clone();
        s.write_all(&hexed(&d)).await.unwrap();
        // Bağlantı açık kalır (gerçek sunucu gibi); test bitince düşer.
        let mut sink = [0u8; 1];
        let _ = s.read(&mut sink).await;
        return;
    }
    let Some(serial) = first.strip_prefix("host:transport:") else { return };
    s.write_all(b"OKAY").await.unwrap();
    let Some(svc) = read_req(&mut s).await else { return };
    f.lock().unwrap().log.push(format!("{serial} {svc}"));
    s.write_all(b"OKAY").await.unwrap();
    let out: Vec<u8> = if svc == "reverse:list-forward" {
        let rules = f.lock().unwrap().rules.iter().filter(|(s, _)| s == serial).map(|(s, r)| format!("{s} {r}\n")).collect::<String>();
        [b"OKAY".as_slice(), &hexed(&rules)].concat()
    } else if let Some(r) = svc.strip_prefix("reverse:forward:") {
        f.lock().unwrap().rules.push((serial.to_owned(), r.replace(';', " ")));
        b"OKAY".to_vec()
    } else if let Some(p) = svc.strip_prefix("shell:pm path ") {
        if f.lock().unwrap().installed.iter().any(|i| i == p) { format!("package:/data/app/{p}/base.apk\n").into_bytes() } else { Vec::new() }
    } else {
        Vec::new()
    };
    s.write_all(&out).await.unwrap();
}

async fn fake(f: F) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            tokio::spawn(conn(s, Arc::clone(&f)));
        }
    });
    addr
}

fn settings(addr: &str, pkgs: &[&str]) -> Settings {
    Settings {
        addr: Some(addr.into()),
        packages: pkgs.iter().map(|p| (*p).to_owned()).collect(),
        phone_port: 18766,
        agent_port: 18766,
        open_panel: true,
    }
}

#[tokio::test]
async fn tp_w2_07b_ensure_sets_reverse_once_and_opens_panel() {
    let f: F = Arc::new(Mutex::new(Fake { installed: vec![adb::DEFAULT_PACKAGE.into()], ..Fake::default() }));
    let addr = fake(Arc::clone(&f)).await;
    let cfg = settings(&addr, &[adb::DEFAULT_PACKAGE]);
    let r = adb::ensure(&cfg, &addr, "S1").await.unwrap();
    assert_eq!(r, Ready::Set { pkg: adb::DEFAULT_PACKAGE.into() });
    let log = f.lock().unwrap().log.clone();
    assert_eq!(
        log,
        [
            "S1 reverse:list-forward",
            "S1 shell:pm path net.oxog.phonebox.deskpanel",
            "S1 reverse:forward:tcp:18766;tcp:18766",
            "S1 shell:input keyevent KEYCODE_WAKEUP",
            "S1 shell:am start -n net.oxog.phonebox.deskpanel/net.oxog.phonebox.deskpanel.MainActivity",
        ]
    );
    f.lock().unwrap().log.clear();
    assert_eq!(adb::ensure(&cfg, &addr, "S1").await.unwrap(), Ready::Already);
    assert_eq!(f.lock().unwrap().log, ["S1 reverse:list-forward"], "kural varken yazma yok");
}

#[tokio::test]
async fn tp_w2_07c_configured_package_list() {
    let f: F = Arc::new(Mutex::new(Fake { installed: vec!["com.example.panel".into()], ..Fake::default() }));
    let addr = fake(Arc::clone(&f)).await;
    let cfg = settings(&addr, &["net.oxog.phonebox.deskpanel", "com.example.panel"]);
    assert_eq!(adb::ensure(&cfg, &addr, "S2").await.unwrap(), Ready::Set { pkg: "com.example.panel".into() });
    let log = f.lock().unwrap().log.clone();
    assert!(log.contains(&"S2 shell:am start -n com.example.panel/net.oxog.phonebox.deskpanel.MainActivity".to_owned()), "{log:?}");
    let f2: F = Arc::new(Mutex::new(Fake::default()));
    let addr2 = fake(Arc::clone(&f2)).await;
    assert_eq!(adb::ensure(&settings(&addr2, &["a.b"]), &addr2, "S3").await.unwrap(), Ready::Missing);
    assert!(f2.lock().unwrap().rules.is_empty(), "uygulama yokken reverse kurulmaz");
}

#[tokio::test]
async fn tp_w2_07bd_tracker_reports_devices_to_info() {
    let f: F = Arc::new(Mutex::new(Fake {
        devices: "S1\tdevice\nS9\tunauthorized\nS4\tdevice\n".into(),
        installed: vec![adb::DEFAULT_PACKAGE.into()],
        ..Fake::default()
    }));
    let addr = fake(Arc::clone(&f)).await;
    let h = common::harness_with(
        |c| c["deviceSerial"] = json!(""),
        |mut l| {
            l.adb = settings(&addr, &["x.missing", adb::DEFAULT_PACKAGE]);
            l
        },
    );
    let st = Arc::clone(&h.st);
    let serial = move || st.cfg.snapshot().0.get("deviceSerial").and_then(|v| v.as_str()).map(str::to_owned);
    let task = tokio::spawn(adb::run(h.st.ext.links.adb.clone(), Arc::clone(&h.st.ext.adb), serial));
    let mut info = json!(null);
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        info = common::get_json(&h.app, "/api/info").await;
        if info["devices"].as_array().is_some_and(|d| d.len() == 2) {
            break;
        }
    }
    assert_eq!(info["devices"], json!(["S1", "S4"]));
    assert_eq!(info["connected"], json!(true));
    assert_eq!(info["appMissing"], json!([]));
    assert!(info["adb"].as_str().unwrap().starts_with("socket 127.0.0.1:"));
    let rules = f.lock().unwrap().rules.clone();
    assert_eq!(rules.len(), 2, "{rules:?}");
    task.abort();
}

#[tokio::test]
async fn tp_w2_07f_no_server_is_disconnected_and_backs_off() {
    // Boş bir port: bağlan → reddedilir.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    drop(l);
    let shared = adb::Shared::default();
    let task = tokio::spawn(adb::run(settings(&addr, &["a.b"]), Arc::clone(&shared), || None));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(adb::status(&shared), adb::Status::default());
    assert!(!task.is_finished(), "döngü yeniden denemek için bekliyor");
    task.abort();
    assert!(adb::RETRY >= Duration::from_secs(5));
}
