#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! W2.4b ses: sahte pulse yerel sunucusu (geçici dizinde UNIX soketi) ile uçtan uca — gerçek ses sunucusuna dokunulmaz.
//! Sunucu, istemcinin gönderdiği paketleri `phoned::audio::proto` ile çözer ve kaydeder (TP-W24b-A3/A4/A6/A7/A8).

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{body_bytes, get, get_json, harness_extras, send};
use http_body_util::BodyExt as _;
use phoned::app::Extras;
use phoned::audio::proto::{self, Writer, cmd};
use phoned::audio::{Audio, Settings, client};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

type Log = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Default)]
struct Sink {
    volumes: Vec<u32>,
    mute: bool,
}

/// Sahte sunucu: tek sink (dizin 7, 2 kanal), aldığı komutların kaydı, dışarıdan olay gönderme kanalı.
struct Fake {
    sink: Arc<Mutex<Sink>>,
    got: Log,
    push: mpsc::UnboundedSender<u32>,
    accepts: Arc<std::sync::atomic::AtomicU64>,
}

impl Fake {
    fn cmds(&self, c: u32) -> Vec<Vec<u8>> {
        self.got.lock().unwrap().iter().filter(|(k, _)| *k == c).map(|(_, b)| b.clone()).collect()
    }
    /// Kullanıcı başka yerden sesi değiştirdi: durum + sink değişim olayı.
    fn external(&self, pct: i64, mute: bool) {
        *self.sink.lock().unwrap() = Sink { volumes: vec![proto::from_percent(pct); 2], mute };
        self.push.send(0x10 | proto::FACILITY_SINK).unwrap();
    }
}

fn sink_reply(tag: u32, s: &Sink) -> Writer {
    Writer::command(cmd::REPLY, tag)
        .u32(7)
        .string(Some("alsa_output.fake"))
        .string(Some("Sahte"))
        .sample_spec(3, 2, 48_000)
        .channel_map(&[1, 2])
        .u32(proto::INVALID)
        .cvolume(&s.volumes)
        .boolean(s.mute)
        .u32(7)
}

fn event(kind: u32) -> Vec<u8> {
    Writer::command(cmd::SUBSCRIBE_EVENT, proto::INVALID).u32(kind).u32(7).packet()
}

fn fake(dir: &std::path::Path, start_pct: i64) -> (Fake, PathBuf) {
    let path = dir.join("native");
    let l = UnixListener::bind(&path).unwrap();
    let sink = Arc::new(Mutex::new(Sink { volumes: vec![proto::from_percent(start_pct); 2], mute: false }));
    let got: Log = Arc::new(Mutex::new(Vec::new()));
    let accepts = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (push, mut events) = mpsc::unbounded_channel::<u32>();
    let (s2, g2, a2) = (Arc::clone(&sink), Arc::clone(&got), Arc::clone(&accepts));
    tokio::spawn(async move {
        let (mut sock, _) = l.accept().await.unwrap();
        a2.fetch_add(1, Relaxed);
        let mut h = [0u8; proto::HEADER];
        loop {
            tokio::select! {
                r = sock.read_exact(&mut h) => {
                    if r.is_err() { return; }
                    let (len, _) = proto::parse_header(&h).unwrap();
                    let mut b = vec![0u8; len as usize];
                    sock.read_exact(&mut b).await.unwrap();
                    let (c, tag, mut rd) = proto::split(&b).unwrap();
                    g2.lock().unwrap().push((c, b.clone()));
                    let (reply, ev) = match c {
                        cmd::AUTH => (Writer::command(cmd::REPLY, tag).u32(35), None),
                        cmd::SET_CLIENT_NAME => (Writer::command(cmd::REPLY, tag).u32(1), None),
                        cmd::SUBSCRIBE => (Writer::command(cmd::REPLY, tag), None),
                        cmd::GET_SINK_INFO => (sink_reply(tag, &s2.lock().unwrap()), None),
                        cmd::SET_SINK_VOLUME => {
                            let (_idx, _name, v) = (rd.u32().unwrap(), rd.string().unwrap(), rd.cvolume().unwrap());
                            s2.lock().unwrap().volumes = v;
                            (Writer::command(cmd::REPLY, tag), Some(0x10))
                        }
                        cmd::SET_SINK_MUTE => {
                            let (_idx, _name, m) = (rd.u32().unwrap(), rd.string().unwrap(), rd.boolean().unwrap());
                            s2.lock().unwrap().mute = m;
                            (Writer::command(cmd::REPLY, tag), Some(0x10))
                        }
                        _ => (Writer::command(cmd::ERROR, tag).u32(1), None),
                    };
                    // Gerçek sunucu gibi: olay yanıttan ÖNCE gelebilir (istemci etiketle eşleştirmeli).
                    if let Some(k) = ev { sock.write_all(&event(k)).await.unwrap(); }
                    sock.write_all(&reply.packet()).await.unwrap();
                }
                k = events.recv() => {
                    let Some(k) = k else { return };
                    sock.write_all(&event(k)).await.unwrap();
                }
            }
        }
    });
    (Fake { sink, got, push, accepts }, path)
}

fn extras(socket: PathBuf) -> (Extras, Arc<Audio>) {
    let mut e = Extras::linux();
    let a = Arc::new(Audio::new(Settings { socket, cookie: None }));
    e.audio = Arc::clone(&a);
    (e, a)
}

fn post(path: &str) -> Request<Body> {
    Request::builder().method("POST").uri(path).header("host", "127.0.0.1:8766").body(Body::empty()).unwrap()
}

async fn post_json(app: &axum::Router, path: &str) -> (u16, Value) {
    let res = send(app, post(path)).await;
    let s = res.status().as_u16();
    (s, serde_json::from_slice(&body_bytes(res).await).unwrap_or(Value::Null))
}

async fn until(max: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < max {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    f()
}

async fn connected(dir: &std::path::Path, pct: i64) -> (Fake, Arc<Audio>, common::Harness) {
    let (f, path) = fake(dir, pct);
    let (e, a) = extras(path);
    let h = harness_extras(e);
    tokio::spawn(client::run(Arc::clone(&a)));
    assert!(until(Duration::from_secs(2), || a.snapshot().is_some()).await, "bağlanmadı");
    (f, a, h)
}

#[tokio::test]
async fn tp_w24b_a6_a8_state_shape_and_handshake() {
    let d = common::temp_dir();
    let (f, a, h) = connected(&d.0, 25).await;
    let st = get_json(&h.app, "/api/state").await;
    assert_eq!(st["audio"], json!({"volume": 25, "muted": false}));
    let mut errs = Vec::new();
    common::compare(&common::fixture("api_state")["audio"], &st["audio"], "audio", &common::Rules::default(), &mut errs);
    assert!(errs.is_empty(), "{errs:?}");
    // El sıkışma sırası: AUTH (sürüm 32, 256 bayt çerez) → ad → abonelik (sink|server) → sink sorgusu.
    let order: Vec<u32> = f.got.lock().unwrap().iter().map(|(c, _)| *c).collect();
    assert_eq!(&order[..4], &[cmd::AUTH, cmd::SET_CLIENT_NAME, cmd::SUBSCRIBE, cmd::GET_SINK_INFO]);
    let sub = &f.cmds(cmd::SUBSCRIBE)[0];
    let (_, _, mut r) = proto::split(sub).unwrap();
    assert_eq!(r.u32().unwrap(), proto::MASK_SINK | proto::MASK_SERVER);
    let t = get_json(&h.app, "/api/debug/topics").await;
    assert_eq!(t["audio"]["trigger"], "pulse-events");
    assert_eq!(t["audio"]["connected"], true);
    assert_eq!(t["audio"]["queries"], 1);
    assert_eq!(a.connects.load(Relaxed), 1);
}

#[tokio::test]
async fn tp_w24b_a6_external_change_wakes_stream_under_1s() {
    let d = common::temp_dir();
    let (f, _a, h) = connected(&d.0, 25).await;
    let mut body = send(&h.app, get("/api/stream")).await.into_body();
    let _work = body.frame().await.unwrap().unwrap();
    let _state = body.frame().await.unwrap().unwrap();
    let t0 = Instant::now();
    f.external(60, true);
    loop {
        let fr = tokio::time::timeout(Duration::from_secs(1), body.frame()).await.expect("1 sn'de çerçeve yok").unwrap().unwrap();
        let txt = String::from_utf8(fr.into_data().unwrap().to_vec()).unwrap();
        if let Some(j) = txt.strip_prefix("event: state\ndata: ") {
            let v: Value = serde_json::from_str(j.trim_end()).unwrap();
            if v["audio"] == json!({"volume": 60, "muted": true}) {
                break;
            }
        }
        assert!(t0.elapsed() < Duration::from_secs(1), "ses değişimi akışa 1 sn'de gelmedi");
    }
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
}

#[tokio::test]
async fn tp_w24b_a6_volume_endpoint_sets_and_returns() {
    let d = common::temp_dir();
    let (f, a, h) = connected(&d.0, 25).await;
    let (s, v) = post_json(&h.app, "/api/volume?level=40").await;
    assert_eq!((s, v), (200, json!({"volume": 40, "muted": false})));
    let set = &f.cmds(cmd::SET_SINK_VOLUME)[0];
    let (_, _, mut r) = proto::split(set).unwrap();
    assert_eq!((r.u32().unwrap(), r.string().unwrap(), r.cvolume().unwrap()), (7, None, vec![26_214, 26_214]));
    assert_eq!(post_json(&h.app, "/api/volume?op=mute").await, (200, json!({"volume": 40, "muted": true})));
    assert_eq!(post_json(&h.app, "/api/volume?op=up").await, (200, json!({"volume": 45, "muted": false})), "level>0 sessizi kaldırır");
    assert_eq!(post_json(&h.app, "/api/volume?op=down").await.1["volume"], 40);
    assert_eq!(post_json(&h.app, "/api/volume?level=0").await.1, json!({"volume": 0, "muted": false}));
    assert_eq!(post_json(&h.app, "/api/volume?op=yok").await.0, 400);
    assert_eq!(post_json(&h.app, "/api/volume").await.0, 400);
    assert_eq!(post_json(&h.app, "/api/volume?level=abc").await.0, 400);
    assert_eq!(get_json(&h.app, "/api/state").await["audio"], json!({"volume": 0, "muted": false}));
    assert!(a.sets.load(Relaxed) >= 5);
    assert!(f.sink.lock().unwrap().volumes == vec![0, 0]);
}

#[tokio::test]
async fn tp_w24b_a3_a4_server_event_requeries_irrelevant_does_not() {
    let d = common::temp_dir();
    let (f, a, _h) = connected(&d.0, 25).await;
    let q0 = a.queries.load(Relaxed);
    f.push.send(0x10 | 1).unwrap(); // kaynak (mikrofon) değişimi
    f.push.send(0x10 | 2).unwrap(); // sink-input (akış)
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.queries.load(Relaxed), q0, "ilgisiz olay yeniden sorgu doğurmamalı");
    *f.sink.lock().unwrap() = Sink { volumes: vec![proto::from_percent(70); 2], mute: false };
    f.push.send(0x10 | proto::FACILITY_SERVER).unwrap(); // varsayılan sink değişti
    assert!(until(Duration::from_secs(1), || a.snapshot().map(|s| s.volume) == Some(70)).await);
    assert_eq!(a.queries.load(Relaxed), q0 + 1);
    assert_eq!(f.cmds(cmd::GET_SINK_INFO).len() as u64, q0 + 1);
}

#[tokio::test]
async fn tp_w24b_a7_no_socket_null_400_and_slow_retry() {
    let d = common::temp_dir();
    let (e, a) = extras(d.0.join("yok"));
    let h = harness_extras(e);
    tokio::spawn(client::run(Arc::clone(&a)));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(a.connects.load(Relaxed) <= 1, "sıcak döngü: {} deneme / 2 sn", a.connects.load(Relaxed));
    assert!(get_json(&h.app, "/api/state").await["audio"].is_null());
    assert_eq!(post_json(&h.app, "/api/volume?level=10").await.0, 400);
    let t = get_json(&h.app, "/api/debug/topics").await;
    assert_eq!(t["audio"]["connected"], false);
    assert!(!t["audio"]["error"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn tp_w24b_a7_server_gone_goes_null() {
    let d = common::temp_dir();
    let (f, a, _h) = connected(&d.0, 25).await;
    assert_eq!(f.accepts.load(Relaxed), 1);
    // Sunucu bağlantıyı kapatsın: dinleyici tek bağlantı kabul eder, kayıt düşürülünce görev de biter → soket kapanır.
    let Fake { push, .. } = f;
    drop(push);
    assert!(until(Duration::from_secs(2), || a.snapshot().is_none()).await, "kopunca audio null olmalı");
    assert!(!a.connected.load(Relaxed));
}
