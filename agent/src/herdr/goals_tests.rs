//! TP-AP-24..27 — Goals yarısı: herdr ajanlarının Claude `/goal` hedefleri (kullanıcı 2026-11-10: "goals te herdr da 4 agent var,
//! ekranda hic oyk neden ?"). Sınıflama herdr-web `src/goal-history.ts` `goalEntry` ile birebir.
use super::goals::*;
use std::io::Write as _;

fn att(met: bool, extra: &str) -> String {
    format!(
        r#"{{"type":"attachment","timestamp":"2026-11-10T20:00:00.000Z","attachment":{{"type":"goal_status","met":{met},"condition":"durma !"{extra}}}}}"#
    )
}

/// TP-AP-24: dört biçim + temizleme komutu + gevezelik (aynı sözcüğü anan metin) yok sayılır.
#[test]
fn tp_ap_24_entry_classification() {
    assert_eq!(entry(&att(false, r#","sentinel":true"#)).map(|e| e.kind), Some(Kind::Set));
    assert_eq!(entry(&att(false, r#","reason":"r""#)).map(|e| e.kind), Some(Kind::Pending));
    assert_eq!(entry(&att(true, r#","reason":"ok","iterations":3"#)).map(|e| e.kind), Some(Kind::Met));
    assert_eq!(entry(&att(false, r#","failed":true"#)).map(|e| e.kind), Some(Kind::Failed));
    assert_eq!(entry(&att(true, r#","sentinel":true"#)).map(|e| e.kind), Some(Kind::Clear), "met+sentinel = temizleme");
    let cmd = r#"{"type":"system","subtype":"local_command","commandRun":{"command":"goal","args":" Clear "}}"#;
    assert_eq!(entry(cmd).map(|e| e.kind), Some(Kind::Clear));
    let other = r#"{"type":"system","subtype":"local_command","commandRun":{"command":"goal","args":"yeni hedef"}}"#;
    assert!(entry(other).is_none());
    assert!(entry(r#"{"type":"user","message":"\"goal_status\" diye bir şey"}"#).is_none());
    assert!(entry("bozuk {").is_none());
}

/// TP-AP-25: durum makinesi — set/pending → active, met/failed kapanır, clear siler.
#[test]
fn tp_ap_25_apply_sequence() {
    let mut st = None;
    for l in [att(false, r#","sentinel":true"#), att(false, r#","reason":"daha var""#)] {
        st = apply(st, entry(&l));
    }
    let s = st.clone().unwrap_or_default();
    assert_eq!((s.state, s.condition.as_str(), s.reason.as_deref()), (State::Active, "durma !", Some("daha var")));
    st = apply(st, entry(&att(true, r#","reason":"bitti""#)));
    assert_eq!(st.clone().map(|s| s.state), Some(State::Met));
    st = apply(st, entry(&att(true, r#","sentinel":true"#)));
    assert!(st.is_none());
}

/// TP-AP-26: okuyucu — ilk okumada yalnız kuyruk (bütçe), sonra yalnız büyüyen kısım; yarım satır beklenir; dosya kısalırsa baştan.
#[test]
fn tp_ap_26_reader_tail_then_growth() {
    let dir = std::env::temp_dir().join(format!("phoned-goals-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join("s.jsonl");
    let mut f = std::fs::File::create(&p).unwrap_or_else(|_| unreachable!());
    let filler = format!("{}\n", "x".repeat(200));
    for _ in 0..200 {
        let _ = f.write_all(filler.as_bytes());
    }
    let _ = writeln!(f, "{}", att(false, r#","sentinel":true"#));
    let mut r = Reader::with_budget(1_000);
    assert_eq!(r.read(&p).map(|s| s.state), Some(State::Active));
    assert!(r.bytes_read() <= 1_000, "ilk okuma bütçeyi aşmaz: {}", r.bytes_read());
    let before = r.bytes_read();
    let line = att(true, r#","reason":"tamam""#);
    let (a, b) = line.split_at(20);
    let _ = f.write_all(a.as_bytes());
    assert_eq!(r.read(&p).map(|s| s.state), Some(State::Active), "yarım satır sayılmaz");
    let _ = writeln!(f, "{b}");
    assert_eq!(r.read(&p).map(|s| s.state), Some(State::Met));
    assert!(r.bytes_read() - before < 1_000, "yalnız büyüyen kısım okunur");
    drop(f);
    let _ = std::fs::write(&p, format!("{}\n", att(false, r#","sentinel":true"#)));
    assert_eq!(r.read(&p).map(|s| s.state), Some(State::Active), "kısalan dosya baştan okunur");
    let _ = std::fs::remove_dir_all(&dir);
}

/// TP-AP-27: görünüm — yalnız süren hedefler, ajan adıyla; oturumu olmayan / hedefsiz ajan girmez.
#[test]
fn tp_ap_27_goals_view_active_only() {
    let dir = std::env::temp_dir().join(format!("phoned-goals27-{}", std::process::id()));
    let proj = dir.join("projects").join("-home-u");
    let _ = std::fs::create_dir_all(&proj);
    let _ = std::fs::write(proj.join("aaa.jsonl"), format!("{}\n", att(false, r#","sentinel":true"#)));
    let _ = std::fs::write(proj.join("bbb.jsonl"), format!("{}\n", att(true, r#","reason":"x""#)));
    let ag = |pane: &str, sid: Option<&str>| {
        let mut a = serde_json::json!({"agent":"claude","pane_id":pane,"agent_status":"working","terminal_title_stripped":format!("ad {pane}")});
        if let Some(s) = sid {
            a["agent_session"] = serde_json::json!({"kind":"id","value":s});
        }
        a
    };
    let agents = vec![ag("p1", Some("aaa")), ag("p2", Some("bbb")), ag("p3", None), ag("p4", Some("yok"))];
    let mut g = Goals::new(dir.join("projects"));
    let v = g.view(&agents);
    assert_eq!(v, serde_json::json!([{"pane":"p1","name":"ad p1","condition":"durma !","state":"active","reason":null}]));
    let _ = std::fs::remove_dir_all(&dir);
}
