#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! W2.5 hazırlık: medya değişimi sessiz kipte akışı uyandırır (TP-W25-M1/M2) · gölge portta adb izleyicisi
//! varsayılan kapalı (TP-W25-A1).

mod common;

use common::{get, harness_extras, send};
use http_body_util::BodyExt as _;
use phoned::app::{Args, Extras};
use phoned::media::{Media, PlayerProps};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn extras(media: Arc<Media>) -> Extras {
    let mut e = Extras::linux();
    e.media = media;
    e.audio = Arc::new(phoned::audio::Audio::new(phoned::audio::Settings { socket: "/yok/pulse/native".into(), cookie: None }));
    e
}

fn playing(title: &str) -> PlayerProps {
    PlayerProps { status: "Playing".into(), title: title.into(), length_us: 200_000_000, ..PlayerProps::default() }
}

/// TP-W25-M1 — ne: sessiz kipte (oturum kilitli) MPRIS oyuncusu değişir · beklenen: ≤ 1 sn içinde `state` çerçevesi,
/// `media.title` yeni parça · neden: sessiz kipte örnekleyici değişmeyen `pc` için 30 sn tavanına kadar `state` yollamaz;
/// ses (W2.4b) gibi medya da kendi tikiyle uyandırmalı (K4 açığı).
#[tokio::test]
async fn tp_w25_m1_media_change_wakes_quiet_stream_under_1s() {
    let media = Arc::new(Media::new());
    let h = harness_extras(extras(Arc::clone(&media)));
    h.locked.send(true).unwrap();
    let mut body = send(&h.app, get("/api/stream")).await.into_body();
    let _work = body.frame().await.unwrap().unwrap();
    let _state = body.frame().await.unwrap().unwrap();
    let t0 = Instant::now();
    media.put("org.mpris.MediaPlayer2.spotify", playing("Yeni Parça"));
    loop {
        let fr = tokio::time::timeout(Duration::from_secs(1), body.frame()).await.expect("1 sn'de çerçeve yok").unwrap().unwrap();
        let txt = String::from_utf8(fr.into_data().unwrap().to_vec()).unwrap();
        if let Some(j) = txt.strip_prefix("event: state\ndata: ") {
            let v: Value = serde_json::from_str(j.trim_end()).unwrap();
            if v["media"]["title"] == "Yeni Parça" {
                break;
            }
        }
        assert!(t0.elapsed() < Duration::from_secs(1), "medya değişimi sessiz akışa 1 sn'de gelmedi");
    }
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
}

/// TP-W25-M2 — ne: aynı özellikler yeniden yazılır / olmayan oyuncu kaldırılır · beklenen: tik artmaz; gerçek değişim
/// ve var olan oyuncunun kaldırılması tiki artırır · neden: konum gibi sürekli değişen alan tik üretmemeli (yoklamaya dönmez).
#[test]
fn tp_w25_m2_media_ticks_only_on_change() {
    let m = Media::new();
    let rx = m.watch();
    let t = || *rx.borrow();
    m.put("org.mpris.MediaPlayer2.a", playing("x"));
    assert_eq!(t(), 1, "yeni oyuncu");
    m.put("org.mpris.MediaPlayer2.a", playing("x"));
    assert_eq!(t(), 1, "aynı özellik → tik yok");
    m.put("org.mpris.MediaPlayer2.a", playing("y"));
    assert_eq!(t(), 2, "parça değişti");
    m.remove("org.mpris.MediaPlayer2.yok");
    assert_eq!(t(), 2, "olmayan oyuncu → tik yok");
    m.remove("org.mpris.MediaPlayer2.a");
    assert_eq!(t(), 3, "oyuncu kapandı");
}

fn parse(args: &[&str]) -> Args {
    Args::parse(args.iter().map(|s| (*s).to_owned())).unwrap()
}

/// TP-W25-A1 — ne: `--port` 8765 değilse adb izleyicisi varsayılan kapalı · beklenen: 18766/varsayılan → `None`,
/// 8765 → varsayılan adb adresi, açık `--adb` her portta geçerli · neden: K4'te gölge 18766 telefona
/// `reverse` + WAKEUP + `am start` yazdı; telefon yalnız üretim portunu (8765) tanır.
#[test]
fn tp_w25_a1_adb_off_unless_production_port() {
    assert_eq!(parse(&["--port", "18766"]).adb, None, "gölge portu");
    assert_eq!(parse(&[]).adb, None, "varsayılan port 8766");
    assert_eq!(parse(&["--port", "8765"]).adb.as_deref(), Some(phoned::adb::DEFAULT_ADDR), "üretim portu");
    assert_eq!(parse(&["--adb", "127.0.0.1:5037", "--port", "18766"]).adb.as_deref(), Some("127.0.0.1:5037"), "açık istek");
    assert_eq!(parse(&["--port", "8765", "--adb", "off"]).adb, None, "üretimde açık kapatma");
}

/// TP-W25-S1 — ne: ayar sayfasının okuduğu kapsam dışı uçlar (`lan`, `update`, `claude/hook`, `phones`, `charge`) ·
/// beklenen: 200 + 0.6.9 şekli; `lan.configured`/`port` ve `phones` ayardan, `active`/`installed` false (özellik yok) ·
/// neden: `settings.html` `loadInfo()` tek `try` içinde `/api/lan` → `/api/update` → `/api/claude/hook` okur; biri 404 olunca
/// sürüm / ayar dosyası / günlük satırları hiç çizilmiyordu (K5 eşlik denetimi).
#[tokio::test]
async fn tp_w25_s1_settings_page_reads_answer_honestly() {
    let h = common::harness_full(
        |c| {
            c["lan"]["enabled"] = serde_json::json!(true);
            c["lan"]["port"] = serde_json::json!(9443);
            c["phones"] = serde_json::json!([{"id": "p1", "name": "Masa", "pages": ["home"]}]);
        },
        |l| l,
        Extras::linux(),
    );
    let lan = common::get_json(&h.app, "/api/lan").await;
    assert_eq!(lan["configured"], true);
    assert_eq!(lan["active"], false, "Wi-Fi dinleyici yok");
    assert_eq!(lan["port"], 9443);
    assert_eq!(lan["hosts"], serde_json::json!([]));
    let upd = common::get_json(&h.app, "/api/update").await;
    assert_eq!(upd["status"]["state"], "idle");
    assert_eq!(upd["status"]["agentUpdate"], false);
    assert!(upd["status"]["agentVersion"].as_str().unwrap().ends_with("-phoned"));
    let hook = common::get_json(&h.app, "/api/claude/hook").await;
    assert_eq!(hook, serde_json::json!({"installed": false, "path": ""}), "kanca kurulmaz (PRD dışı)");
    let phones = common::get_json(&h.app, "/api/phones").await;
    assert_eq!(phones, serde_json::json!([{"id": "p1", "model": null, "lastSeen": null, "via": null, "name": "Masa", "pages": ["home"]}]));
    let ch = common::get_json(&h.app, "/api/charge").await;
    assert_eq!(ch, serde_json::json!({"phone": null, "power": null, "switchedAt": null, "error": null}));
    assert_eq!(common::get_json(&h.app, "/agent/api/lan").await, lan, "/agent öneki");
}
