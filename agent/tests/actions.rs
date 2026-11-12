#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! W2.3: TP-W2-P3/P4/P5 (push · notify · goto · refresh · run) · TP-W2-08b/c (herdr kapısı, değişim → akış) · TP-W2-09 (Zen).

mod common;
use axum::body::Body;
use axum::http::Request;
use common::{body_bytes, get, get_json, harness, harness_with, send};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

fn req(method: &str, path: &str, ctype: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path).header("host", "127.0.0.1:8766");
    if let Some(c) = ctype {
        b = b.header("content-type", c);
    }
    b.body(Body::from(body.to_owned())).unwrap()
}

async fn json_of(res: axum::http::Response<Body>) -> (u16, Value) {
    let st = res.status().as_u16();
    let b = body_bytes(res).await;
    (st, if b.is_empty() { Value::Null } else { serde_json::from_slice(&b).unwrap() })
}

#[tokio::test]
async fn tp_w2_p3_push_roundtrip_over_http() {
    let h = harness();
    let v0 = get_json(&h.app, "/api/push").await["version"].as_i64().unwrap();
    let (st, r) = json_of(send(&h.app, req("POST", "/api/push/temp?ttl=60", Some("text/plain"), "21.5")).await).await;
    assert_eq!(st, 200);
    assert_eq!((r["ok"].clone(), r["key"].clone()), (json!(true), json!("temp")));
    assert!(r["version"].as_i64().unwrap() > v0);
    let (_, r) = json_of(send(&h.app, req("PUT", "/api/push/obj", Some("application/json"), r#"{"a":[1,2]}"#)).await).await;
    assert_eq!(r["ok"], json!(true));
    send(&h.app, req("POST", "/api/push/q?value=true", None, "yok sayılır")).await;
    let all = get_json(&h.app, "/api/push").await;
    assert_eq!(all["items"]["temp"]["data"], json!(21.5));
    assert!(all["items"]["temp"]["expiresAt"].is_i64());
    assert_eq!(all["items"]["obj"]["data"], json!({"a":[1,2]}));
    assert_eq!(all["items"]["q"]["data"], json!(true));
    assert!(all["items"]["obj"]["expiresAt"].is_null());
    assert_eq!(get_json(&h.app, "/api/push/obj").await["data"]["a"][1], json!(2));
    assert_eq!(get_json(&h.app, "/agent/api/push/obj").await["data"]["a"][0], json!(1), "/agent öneki");
    let s = get_json(&h.app, "/api/state").await;
    assert_eq!(s["pushVersion"], all["version"], "state pushVersion = push sürümü");
    assert_eq!(send(&h.app, req("POST", "/api/push/bad%20key", None, "1")).await.status(), 400);
    assert_eq!(send(&h.app, req("POST", "/api/push/j", Some("application/json"), "{")).await.status(), 400);
    assert_eq!(send(&h.app, req("POST", "/api/push/big", None, &"x".repeat(64 * 1024 + 1))).await.status(), 400);
    assert_eq!(send(&h.app, req("DELETE", "/api/push/obj", None, "")).await.status(), 200);
    assert_eq!(send(&h.app, req("DELETE", "/api/push/obj", None, "")).await.status(), 404);
    assert_eq!(send(&h.app, get("/api/push/obj")).await.status(), 404);
    assert_eq!(send(&h.app, req("DELETE", "/api/push", None, "")).await.status(), 200);
    assert_eq!(get_json(&h.app, "/api/push").await["items"], json!({}));
}

#[tokio::test]
async fn tp_w2_p4_notify_goto_refresh_and_state_ev() {
    let h = harness();
    let (st, e1) = json_of(send(&h.app, req("POST", "/api/notify", Some("text/plain"), "merhaba")).await).await;
    assert_eq!(st, 200);
    assert_eq!((e1["seq"].clone(), e1["kind"].clone(), e1["data"].clone()), (json!(1), json!("notify"), json!({"text":"merhaba"})));
    assert!(e1["at"].is_i64());
    let (_, e2) = json_of(send(&h.app, req("POST", "/api/notify", Some("application/json"), r#"{"title":"T","level":"warn"}"#)).await).await;
    assert_eq!(e2["data"]["title"], "T");
    assert_eq!(send(&h.app, req("POST", "/api/notify", Some("application/json"), r#"{"level":"x"}"#)).await.status(), 400);
    let (_, g) = json_of(send(&h.app, req("POST", "/api/goto/3?countdown=99", None, "")).await).await;
    assert_eq!(g["data"], json!({"page":"3","countdown":30}));
    let (_, g) = json_of(send(&h.app, req("POST", "/api/goto/home", None, "")).await).await;
    assert_eq!(g["data"], json!({"page":"home","countdown":null}));
    let (_, r) = json_of(send(&h.app, req("POST", "/api/refresh?reload=1", None, "")).await).await;
    assert_eq!((r["kind"].clone(), r["data"].clone()), (json!("reload"), Value::Null));
    let (_, r) = json_of(send(&h.app, req("POST", "/api/refresh", None, "")).await).await;
    assert_eq!(r["kind"], "refresh");
    let s = get_json(&h.app, "/api/state").await;
    assert_eq!(s["evSeq"], json!(6));
    assert_eq!(s["events"], json!([]), "?ev yoksa olay yok");
    let s = get_json(&h.app, "/api/state?ev=4").await;
    let seqs: Vec<i64> = s["events"].as_array().unwrap().iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert_eq!(seqs, [5, 6]);
}

fn frame_text(b: &[u8]) -> String {
    String::from_utf8(b.to_vec()).unwrap()
}

/// Olay ve push değişimi akışı bekleyen tik olmadan uyandırır.
#[tokio::test]
async fn tp_w2_p4_08c_stream_event_and_push_wake_immediately() {
    let h = harness();
    let mut body = send(&h.app, get("/api/stream")).await.into_body();
    let _work = body.frame().await.unwrap().unwrap();
    let _state = body.frame().await.unwrap().unwrap();
    let t0 = Instant::now();
    send(&h.app, req("POST", "/api/notify", Some("text/plain"), "anında")).await;
    let f = frame_text(&body.frame().await.unwrap().unwrap().into_data().unwrap());
    assert!(t0.elapsed() < Duration::from_millis(100), "olay bekletildi: {:?}", t0.elapsed());
    assert!(f.starts_with("event: event\ndata: "), "{f}");
    assert!(f.contains("\"anında\""));
    let t1 = Instant::now();
    let (_, r) = json_of(send(&h.app, req("POST", "/api/push/herdr", Some("application/json"), r#"{"x":1}"#)).await).await;
    let f = frame_text(&body.frame().await.unwrap().unwrap().into_data().unwrap());
    assert!(t1.elapsed() < Duration::from_millis(100), "push bekletildi: {:?}", t1.elapsed());
    assert!(f.starts_with("event: state\ndata: "), "{f}");
    let v: Value = serde_json::from_str(f.trim_start_matches("event: state\ndata: ").trim_end()).unwrap();
    assert_eq!(v["pushVersion"], r["version"]);
}

#[tokio::test]
async fn tp_w2_p5_run_unknown_and_unsupported() {
    let h = harness_with(
        |c| {
            c["shortcuts"].as_array_mut().unwrap().push(json!({"id":"m","label":"Mute","cmd":"builtin:mute"}));
        },
        |l| l,
    );
    let res = send(&h.app, req("POST", "/api/run/yok", None, "")).await;
    assert_eq!(res.status(), 500);
    assert_eq!(res.headers()["content-type"], "application/problem+json");
    let (_, p) = json_of(res).await;
    assert_eq!(p["detail"], "no such shortcut: yok");
    let (st, p) = json_of(send(&h.app, req("POST", "/api/run/m", None, "")).await).await;
    assert_eq!(st, 500);
    assert!(p["detail"].as_str().unwrap().contains("builtin:mute"));
}

/// Sahte herdr sunucusu: her bağlantıda sayaç artar, verilen satırı yazar.
fn fake_herdr(path: std::path::PathBuf, reply: Arc<std::sync::Mutex<String>>) -> Arc<AtomicUsize> {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = Arc::clone(&n);
    let _ = std::fs::remove_file(&path);
    let l = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            n2.fetch_add(1, Ordering::SeqCst);
            let line = reply.lock().unwrap().clone();
            tokio::spawn(async move {
                let mut r = tokio::io::BufReader::new(s);
                let mut got = String::new();
                r.read_line(&mut got).await.unwrap();
                r.get_mut().write_all(line.as_bytes()).await.unwrap();
            });
        }
    });
    n
}

fn agents_line(st: &str) -> String {
    format!(
        "{}\n",
        json!({"id":"phoned","result":{"type":"agent_list","agents":[
        {"agent":"claude","agent_status":st,"focused":false,"pane_id":"wC:pA1","terminal_title_stripped":"İşçi A","foreground_cwd":"/p/phoned"},
        {"agent":"codex","agent_status":"idle","focused":true,"pane_id":"wC:pB2","terminal_title_stripped":"İşçi B"}]}})
    )
}

fn no_night(c: &mut Value) {
    c["night"]["enabled"] = json!(false);
    c["shortcuts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"herdr-go-1","label":"herdr 1","cmd":"/home/u/.local/bin/herdr-phonebox-bridge","args":"go 1"}));
    c["shortcuts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"herdr-go-9","label":"herdr 9","cmd":"/home/u/.local/bin/herdr-phonebox-bridge","args":"go 9"}));
}

/// TP-W2-08b: biri bakmıyorken herdr'a 0 istek; bakılırken 1,5 sn'de bir. TP-W2-08c: değişim → push + yeni blocked → notify.
#[tokio::test]
async fn tp_w2_08bc_gate_and_change() {
    let h = harness_with(no_night, |l| l);
    let reply = Arc::new(std::sync::Mutex::new(agents_line("working")));
    let n = fake_herdr(h.st.ext.links.herdr_sock.clone(), Arc::clone(&reply));
    // Harness iki örnek aldı ama çekme/akış yok → bakan yok.
    let lp = tokio::spawn(phoned::actions::herdr_loop(Arc::clone(&h.st)));
    assert!(!phoned::app::looking(&h.st));
    tokio::time::sleep(Duration::from_millis(3_400)).await;
    assert_eq!(n.load(Ordering::SeqCst), 0, "bakan yokken herdr'a istek gitti");
    // Kilitli + akış → yine bakan yok sayılır.
    h.locked.send_replace(true);
    let stream = send(&h.app, get("/api/stream")).await;
    tokio::time::sleep(Duration::from_millis(1_700)).await;
    assert_eq!(n.load(Ordering::SeqCst), 0, "kilitliyken herdr'a istek gitti");
    h.locked.send_replace(false);
    tokio::time::sleep(Duration::from_millis(3_200)).await;
    let polled = n.load(Ordering::SeqCst);
    assert!((2..=3).contains(&polled), "bakılırken 1,5 sn'de bir: {polled}");
    let p = get_json(&h.app, "/api/push").await;
    let herdr = &p["items"]["herdr"]["data"];
    assert_eq!(herdr["agents"][0]["slot"], json!(1));
    assert_eq!(herdr["agents"][0]["status"], "working");
    assert_eq!(herdr["counts"]["idle"], json!(1));
    let seq0 = get_json(&h.app, "/api/state").await["evSeq"].as_i64().unwrap();
    *reply.lock().unwrap() = agents_line("blocked");
    tokio::time::sleep(Duration::from_millis(1_700)).await;
    let s = get_json(&h.app, &format!("/api/state?ev={seq0}")).await;
    let ev = s["events"].as_array().unwrap();
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["data"]["text"], "herdr: İşçi A onay bekliyor");
    assert_eq!(get_json(&h.app, "/api/push").await["items"]["herdr"]["data"]["agents"][0]["status"], "blocked");
    drop(stream);
    lp.abort();
}

/// TP-W2-09: `run/herdr-go-1` → Zen soketine `route <url>` (host = bu test süreci, ikilisi işareti taşır); süreç yok.
#[tokio::test]
async fn tp_w2_09_run_herdr_go_routes_to_zen() {
    let h = harness_with(no_night, |l| l);
    let reply = Arc::new(std::sync::Mutex::new(agents_line("idle")));
    let _n = fake_herdr(h.st.ext.links.herdr_sock.clone(), reply);
    // Zen soketi yokken: açık hata.
    let (st, p) = json_of(send(&h.app, req("POST", "/api/run/herdr-go-1", None, "")).await).await;
    assert_eq!(st, 500);
    assert!(p["detail"].as_str().unwrap().contains("Zen soketi yok"), "{p}");
    let zsock = h.st.ext.links.zen_sock.clone();
    let _ = std::fs::remove_file(&zsock);
    let l = tokio::net::UnixListener::bind(&zsock).unwrap();
    let got = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let g2 = Arc::clone(&got);
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let mut r = tokio::io::BufReader::new(s);
            let mut line = String::new();
            r.read_line(&mut line).await.unwrap();
            g2.lock().unwrap().push(line);
            r.get_mut().write_all(b"routed 4\n").await.unwrap();
        }
    });
    let res = send(&h.app, req("POST", "/api/run/herdr-go-1", None, "")).await;
    assert_eq!(res.status(), 200);
    assert_eq!(got.lock().unwrap().as_slice(), ["route https://h.example/herdr/#/t/wC:pA1\n"]);
    let (st, p) = json_of(send(&h.app, req("POST", "/api/run/herdr-go-9", None, "")).await).await;
    assert_eq!(st, 500);
    assert_eq!(p["detail"], "yuva 9 boş");
    assert_eq!(got.lock().unwrap().len(), 1, "boş yuva Zen'e gitmez");
}
