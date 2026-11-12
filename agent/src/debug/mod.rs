//! `/api/debug/topics` W2.4 bölümleri (K7): `procs` · `media` · `kill` · `audio`. Yalnız sayaç okur (atomik), iş yaptırmaz.

use crate::app::Shared;
use serde_json::{Map, Value, json};
use std::sync::atomic::Ordering::Relaxed;

/// `pc` bölümünün yanına eklenecek anahtarlar.
#[must_use]
pub fn topics(st: &Shared) -> Map<String, Value> {
    let (p, m, k) = (&st.extras.procs, &st.extras.media, &st.extras.killer.counters);
    let enabled = crate::kill::Policy::load(&crate::kill::http::policy_path(st)).allow_kill;
    let mut out = Map::new();
    out.insert(
        "procs".into(),
        json!({
            "trigger": "pull",
            "runs": p.runs.load(Relaxed),
            "collectUs": p.collect_us.load(Relaxed),
            "cachedHits": p.cached_hits.load(Relaxed),
            "lastPullAgeMs": p.last_pull_age_ms(),
        }),
    );
    out.insert(
        "media".into(),
        json!({
            "trigger": "dbus",
            "source": m.source.lock().map(|s| s.clone()).unwrap_or_default(),
            "players": m.player_count(),
            "events": m.events.load(Relaxed),
            "refreshes": m.refreshes.load(Relaxed),
        }),
    );
    let a = &st.extras.audio;
    out.insert(
        "audio".into(),
        json!({
            "trigger": "pulse-events",
            "socket": a.settings.socket.display().to_string(),
            "connected": a.connected.load(Relaxed),
            "connects": a.connects.load(Relaxed),
            "events": a.events.load(Relaxed),
            "queries": a.queries.load(Relaxed),
            "sets": a.sets.load(Relaxed),
            "error": a.error.lock().map(|s| s.clone()).unwrap_or_default(),
        }),
    );
    out.insert(
        "kill".into(),
        json!({
            "enabled": enabled,
            "requests": k.requests.load(Relaxed),
            "denied": k.denied.load(Relaxed),
            "term": k.term.load(Relaxed),
            "kill": k.kill.load(Relaxed),
            "failed": k.failed.load(Relaxed),
        }),
    );
    out
}
