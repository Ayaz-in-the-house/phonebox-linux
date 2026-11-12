#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test")]
//! TP-W2-01: 0.6.9 fixture'larına karşı yanıt şekli.

mod common;
use common::{Rules, compare, fixture, get_json, harness};
use serde_json::json;

/// (uç, fixture, bilinçli null, dinamik eşlem, isteğe bağlı anahtar)
type Case = (&'static str, &'static str, &'static [&'static str], &'static [&'static str], &'static [&'static str]);

const CASES: &[Case] = &[
    ("/api/pair", "api_pair", &[], &[], &[]),
    ("/api/info", "api_info", &[], &[], &[]),
    // catalog = templates.json/widgets.json ham metni (0.6.9 da ham sunar): şablon widget'larında `options` her zaman yok
    ("/api/catalog", "api_catalog", &[], &["widgets[].options[]", "templates[].widgets[].options"], &["templates[].widgets[].options"]),
    // options: 0.6.9 `WidgetConfig.Options` nullable (Config.cs:179) → seçeneksiz widget'ta 0.6.9 da null yazar
    ("/api/config", "api_config", &["config.pages[].widgets[].options"], &["config.pages[].widgets[].options"], &[]),
    ("/api/settings", "api_settings", &[], &["pages[].widgets[].options", "homeAssistant"], &["pages[].widgets[].options"]),
    // update: güncelleme servisi yok (0.6.9 da güncelleme yokken null döner) · ping: kapsam dışı · gpu: W2.4 · audio: düzenekte ses sunucusu yok → null (0.6.9: wpctl yoksa null); bağlı şekil tests/audio.rs TP-W24b-A8
    ("/api/state", "api_state", &["update", "ping", "pc.gpu", "audio"], &["toggles"], &[]),
    ("/api/approvals", "api_approvals", &[], &[], &[]),
    ("/api/quotas", "api_quotas", &[], &[], &[]),
    ("/api/work", "api_work", &[], &[], &[]),
    ("/api/push", "api_push", &[], &["items"], &[]),
    ("/api/procs", "api_procs", &[], &[], &[]),
    ("/api/calendar", "api_calendar", &[], &[], &[]),
    ("/api/batteries", "api_batteries", &[], &[], &[]),
    // W2.5: ayar sayfasının `loadInfo`/`loadPhones`/`pollCharge` okudukları (404 → `loadInfo` yarıda kesiliyordu). Wi-Fi, güncelleme,
    // şarj anahtarı kapsam dışı → dürüst "kapalı/yok" yanıtı; telefon raporu yok → `phone` null (0.6.9 da rapor gelmeden null).
    ("/api/lan", "api_lan", &[], &[], &[]),
    ("/api/update", "api_update", &[], &[], &[]),
    ("/api/charge", "api_charge", &["phone"], &[], &[]),
    ("/api/phones", "api_phones", &[], &[], &[]),
];

#[tokio::test]
async fn tp_w2_01_shapes_match_069_fixtures() {
    let h = harness();
    let mut all = Vec::new();
    for (path, fx, nullable, dynamic, optional) in CASES {
        let ours = get_json(&h.app, path).await;
        let mut errs = Vec::new();
        compare(&fixture(fx), &ours, "", &Rules { nullable, dynamic, optional }, &mut errs);
        all.extend(errs.into_iter().map(|e| format!("{path}: {e}")));
    }
    assert!(all.is_empty(), "şekil farkları:\n{}", all.join("\n"));
}

#[tokio::test]
async fn tp_w2_01_agent_prefix_same_shape() {
    let h = harness();
    let a = get_json(&h.app, "/api/pair").await;
    let b = get_json(&h.app, "/agent/api/pair").await;
    assert_eq!(a, b);
}

#[tokio::test]
async fn tp_w2_s1_state_has_swap_and_keeps_069_keys() {
    let h = harness();
    let s = get_json(&h.app, "/api/state").await;
    assert_eq!(s["pc"]["swapTotalGb"], json!(100.0));
    assert_eq!(s["pc"]["swapUsedGb"], json!(12.0));
    assert_eq!(s["host"], "testhost");
    assert_eq!(s["pc"]["cpu"], json!(20.0));
}

#[test]
fn tp_w2_01a_comparator_negative_control() {
    let fx = json!({"a": "int", "b": {"c": "str"}, "l": [{"x": "bool"}], "n": "NoneType"});
    let good = json!({"a": 1.5, "b": {"c": "x"}, "l": [{"x": true}], "n": 5, "extra": 1});
    let mut errs = Vec::new();
    compare(&fx, &good, "", &Rules::default(), &mut errs);
    assert!(errs.is_empty(), "{errs:?}");
    for bad in [
        json!({"b": {"c": "x"}, "l": [], "n": null}),
        json!({"a": "1", "b": {"c": "x"}, "l": [], "n": 1}),
        json!({"a": 1, "b": {}, "l": [{"x": 1}], "n": 1}),
        json!({"a": 1, "b": null, "l": [], "n": 1}),
    ] {
        let mut errs = Vec::new();
        compare(&fx, &bad, "", &Rules::default(), &mut errs);
        assert!(!errs.is_empty(), "yakalanmalıydı: {bad}");
    }
    let mut errs = Vec::new();
    compare(&fx, &json!({"a": 1, "b": null, "l": [], "n": 1}), "", &Rules { nullable: &["b"], ..Rules::default() }, &mut errs);
    assert!(errs.is_empty());
}
