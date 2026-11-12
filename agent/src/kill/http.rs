//! `POST /api/procs/kill?pid=N` | `?name=X` — 0.6.9 sözleşmesi (APK değişmez). İkinci onay = aynı isteğin ≥ 5 sn sonra
//! tekrarı (panel "End" → "Confirm"; hâlâ yaşıyorsa kullanıcı yeniden basar). Ek kapı: eş adresi loopback değilse 403.

use super::{Ctx, Outcome, Policy, Target};
use crate::app::Shared;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{Extensions, StatusCode};
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, serde::Deserialize)]
pub struct KillQuery {
    pub name: Option<String>,
    pub pid: Option<i32>,
}

/// `<ayar dizini>/kill-policy.json` (ayar dosyasının yanında).
#[must_use]
pub fn policy_path(st: &Shared) -> PathBuf {
    st.cfg.path().with_file_name("kill-policy.json")
}

/// Ajanın kendi kimliği.
#[must_use]
pub fn ctx() -> Ctx {
    let uid = std::fs::metadata("/proc/self").map_or(u32::MAX, |m| m.uid());
    Ctx { self_pid: i32::try_from(std::process::id()).unwrap_or(-1), uid }
}

fn json(status: StatusCode, v: &impl serde::Serialize) -> Response {
    (status, [(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")], serde_json::to_string(v).unwrap_or_default()).into_response()
}

/// Eş adresi biliniyorsa loopback olmalı (dinleyici zaten 127.0.0.1; bu, ileride bir dinleyici eklenirse son kapıdır).
#[must_use]
pub fn peer_ok(ext: &Extensions) -> bool {
    ext.get::<ConnectInfo<SocketAddr>>().is_none_or(|c| c.0.ip().is_loopback())
}

pub async fn kill(State(st): State<Shared>, ext: Extensions, Query(q): Query<KillQuery>) -> Response {
    let target = match (q.pid, q.name) {
        (Some(pid), _) => Target::Pid(pid),
        (None, Some(n)) => Target::Name(n),
        (None, None) => Target::Name(String::new()),
    };
    if !peer_ok(&ext) {
        eprintln!("phoned: kill {target} → reddedildi neden=yerel olmayan eş");
        st.extras.killer.counters.denied.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return json(
            StatusCode::FORBIDDEN,
            &super::KillResult { killed: 0, failed: 0, error: Some("yalnız 127.0.0.1".into()), signal: None, pids: vec![] },
        );
    }
    let policy = Policy::load(&policy_path(&st));
    let procs = Arc::clone(&st.extras.procs);
    // Kapalıyken tarama bile yapılmaz.
    let scan = if policy.allow_kill { tokio::task::spawn_blocking(move || procs.fresh()).await.unwrap_or_default() } else { Arc::default() };
    let (outcome, r) = st.extras.killer.request(&policy, &target, &scan.procs, st.extras.ctx, st.hub.now_ms());
    let status = match outcome {
        Outcome::Done if r.killed > 0 => StatusCode::OK,
        Outcome::Denied => StatusCode::FORBIDDEN,
        Outcome::Done | Outcome::Conflict => StatusCode::CONFLICT,
    };
    json(status, &r)
}
