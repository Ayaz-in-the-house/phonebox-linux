#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! W2.4 uç testleri: `procs` (TP-EX-05), korumalı kapatma (TP-EX-04), medya, `/api/debug/topics`.
//!
//! ⛔ Gerçek sinyal YALNIZ testin kendi başlattığı kukla süreçlere, PID ile. Ret listesi testleri sahte süreç listesi +
//! kayıtçı gönderici kullanır: ret kuralı bozulsa (mutant) bile gerçek bir süreç (herdr, gnome-shell) hedeflenemez.

mod common;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use common::{body_bytes, get, get_json, harness, harness_extras, send};
use phoned::app::Extras;
use phoned::kill::fake::Recorder;
use phoned::kill::{Killer, Sig};
use phoned::media::Media;
use phoned::procs::fake::FakeProcs;
use phoned::procs::{ProcInfo, Procs};
use serde_json::Value;
use std::os::unix::fs::MetadataExt as _;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn post(path: &str) -> Request<Body> {
    Request::builder().method("POST").uri(path).header("host", "127.0.0.1:8766").body(Body::empty()).unwrap()
}

async fn post_json(app: &axum::Router, path: &str) -> (u16, Value) {
    let res = send(app, post(path)).await;
    let status = res.status().as_u16();
    (status, serde_json::from_slice(&body_bytes(res).await).unwrap_or(Value::Null))
}

fn enable(h: &common::Harness, allow: &[&str]) {
    let p = h.config_path().with_file_name("kill-policy.json");
    std::fs::write(p, serde_json::json!({"allowKill": true, "allow": allow}).to_string()).unwrap();
}

fn my_uid() -> u32 {
    std::fs::metadata("/proc/self").unwrap().uid()
}

fn dummy() -> (Child, i32) {
    let c = Command::new("sleep").arg("300").spawn().unwrap();
    let pid = i32::try_from(c.id()).unwrap();
    (c, pid)
}

fn exited(c: &mut Child, max: Duration) -> Option<std::process::ExitStatus> {
    let t = Instant::now();
    while t.elapsed() < max {
        if let Ok(Some(s)) = c.try_wait() {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn reap(mut c: Child) {
    let _ = c.kill();
    let _ = c.wait();
}

/// Sahte düzenek: verilen süreçler + kayıtçı gönderici (gerçek sinyal yok).
fn fake(list: Vec<ProcInfo>) -> (common::Harness, Recorder) {
    let src = FakeProcs::default();
    *src.0.lock().unwrap() = list;
    let rec = Recorder::default();
    let ext = Extras {
        procs: Arc::new(Procs::new(Box::new(src))),
        killer: Arc::new(Killer::new(Box::new(rec.clone()))),
        media: Arc::new(Media::new()),
        audio: Arc::new(phoned::audio::Audio::new(phoned::audio::Settings { socket: "/yok/pulse/native".into(), cookie: None })),
        ctx: phoned::kill::Ctx { self_pid: 4242, uid: my_uid() },
    };
    (harness_extras(ext), rec)
}

fn proc(pid: i32, comm: &str, unit: &str) -> ProcInfo {
    ProcInfo { pid, comm: comm.into(), uid: my_uid(), unit: unit.into(), cmdline: comm.into(), start: 9, ..ProcInfo::default() }
}

#[tokio::test]
async fn tp_w24_p3_procs_shape_and_top() {
    let h = harness();
    let v = get_json(&h.app, "/api/procs?top=150").await;
    let mut errs = Vec::new();
    common::compare(&common::fixture("api_procs"), &v, "", &common::Rules::default(), &mut errs);
    assert!(errs.is_empty(), "{errs:?}");
    let groups = v["groups"].as_array().unwrap();
    assert!(!groups.is_empty(), "gerçek /proc taraması boş olamaz (en az test süreci)");
    for k in ["name", "title", "count", "cpu", "memMb", "locked", "hasIcon"] {
        assert!(groups[0].get(k).is_some(), "{k}");
    }
    assert!(v["total"].as_u64().unwrap() >= v["groupCount"].as_u64().unwrap());
    let one = get_json(&h.app, "/api/procs?top=1").await;
    assert_eq!(one["groups"].as_array().unwrap().len(), 1);
    let none = get_json(&h.app, "/api/procs?top=-5").await;
    assert_eq!(none["groups"].as_array().unwrap().len(), 1, "0.6.9 Clamp(1..200)");
}

#[tokio::test]
async fn tp_ex_05_procs_runs_only_on_pull() {
    let (h, _) = fake(vec![proc(100, "loupe", "app-x.scope")]);
    let t = get_json(&h.app, "/api/debug/topics").await;
    assert_eq!(t["procs"]["runs"], 0, "istenmeden tarama yok");
    assert_eq!(t["procs"]["trigger"], "pull");
    assert!(t["procs"]["lastPullAgeMs"].is_null());
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert_eq!(get_json(&h.app, "/api/debug/topics").await["procs"]["runs"], 0, "arka plan görevi yok");
    get_json(&h.app, "/api/procs").await;
    get_json(&h.app, "/api/procs").await;
    let t = get_json(&h.app, "/api/debug/topics").await;
    assert_eq!(t["procs"]["runs"], 1, "aynı saniyedeki ikinci istek önbellekten");
    assert_eq!(t["procs"]["cachedHits"], 1);
    assert!(t["procs"]["lastPullAgeMs"].is_u64());
}

#[tokio::test]
async fn tp_w24_p2_detail_and_locked() {
    let (h, _) = fake(vec![proc(100, "loupe", "app-x.scope"), proc(101, "herdr", "herdr-server.service")]);
    let v = get_json(&h.app, "/api/procs").await;
    let g = v["groups"].as_array().unwrap();
    let find = |n: &str| g.iter().find(|x| x["name"] == n).unwrap().clone();
    assert_eq!(find("loupe")["locked"], false);
    assert_eq!(find("herdr")["locked"], true);
    let d = get_json(&h.app, "/api/procs/detail?name=loupe").await;
    assert_eq!(d["instances"][0]["pid"], 100);
    for k in ["pid", "cpu", "memMb", "startedAt", "session", "window", "args", "locked"] {
        assert!(d["instances"][0].get(k).is_some(), "{k}");
    }
}

#[tokio::test]
async fn tp_ex_04a_disabled_by_default_real_dummy_survives() {
    let h = harness();
    let (mut c, pid) = dummy();
    let (status, body) = post_json(&h.app, &format!("/api/procs/kill?pid={pid}")).await;
    assert_eq!(status, 403);
    assert_eq!(body["killed"], 0);
    assert!(body["error"].as_str().unwrap().contains("kapalı"));
    assert!(exited(&mut c, Duration::from_millis(300)).is_none(), "kapalı uç süreç öldürmemeli");
    assert_eq!(get_json(&h.app, "/api/debug/topics").await["kill"]["enabled"], false);
    reap(c);
}

#[tokio::test]
async fn tp_ex_04b_deny_list_endpoint_sends_nothing() {
    let list = vec![
        proc(10, "gnome-shell", "org.gnome.Shell@user.service"),
        proc(11, "herdr", "herdr-server.service"),
        proc(12, "ghostty", "app-com.mitchellh.ghostty-1.scope"),
        proc(13, "zsh", "app-claude-session-77.scope"),
        proc(14, "chromium", "wa-gateway-core.service"),
        proc(4242, "phoned", "phoned-shadow.scope"),
        ProcInfo { uid: my_uid() + 1, ..proc(15, "loupe", "app-y.scope") },
    ];
    let (h, rec) = fake(list);
    enable(&h, &["zsh", "chromium", "herdr", "loupe"]);
    for q in ["name=gnome-shell", "name=herdr", "name=ghostty", "pid=13", "pid=14", "pid=4242", "pid=1", "pid=15"] {
        let (status, body) = post_json(&h.app, &format!("/api/procs/kill?{q}")).await;
        assert_eq!(status, 403, "{q}: {body}");
        assert_eq!(body["killed"], 0, "{q}");
        assert!(body["error"].is_string(), "{q}");
    }
    assert!(rec.sent.lock().unwrap().is_empty(), "ret listesinde hiçbir sinyal gitmez");
    let t = get_json(&h.app, "/api/debug/topics").await;
    assert_eq!(t["kill"]["denied"], 8);
    assert_eq!(t["kill"]["requests"], 8);
    assert_eq!(t["kill"]["term"], 0);
}

#[tokio::test]
async fn tp_ex_04c_allowed_dummy_gets_sigterm() {
    use std::os::unix::process::ExitStatusExt as _;
    let h = harness();
    enable(&h, &["sleep"]);
    let (mut c, pid) = dummy();
    // Kuklanın kapsamı korumalıysa (ör. laptopta ajan oturumu `app-claude-session-*`) doğru yanıt 403'tür; filoda 200 beklenir.
    let me = phoned::procs::linux::read_one(pid).unwrap();
    let expect_allow = phoned::kill::judge(&me, &[], phoned::kill::http::ctx(), &["sleep".into()]) == phoned::kill::Verdict::Allow;
    eprintln!("kukla birimi: {:?} → izin={expect_allow}", me.unit);
    let (status, body) = post_json(&h.app, &format!("/api/procs/kill?pid={pid}")).await;
    if expect_allow {
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["killed"], 1);
        assert_eq!(body["signal"], "TERM");
        assert_eq!(body["pids"][0], pid);
        let st = exited(&mut c, Duration::from_secs(5)).expect("SIGTERM ile ölmeli");
        assert_eq!(st.signal(), Some(libc::SIGTERM));
        assert_eq!(get_json(&h.app, "/api/debug/topics").await["kill"]["term"], 1);
    } else {
        assert_eq!(status, 403, "{body}");
        assert!(exited(&mut c, Duration::from_millis(300)).is_none());
    }
    reap(c);
}

#[tokio::test]
async fn tp_ex_04d_second_request_waits_then_kills() {
    let (h, rec) = fake(vec![proc(100, "loupe", "app-x.scope")]);
    enable(&h, &[]);
    let (s1, b1) = post_json(&h.app, "/api/procs/kill?pid=100").await;
    assert_eq!((s1, b1["signal"].clone()), (200, Value::from("TERM")));
    let (s2, b2) = post_json(&h.app, "/api/procs/kill?pid=100").await;
    assert_eq!(s2, 409, "5 sn dolmadan zorla kapatma yok: {b2}");
    assert!(b2["error"].as_str().unwrap().contains("sn"));
    tokio::time::sleep(Duration::from_millis(phoned::kill::GRACE_MS + 100)).await;
    let (s3, b3) = post_json(&h.app, "/api/procs/kill?pid=100").await;
    assert_eq!((s3, b3["signal"].clone()), (200, Value::from("KILL")));
    assert_eq!(*rec.sent.lock().unwrap(), vec![(100, Sig::Term), (100, Sig::Kill)]);
}

#[tokio::test]
async fn tp_ex_04e_non_loopback_peer_rejected() {
    let (h, rec) = fake(vec![proc(100, "loupe", "app-x.scope")]);
    enable(&h, &[]);
    let mut req = post("/api/procs/kill?pid=100");
    req.extensions_mut().insert(ConnectInfo(std::net::SocketAddr::from(([192, 168, 1, 5], 40000))));
    let res = send(&h.app, req).await;
    assert_eq!(res.status(), 403);
    let mut ok = post("/api/procs/kill?pid=100");
    ok.extensions_mut().insert(ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000))));
    assert_eq!(send(&h.app, ok).await.status(), 200);
    assert_eq!(rec.sent.lock().unwrap().len(), 1, "yalnız yerel istek sinyal üretti");
}

#[tokio::test]
async fn tp_w24_d1_topics_and_media_without_players() {
    let (h, _) = fake(vec![]);
    let t = get_json(&h.app, "/api/debug/topics").await;
    for k in ["pc", "procs", "media", "kill", "changes", "mode"] {
        assert!(t.get(k).is_some(), "{k}");
    }
    assert_eq!(t["media"]["trigger"], "dbus");
    assert_eq!(t["media"]["players"], 0);
    let st = get_json(&h.app, "/api/state").await;
    assert!(st["media"].is_null(), "oyuncu yok → null (0.6.9 eşi)");
    assert_eq!(send(&h.app, post("/api/media/playpause")).await.status(), 400);
    assert_eq!(send(&h.app, post("/api/volume?op=up")).await.status(), 400);
    assert_eq!(send(&h.app, get("/api/art")).await.status(), 404);
    // GET ile kapatma yok.
    assert_eq!(send(&h.app, get("/api/procs/kill?pid=1")).await.status(), 405);
}
