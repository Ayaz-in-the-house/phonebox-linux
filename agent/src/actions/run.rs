//! `run/{id}` kısayolu: hangi yol (saf karar) + sıradan komutun başlatılması.
//!
//! `herdr-go-N` (W1 yuva kısayolu) ajanın içinde çözülür: süreç başlatılmaz, yuva → pane → Zen soketi (TP-W2-09).
//! Sıradan kısayol 0.6.9 `Shortcuts.Execute` gibi komutu başlatır (kullanıcı eylemi; kararlı hâl fork sayımına girmez).
//! Bu dilimde yok: `builtin:keys/text/macro/mute/monitor-off` (W2.4 ses/klavye) → açık hata döner, sessizce yutulmaz.

use serde_json::Value;
use std::path::Path;

/// Kısayolun gideceği yol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ajanın içinde: herdr yuvası → Zen.
    HerdrGo(u32),
    /// Komut başlat.
    Spawn { program: String, args: Vec<String>, cwd: Option<String> },
    /// Desteklenmeyen yerleşik.
    Unsupported(String),
}

fn sget<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// `herdr-go-N` kimliği ya da köprü komutu (`…/herdr-phonebox-bridge go N`) → N.
#[must_use]
pub fn herdr_slot(sc: &Value) -> Option<u32> {
    let from_args = || {
        let cmd = sget(sc, "cmd")?;
        if Path::new(cmd).file_name()?.to_str()? != "herdr-phonebox-bridge" {
            return None;
        }
        let mut a = sget(sc, "args")?.split_whitespace();
        match (a.next(), a.next(), a.next()) {
            (Some("go"), Some(n), None) => n.parse().ok(),
            _ => None,
        }
    };
    from_args().or_else(|| sget(sc, "id")?.strip_prefix("herdr-go-")?.parse().ok()).filter(|n| (1..=crate::herdr::NSLOTS).contains(n))
}

/// `~` → ev dizini (0.6.9 `Home`: baştaki `~`/`~/` ve argümandaki ` ~/`).
#[must_use]
pub fn home(s: &str, home: &str) -> String {
    if s == "~" {
        return home.to_owned();
    }
    let s = s.strip_prefix("~/").map_or_else(|| s.to_owned(), |r| format!("{home}/{r}"));
    s.replace(" ~/", &format!(" {home}/"))
}

/// Argüman dizgesi → argümanlar (boşlukla ayrılır; çift/tek tırnak gruplar, `\"` kaçış).
#[must_use]
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Kısayol nesnesi → yol.
#[must_use]
pub fn plan(sc: &Value, home_dir: &str) -> Action {
    if let Some(n) = herdr_slot(sc) {
        return Action::HerdrGo(n);
    }
    let cmd = sget(sc, "cmd").unwrap_or("");
    let args = sget(sc, "args").map(|a| split_args(&home(a, home_dir))).unwrap_or_default();
    let cwd = sget(sc, "cwd").map(|c| home(c, home_dir));
    match cmd {
        "builtin:lock" => Action::Spawn { program: "loginctl".into(), args: vec!["lock-session".into()], cwd: None },
        b if b.starts_with("builtin:") => Action::Unsupported(b.to_owned()),
        url if url.contains("://") => Action::Spawn { program: "xdg-open".into(), args: vec![url.to_owned()], cwd },
        p => Action::Spawn { program: home(p, home_dir), args, cwd },
    }
}

/// Komutu başlat ve arka planda topla (zombi kalmaz).
///
/// # Errors
/// Program bulunamadı / başlatılamadı.
pub fn spawn(program: &str, args: &[String], cwd: Option<&str>) -> Result<(), String> {
    let mut c = std::process::Command::new(program);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let mut child = c.spawn().map_err(|e| format!("{program}: {e}"))?;
    tokio::task::spawn_blocking(move || child.wait());
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tp_w2_p5_herdr_slot_detection() {
        assert_eq!(herdr_slot(&json!({"id":"herdr-go-3","cmd":"/home/u/.local/bin/herdr-phonebox-bridge","args":"go 3"})), Some(3));
        assert_eq!(herdr_slot(&json!({"id":"x","cmd":"herdr-phonebox-bridge","args":"go 5"})), Some(5), "komuttan da tanınır");
        assert_eq!(herdr_slot(&json!({"id":"herdr-go-7","cmd":"other"})), Some(7));
        assert_eq!(herdr_slot(&json!({"id":"herdr-go-17"})), None, "16 üstü yuva yok");
        assert_eq!(herdr_slot(&json!({"id":"terminal","cmd":"x-terminal-emulator"})), None);
        assert_eq!(plan(&json!({"id":"herdr-go-2"}), "/h"), Action::HerdrGo(2));
    }

    #[test]
    fn tp_w2_p5_plan_paths() {
        assert_eq!(
            plan(&json!({"cmd":"xdg-open","args":"~"}), "/h"),
            Action::Spawn { program: "xdg-open".into(), args: vec!["/h".into()], cwd: None }
        );
        assert_eq!(
            plan(&json!({"cmd":"https://github.com"}), "/h"),
            Action::Spawn { program: "xdg-open".into(), args: vec!["https://github.com".into()], cwd: None }
        );
        assert_eq!(
            plan(&json!({"cmd":"~/bin/x","args":"-a \"iki kelime\" ~/f 'tek'","cwd":"~/w"}), "/h"),
            Action::Spawn {
                program: "/h/bin/x".into(),
                args: vec!["-a".into(), "iki kelime".into(), "/h/f".into(), "tek".into()],
                cwd: Some("/h/w".into())
            }
        );
        assert_eq!(plan(&json!({"cmd":"builtin:mute"}), "/h"), Action::Unsupported("builtin:mute".into()));
        assert!(matches!(plan(&json!({"cmd":"builtin:lock"}), "/h"), Action::Spawn { .. }));
        assert_eq!(split_args(r#"a "" "b\"c""#), ["a", "", "b\"c"]);
    }
}
