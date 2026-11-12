//! herdr ajan durumu → panel `push["herdr"]` (W1 köprüsünün ajana taşınmış hâli; `src/herdr-phonebox-bridge` ile aynı yük).
//!
//! Kaynak: herdr sunucu soketi, tek satır `agent.list` (olay aboneliği değil: `pane.agent_status_changed` herdr sunucu
//! CPU'sunu ~2× yapar, herdr-web ADR-10). Yoklama yalnız biri bakarken (kapı çağıranın). Saf parçalar: `parse`, `name_of`,
//! `Slots`, `Tracker::observe` (görünüm + yeni blocked) — G/Ç yalnız `list`.

pub mod goals;

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

/// Yoklama aralığı (bakılırken).
pub const PERIOD_MS: u64 = 1_500;
pub const NSLOTS: u32 = 16;
const ORDER: [&str; 4] = ["blocked", "working", "idle", "done"];
const SPIN: &str = "◐◑◒◓⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏✳·*• ";

/// `$HERDR_SOCKET_PATH`, yoksa `~/.config/herdr/herdr.sock` (deckd `agents::sock_path` ile aynı).
#[must_use]
pub fn sock_path() -> PathBuf {
    if let Some(p) = std::env::var_os("HERDR_SOCKET_PATH") {
        return PathBuf::from(p);
    }
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default();
    cfg.join("herdr").join("herdr.sock")
}

fn label(status: &str) -> (&str, &'static str) {
    match status {
        "blocked" => ("onay bekliyor", "error"),
        "working" => ("çalışıyor", "warn"),
        "idle" => ("boşta", "info"),
        "done" => ("bitti", "ok"),
        other => (other, "info"),
    }
}

/// `agent.list` yanıtı → ajan nesneleri (herdr sırası).
///
/// # Errors
/// JSON değil ya da liste yok.
pub fn parse(line: &str) -> Result<Vec<Value>, String> {
    let msg: Value = serde_json::from_str(line).map_err(|e| format!("herdr yanıtı JSON değil: {e}"))?;
    if let Some(e) = msg.get("error") {
        return Err(format!("herdr: {e}"));
    }
    let r = msg.get("result").unwrap_or(&msg);
    let list = if r.is_array() { Some(r) } else { r.get("agents") };
    list.and_then(Value::as_array).cloned().ok_or_else(|| "herdr yanıtında ajan listesi yok".to_owned())
}

fn s<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

#[must_use]
pub fn pane_of(a: &Value) -> Option<&str> {
    s(a, "pane_id")
}

/// Görünen ad: başlıktaki dönen imler kırpılır, ≤ 60 karakter.
#[must_use]
pub fn name_of(a: &Value) -> String {
    let n = s(a, "terminal_title_stripped").or_else(|| s(a, "terminal_title")).or_else(|| s(a, "name")).or_else(|| s(a, "pane_id")).unwrap_or("?");
    n.trim_start_matches(|c| SPIN.contains(c)).trim().chars().take(60).collect()
}

/// pane → kalıcı yuva (1..=16). Pane yaşadıkça yuvası değişmez; boşalan yuva en küçük olarak yeniden verilir.
#[derive(Debug, Default)]
pub struct Slots {
    map: HashMap<String, Option<u32>>,
}

impl Slots {
    pub fn assign(&mut self, panes: &[String]) -> HashMap<String, Option<u32>> {
        let live: HashSet<&String> = panes.iter().collect();
        self.map.retain(|p, s| live.contains(p) && s.is_some());
        let used: HashSet<u32> = self.map.values().flatten().copied().collect();
        let mut free = (1..=NSLOTS).filter(|i| !used.contains(i));
        for p in panes {
            if !self.map.contains_key(p) {
                self.map.insert(p.clone(), free.next());
            }
        }
        panes.iter().map(|p| (p.clone(), self.map.get(p).copied().flatten())).collect()
    }

    #[must_use]
    pub fn pane_for(&self, slot: u32) -> Option<&str> {
        self.map.iter().find(|(_, s)| **s == Some(slot)).map(|(p, _)| p.as_str())
    }
}

/// Bir gözlemin sonucu.
#[derive(Debug, Default, PartialEq)]
pub struct Change {
    /// Görünüm değiştiyse yeni yük (`push["herdr"]`).
    pub view: Option<Value>,
    /// Yeni onay bekleyen ajanlar için bildirim metinleri.
    pub notify: Vec<String>,
}

/// Durum izleyicisi: yuvalar, son durum değişim zamanı, son gönderilen görünüm, önceki blocked kümesi.
#[derive(Debug, Default)]
pub struct Tracker {
    pub slots: Slots,
    status: HashMap<String, String>,
    since: HashMap<String, f64>,
    last: Option<String>,
    blocked_prev: HashSet<String>,
    /// Son Zen'de açılan pane (TP-AP-17): panelin "şu an hangi ajandasın" vurgusu. Pane ölünce düşer.
    active: Option<String>,
    /// Son gözlenen liste: `set_active` görünümü herdr'ı yeniden yoklamadan üretsin.
    agents: Vec<Value>,
    /// Son rotanın Zen onayı (unix ms) + phoned'daki tur süresi (TP-AP-21/22): sayfanın "açıldı" aşaması.
    routed: Option<(i64, u64)>,
    /// Goals yarısı (TP-AP-27): süren `/goal` hedefleri; yoklayıcı `set_goals` ile verir.
    goals: Value,
}

impl Tracker {
    /// `now` = Unix saniye (köprüyle aynı birim: `since` kayan nokta saniye).
    pub fn observe(&mut self, agents: &[Value], now: f64) -> Change {
        let panes: Vec<String> = agents.iter().filter_map(pane_of).map(str::to_owned).collect();
        for a in agents {
            if let Some(p) = pane_of(a) {
                let st = s(a, "agent_status").unwrap_or("unknown");
                if self.status.get(p).map(String::as_str) != Some(st) {
                    self.status.insert(p.to_owned(), st.to_owned());
                    self.since.insert(p.to_owned(), now);
                }
            }
        }
        let live: HashSet<&String> = panes.iter().collect();
        self.status.retain(|p, _| live.contains(p));
        self.since.retain(|p, _| live.contains(p));
        if self.active.as_ref().is_some_and(|p| !live.contains(p)) {
            self.active = None;
        }
        agents.clone_into(&mut self.agents);
        let mut out = Change { view: self.render(), ..Change::default() };
        let blocked: HashSet<String> =
            agents.iter().filter(|a| s(a, "agent_status") == Some("blocked")).filter_map(pane_of).map(str::to_owned).collect();
        for a in agents {
            if pane_of(a).is_some_and(|p| blocked.contains(p) && !self.blocked_prev.contains(p)) {
                out.notify.push(format!("herdr: {} onay bekliyor", name_of(a)));
            }
        }
        self.blocked_prev = blocked;
        out
    }

    /// Görünümü son listeden üret; öncekiyle aynıysa `None` (yalnız değişince gönder).
    fn render(&mut self) -> Option<Value> {
        let mapping = self.slots.assign(&self.agents.iter().filter_map(pane_of).map(str::to_owned).collect::<Vec<_>>());
        let mut v = view(&self.agents, &mapping, &self.since);
        if let Some(o) = v.as_object_mut() {
            o.insert("active".to_owned(), self.active.as_deref().map_or(Value::Null, |p| json!(p)));
            o.insert("routed_at".to_owned(), self.routed.map_or(Value::Null, |(at, _)| json!(at)));
            o.insert("route_ms".to_owned(), self.routed.map_or(Value::Null, |(_, ms)| json!(ms)));
            o.insert("goals".to_owned(), if self.goals.is_array() { self.goals.clone() } else { json!([]) });
        }
        let enc = v.to_string();
        if self.last.as_deref() == Some(enc.as_str()) {
            return None;
        }
        self.last = Some(enc);
        Some(v)
    }

    /// Zen bu pane'e yönlendirildi: vurgu ona geçer; görünüm değiştiyse hemen gönderilecek yük döner.
    /// `routed_at` = rotanın Zen onayı (unix ms), `route_ms` = isteğin phoned'daki tur süresi (TP-AP-21/22).
    pub fn set_active(&mut self, pane: &str, routed_at: i64, route_ms: u64) -> Option<Value> {
        if self.active.as_deref() == Some(pane) && self.routed.map(|r| r.0) == Some(routed_at) {
            return None;
        }
        self.active = Some(pane.to_owned());
        self.routed = Some((routed_at, route_ms));
        self.render()
    }

    /// Süren hedefler (sonraki `observe`/`render` görünüme koyar).
    pub fn set_goals(&mut self, goals: Value) {
        self.goals = goals;
    }

    /// Kaynak koptu: sonraki başarılı gözlem görünümü yeniden göndersin (köprü `last = None`).
    pub fn forget_view(&mut self) {
        self.last = None;
    }
}

/// Köprü `view()` eşi: blocked önde, öbürleri herdr sırası (konum ezberi bozulmasın).
#[must_use]
pub fn view(agents: &[Value], slots: &HashMap<String, Option<u32>>, since: &HashMap<String, f64>) -> Value {
    let status = |a: &Value| s(a, "agent_status").unwrap_or("unknown").to_owned();
    let mut rows: Vec<&Value> = agents.iter().collect();
    rows.sort_by_key(|a| status(a) != "blocked");
    let mut counts = serde_json::Map::new();
    for k in ORDER {
        counts.insert(k.to_owned(), json!(0));
    }
    for a in agents {
        let st = status(a);
        let n = counts.get(&st).and_then(Value::as_u64).unwrap_or(0);
        counts.insert(st, json!(n + 1));
    }
    let sub: Vec<String> =
        ORDER.iter().filter_map(|k| counts.get(*k).and_then(Value::as_u64).filter(|n| *n > 0).map(|n| format!("{n} {}", label(k).0))).collect();
    let mut out = Vec::new();
    let mut items = Vec::new();
    for a in rows {
        let p = pane_of(a).unwrap_or("");
        let st = status(a);
        let cwd = s(a, "foreground_cwd").or_else(|| s(a, "cwd")).unwrap_or("").trim_end_matches('/');
        let base = Path::new(cwd).file_name().and_then(|b| b.to_str()).unwrap_or(cwd);
        let name = name_of(a);
        let (value, level) = label(&st);
        items.push(json!({"label": name.chars().take(34).collect::<String>(), "value": value, "level": level}));
        out.push(json!({
            "slot": slots.get(p).copied().flatten(),
            "pane": p,
            "name": name,
            "status": st,
            "since": since.get(p),
            "cwd": base,
            "kind": s(a, "agent").unwrap_or(""),
            "focused": a.get("focused").and_then(Value::as_bool).unwrap_or(false),
        }));
    }
    let sub = if sub.is_empty() { "ajan yok".to_owned() } else { sub.join(" · ") };
    json!({"title": "herdr ajanları", "sub": sub, "items": items, "agents": out, "counts": counts})
}

/// herdr-web rota adresi: `<base>/#/t/<pane>`.
#[must_use]
pub fn route_url(base: &str, pane: &str) -> String {
    format!("{}/#/t/{pane}", base.trim_end_matches('/'))
}

/// Bir `agent.list` isteği (tek satır yanıt, ≤ 1 MB, zaman aşımlı).
///
/// # Errors
/// Soket yok, zaman aşımı, bozuk yanıt.
pub async fn list(sock: &Path, timeout: Duration) -> Result<Vec<Value>, String> {
    let req = async {
        let s = tokio::net::UnixStream::connect(sock).await?;
        let (r, mut w) = s.into_split();
        w.write_all(b"{\"id\":\"phoned\",\"method\":\"agent.list\",\"params\":{}}\n").await?;
        let mut line = String::new();
        tokio::io::BufReader::new(r.take(1 << 20)).read_line(&mut line).await?;
        Ok::<_, std::io::Error>(line)
    };
    match tokio::time::timeout(timeout, req).await {
        Ok(Ok(line)) => parse(&line),
        Ok(Err(e)) => Err(format!("herdr soketi ({}): {e}", sock.display())),
        Err(_) => Err("herdr zaman aşımı".to_owned()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test")]
mod tests {
    use super::*;

    fn ag(pane: &str, st: &str, title: &str) -> Value {
        json!({"agent":"claude","agent_status":st,"focused":false,"pane_id":pane,"terminal_title_stripped":title,"foreground_cwd":"/home/u/projects/phoned/"})
    }

    #[test]
    fn tp_w2_08a_parse_live_shape_and_errors() {
        let line = r#"{"id":"x","result":{"type":"agent_list","agents":[{"pane_id":"wC:p1","agent_status":"idle"}]}}"#;
        assert_eq!(parse(line).unwrap().len(), 1);
        assert!(parse("{").is_err());
        assert!(parse(r#"{"error":{"code":1}}"#).is_err());
        assert!(parse(r#"{"result":{}}"#).is_err());
    }

    #[test]
    fn tp_w2_08a_name_strips_spinners() {
        assert_eq!(name_of(&json!({"terminal_title_stripped": "✳ ⠋ phoned w23  "})), "phoned w23");
        assert_eq!(name_of(&json!({"pane_id": "wC:p9"})), "wC:p9");
        assert_eq!(name_of(&json!({"terminal_title": "x".repeat(80)})).chars().count(), 60);
    }

    #[test]
    fn tp_w2_08a_view_matches_bridge_contract() {
        let agents = vec![ag("p1", "working", "A"), ag("p2", "blocked", "B"), ag("p3", "done", "C"), ag("p4", "idle", "D")];
        let mut t = Tracker::default();
        let v = t.observe(&agents, 100.5).view.unwrap();
        let order: Vec<&str> = v["agents"].as_array().unwrap().iter().map(|a| a["pane"].as_str().unwrap()).collect();
        assert_eq!(order, ["p2", "p1", "p3", "p4"], "blocked önde, gerisi herdr sırası");
        assert_eq!(v["counts"], json!({"blocked":1,"working":1,"idle":1,"done":1}));
        assert_eq!(v["sub"], "1 onay bekliyor · 1 çalışıyor · 1 boşta · 1 bitti");
        assert_eq!(
            v["agents"][0],
            json!({"slot":2,"pane":"p2","name":"B","status":"blocked","since":100.5,"cwd":"phoned","kind":"claude","focused":false})
        );
        assert_eq!(v["items"][0], json!({"label":"B","value":"onay bekliyor","level":"error"}));
        assert_eq!(v["title"], "herdr ajanları");
        let empty = view(&[], &HashMap::new(), &HashMap::new());
        assert_eq!(empty["sub"], "ajan yok");
    }

    #[test]
    fn tp_w2_08a_slots_stable() {
        let mut s = Slots::default();
        let p = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        let m = s.assign(&p(&["a", "b", "c"]));
        assert_eq!((m["a"], m["b"], m["c"]), (Some(1), Some(2), Some(3)));
        let m = s.assign(&p(&["c", "a", "b"]));
        assert_eq!((m["a"], m["b"], m["c"]), (Some(1), Some(2), Some(3)), "sıra değişince yuva aynı");
        let m = s.assign(&p(&["a", "c", "d"]));
        assert_eq!(m["d"], Some(2), "boşalan en küçük yuva");
        assert_eq!(s.pane_for(3), Some("c"));
        let many: Vec<String> = (0..20).map(|i| format!("x{i}")).collect();
        let mut s2 = Slots::default();
        let m = s2.assign(&many);
        assert_eq!(m["x15"], Some(16));
        assert_eq!(m["x16"], None, "16 dolunca yuvasız");
    }

    #[test]
    fn tp_w2_08c_change_only_and_new_blocked_notifies_once() {
        let mut t = Tracker::default();
        let a = vec![ag("p1", "working", "A")];
        assert!(t.observe(&a, 1.0).view.is_some());
        assert_eq!(t.observe(&a, 2.0), Change::default(), "değişim yok → yük yok");
        let b = vec![ag("p1", "blocked", "A")];
        let c = t.observe(&b, 3.0);
        assert!(c.view.is_some());
        assert_eq!(c.notify, ["herdr: A onay bekliyor"]);
        assert!(t.observe(&b, 4.0).notify.is_empty(), "aynı blocked ikinci kez bildirilmez");
        t.forget_view();
        assert!(t.observe(&b, 5.0).view.is_some());
    }

    #[test]
    fn tp_ap_17_active_follows_route_and_dies_with_pane() {
        let mut t = Tracker::default();
        let a = vec![ag("p1", "working", "A"), ag("p2", "idle", "B")];
        let v = t.observe(&a, 1.0).view.unwrap();
        assert_eq!(v["active"], Value::Null, "henüz Zen'de açılan yok");
        let v = t.set_active("p2", 1_000, 12).unwrap();
        assert_eq!(v["active"], "p2", "rota sonrası görünüm yoklamayı beklemeden yeniden üretilir");
        assert!(t.set_active("p2", 1_000, 12).is_none(), "birebir aynı rota → yeniden gönderim yok");
        assert_eq!(t.observe(&a, 2.0), Change::default(), "set_active sonrası aynı liste → değişim yok");
        let v = t.observe(&[ag("p1", "working", "A")], 3.0).view.unwrap();
        assert_eq!(v["active"], Value::Null, "pane ölünce active düşer");
    }

    /// TP-AP-21/22: görünüm `routed_at` + `route_ms` taşır; aynı karta yeniden dokunuş (yeni `routed_at`) yeniden gönderilir.
    #[test]
    fn tp_ap_22_retap_same_pane_resends_with_routed_at() {
        let mut t = Tracker::default();
        let a = vec![ag("p1", "working", "A"), ag("p2", "idle", "B")];
        let v = t.observe(&a, 1.0).view.unwrap();
        assert_eq!(v["routed_at"], Value::Null, "henüz rota yok");
        let v = t.set_active("p1", 1_000, 12).unwrap();
        assert_eq!((v["routed_at"].as_i64(), v["route_ms"].as_u64()), (Some(1_000), Some(12)));
        let v = t.set_active("p1", 2_000, 9).expect("aynı pane yeniden dokunuş → yeniden gönderilir");
        assert_eq!((v["active"].as_str(), v["routed_at"].as_i64()), (Some("p1"), Some(2_000)));
        assert_eq!(t.observe(&a, 2.0), Change::default(), "aynı liste → değişim yok, routed_at korunur");
        t.forget_view();
        assert_eq!(t.observe(&a, 3.0).view.unwrap()["routed_at"].as_i64(), Some(2_000), "yeniden gönderimde routed_at kaybolmaz");
    }

    #[tokio::test]
    async fn tp_w2_08a_list_over_socket() {
        let dir = std::env::temp_dir().join(format!("phoned-herdr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("h.sock");
        let _ = std::fs::remove_file(&sock);
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut r = tokio::io::BufReader::new(s);
            let mut got = String::new();
            r.read_line(&mut got).await.unwrap();
            assert!(got.contains("\"agent.list\""));
            r.get_mut().write_all(b"{\"result\":{\"agents\":[{\"pane_id\":\"p\"}]}}\n").await.unwrap();
        });
        assert_eq!(list(&sock, Duration::from_secs(1)).await.unwrap().len(), 1);
        assert!(list(&dir.join("yok"), Duration::from_secs(1)).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
#[path = "goals_tests.rs"]
mod goals_tests;
