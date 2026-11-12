//! Goals yarısı (TP-AP-24..27): herdr ajanlarının Claude `/goal` hedefleri → `push["herdr"].goals[]`.
//!
//! Kaynak: Claude Code her hedef olayını oturum kaydına `goal_status` eki olarak yazar (herdr-web PRD-191/340,
//! `src/goal-history.ts` — sınıflama birebir buradan): set `{met:false, sentinel:true}` · check `{met:false, reason}` ·
//! met `{met:true}` · failed `{failed:true}` · cleared `{met:true, sentinel:true}` (bitiş DEĞİL) + `/goal clear` komut satırı.
//! Kayıt yüzlerce MB olabilir: ilk karşılaşmada yalnız son `budget` bayt, sonra yalnız büyüyen kısım okunur (saat yok).
//! Oturum kimliği herdr `agent_session.value`; kayıt `<projects>/<herhangi>/<kimlik>.jsonl`.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

const STATUS_MARK: &str = "\"goal_status\"";
const COMMAND_MARK: &str = "\"commandRun\"";
/// `/goal` ardından vazgeçme sözcükleri (Claude ikilisinin kendi kümesi, herdr-web `CLEARS`).
const CLEARS: [&str; 6] = ["clear", "off", "reset", "cancel", "none", "stop"];
/// İlk okuma bütçesi (herdr-web HW-GOAL-02: 16 MB).
pub const BUDGET: u64 = 16 << 20;
const CHUNK: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Set,
    Pending,
    Met,
    Failed,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub condition: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Active,
    Met,
    Failed,
}

impl State {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Met => "met",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Goal {
    pub state: State,
    pub condition: String,
    pub reason: Option<String>,
}

/// Bir kayıt satırı → hedef olayı (değilse `None`). Ucuz ön süzgeç: işaret yoksa JSON çözülmez.
#[must_use]
pub fn entry(line: &str) -> Option<Entry> {
    if !line.contains(STATUS_MARK) && !line.contains(COMMAND_MARK) {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let ty = v.get("type").and_then(Value::as_str);
    if ty == Some("system") && v.get("subtype").and_then(Value::as_str) == Some("local_command") {
        let cmd = v.get("commandRun")?;
        if cmd.get("command").and_then(Value::as_str) != Some("goal") {
            return None;
        }
        let args = cmd.get("args").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
        return CLEARS.contains(&args.as_str()).then(|| Entry { kind: Kind::Clear, condition: String::new(), reason: None });
    }
    if ty != Some("attachment") {
        return None;
    }
    let b = v.get("attachment")?;
    if b.get("type").and_then(Value::as_str) != Some("goal_status") {
        return None;
    }
    let condition = b.get("condition").and_then(Value::as_str)?.to_owned();
    let met = b.get("met").and_then(Value::as_bool)?;
    let sentinel = b.get("sentinel").and_then(Value::as_bool) == Some(true);
    let failed = b.get("failed").and_then(Value::as_bool) == Some(true);
    // `met + sentinel` bir temizlemedir, bitiş değil (herdr-web ADR-531, HW-GOAL-13).
    let kind = match (met, sentinel, failed) {
        (true, true, _) => Kind::Clear,
        (true, false, _) => Kind::Met,
        (false, _, true) => Kind::Failed,
        (false, true, false) => Kind::Set,
        (false, false, false) => Kind::Pending,
    };
    Some(Entry { kind, condition, reason: b.get("reason").and_then(Value::as_str).map(str::to_owned) })
}

/// Yazıldıkları sırayla bir olay daha.
#[must_use]
pub fn apply(state: Option<Goal>, e: Option<Entry>) -> Option<Goal> {
    let Some(e) = e else { return state };
    let st = match e.kind {
        Kind::Clear => return None,
        // Aynı hedef yeniden eklendi (sıkıştırma/devam): sürer, eski gerekçe kalır.
        Kind::Set if state.as_ref().is_some_and(|s| s.state == State::Active && s.condition == e.condition) => return state,
        Kind::Set | Kind::Pending => State::Active,
        Kind::Met => State::Met,
        Kind::Failed => State::Failed,
    };
    Some(Goal { state: st, condition: e.condition, reason: e.reason })
}

#[derive(Debug)]
struct Known {
    size: u64,
    /// Bu bayta kadar tam satırlar okundu.
    offset: u64,
    state: Option<Goal>,
}

/// Kayıt okuyucu: dosya başına son okunan konum + durum.
#[derive(Debug)]
pub struct Reader {
    budget: u64,
    known: HashMap<PathBuf, Known>,
    bytes: u64,
}

impl Default for Reader {
    fn default() -> Self {
        Self::with_budget(BUDGET)
    }
}

impl Reader {
    #[must_use]
    pub fn with_budget(budget: u64) -> Self {
        Self { budget, known: HashMap::new(), bytes: 0 }
    }

    /// Şimdiye dek okunan bayt (tanı + test).
    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes
    }

    pub fn forget(&mut self, path: &Path) {
        self.known.remove(path);
    }

    /// Kaydın sonundaki hedef; yoksa / okunamazsa `None`.
    pub fn read(&mut self, path: &Path) -> Option<Goal> {
        let Ok(meta) = std::fs::metadata(path) else {
            self.known.remove(path);
            return None;
        };
        let size = meta.len();
        let (from, fresh, state) = match self.known.get(path) {
            Some(k) if size >= k.size => (k.offset, false, k.state.clone()),
            _ => (size.saturating_sub(self.budget), true, None),
        };
        if !fresh && size == from {
            return state;
        }
        let Ok(mut f) = File::open(path) else { return state };
        if f.seek(SeekFrom::Start(from)).is_err() {
            return state;
        }
        let mut state = state;
        let mut carry: Vec<u8> = Vec::new();
        // Bütçenin ortasından başlandıysa ilk (yarım) satır atılır.
        let mut skip_first = fresh && from > 0;
        let mut pos = from;
        let mut offset = from;
        let mut buf = vec![0u8; CHUNK];
        while pos < size {
            let want = usize::try_from((size - pos).min(CHUNK as u64)).unwrap_or(CHUNK);
            let Some(dst) = buf.get_mut(..want) else { break };
            let Ok(n) = f.read(dst) else { break };
            if n == 0 {
                break;
            }
            self.bytes += n as u64;
            pos += n as u64;
            carry.extend_from_slice(buf.get(..n).unwrap_or_default());
            let Some(last_nl) = carry.iter().rposition(|b| *b == b'\n') else { continue };
            for line in carry.get(..last_nl).unwrap_or_default().split(|b| *b == b'\n') {
                if skip_first {
                    skip_first = false;
                    continue;
                }
                if let Ok(s) = std::str::from_utf8(line) {
                    state = apply(state, entry(s));
                }
            }
            skip_first = false;
            offset = pos - (carry.len() - last_nl - 1) as u64;
            carry.drain(..=last_nl);
        }
        self.known.insert(path.to_owned(), Known { size, offset, state: state.clone() });
        state
    }
}

/// herdr ajan listesi → süren hedefler (Goals yarısı). Oturum kaydı yolları önbellekte.
#[derive(Debug)]
pub struct Goals {
    projects: PathBuf,
    paths: HashMap<String, PathBuf>,
    reader: Reader,
}

impl Goals {
    #[must_use]
    pub fn new(projects: PathBuf) -> Self {
        Self { projects, paths: HashMap::new(), reader: Reader::default() }
    }

    /// `~/.claude/projects` (yoksa `$HOME` boş → hiçbir kayıt bulunmaz).
    #[must_use]
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        Self::new(home.join(".claude").join("projects"))
    }

    fn locate(&mut self, sid: &str) -> Option<PathBuf> {
        if let Some(p) = self.paths.get(sid).filter(|p| p.is_file()) {
            return Some(p.clone());
        }
        let name = format!("{sid}.jsonl");
        let found = std::fs::read_dir(&self.projects).ok()?.flatten().map(|d| d.path().join(&name)).find(|p| p.is_file())?;
        self.paths.insert(sid.to_owned(), found.clone());
        Some(found)
    }

    /// Yalnız süren (`active`) hedefler, herdr sırasıyla: `{pane, name, condition, state, reason}`.
    pub fn view(&mut self, agents: &[Value]) -> Value {
        let mut out = Vec::new();
        let mut live = Vec::new();
        for a in agents {
            let sess = a.get("agent_session");
            let Some(sid) = sess.filter(|s| s.get("kind").and_then(Value::as_str) == Some("id")).and_then(|s| s.get("value")).and_then(Value::as_str)
            else {
                continue;
            };
            // Kimlik dosya adına girer: yol ayırıcısı / üst dizin taşıyan kimlik reddedilir.
            if sid.is_empty() || sid.contains(['/', '\\']) || sid.contains("..") {
                continue;
            }
            let Some(path) = self.locate(sid) else { continue };
            live.push(path.clone());
            if let Some(g) = self.reader.read(&path).filter(|g| g.state == State::Active) {
                out.push(json!({
                    "pane": super::pane_of(a).unwrap_or(""),
                    "name": super::name_of(a),
                    "condition": g.condition,
                    "state": g.state.as_str(),
                    "reason": g.reason,
                }));
            }
        }
        let gone: Vec<PathBuf> = self.reader.known.keys().filter(|p| !live.contains(p)).cloned().collect();
        for p in gone {
            self.reader.forget(&p);
        }
        Value::Array(out)
    }
}
