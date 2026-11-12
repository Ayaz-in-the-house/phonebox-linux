//! Yerel API kapısı — PhoneBox 0.6.9 `Program.cs:104-113` ve `BrowserGuard.cs` eşi.
//!
//! - Host yalnız `127.0.0.1` / `localhost` (DNS rebinding).
//! - Origin boş ya da aynı köken (`http://127.0.0.1:<port>`, `http://localhost:<port>`) (CSRF).
//! - `Sec-Fetch-Site: cross-site|same-site` → yabancı; tek istisna `/api` dışına GET sayfa gezintisi.
//!
//! Yol karşılaştırması ASP.NET `StartsWithSegments` gibi büyük/küçük harf duyarsızdır; `/agent/api/*`
//! 0.6.9'da kapıdan ÖNCE `/api/*`'a yazıldığı için burada da öyle değerlendirilir.

/// Kapının gördüğü istek alanları (başlık değeri yoksa `None`).
#[derive(Debug, Clone, Copy)]
pub struct Incoming<'a> {
    pub host: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub sec_fetch_site: Option<&'a str>,
    pub sec_fetch_mode: Option<&'a str>,
    pub is_get: bool,
    pub path: &'a str,
}

/// `Host` başlığından port ve IPv6 köşeli parantezi ayıklanmış ad.
#[must_use]
pub fn host_part(h: &str) -> &str {
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    h.rsplit_once(':').map_or(h, |(a, _)| a)
}

/// `path` == `seg` ya da `seg/` ile başlıyor (ASCII büyük/küçük harf duyarsız).
#[must_use]
pub fn starts_with_segment(path: &str, seg: &str) -> bool {
    let Some(head) = path.get(..seg.len()) else {
        return false;
    };
    head.eq_ignore_ascii_case(seg) && matches!(path.as_bytes().get(seg.len()), None | Some(b'/'))
}

/// 0.6.9 yeniden yazımı: `/agent/api/x` → `/api/x`.
#[must_use]
pub fn effective_path(path: &str) -> &str {
    if starts_with_segment(path, "/agent/api") { path.get("/agent".len()..).unwrap_or(path) } else { path }
}

/// `BrowserGuard.IsForeign` eşi.
#[must_use]
pub fn is_foreign(site: Option<&str>, mode: Option<&str>, is_get: bool, path: &str) -> bool {
    if !matches!(site, Some("cross-site" | "same-site")) {
        return false;
    }
    let page_navigation = mode == Some("navigate") && is_get && !starts_with_segment(path, "/api");
    !page_navigation
}

/// İstek yerel API'ye girebilir mi.
#[must_use]
pub fn allowed(r: &Incoming<'_>, port: u16) -> bool {
    let host = r.host.map_or("", host_part);
    let host_ok = host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost");
    let origin_ok = match r.origin {
        None | Some("") => true,
        Some(o) => o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}"),
    };
    let path = effective_path(r.path);
    host_ok && origin_ok && !is_foreign(r.sec_fetch_site, r.sec_fetch_mode, r.is_get, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req<'a>(host: &'a str, origin: Option<&'a str>, site: Option<&'a str>, mode: Option<&'a str>, path: &'a str) -> Incoming<'a> {
        Incoming { host: Some(host), origin, sec_fetch_site: site, sec_fetch_mode: mode, is_get: true, path }
    }

    #[test]
    fn host_part_strips_port_and_brackets() {
        assert_eq!(host_part("127.0.0.1:8766"), "127.0.0.1");
        assert_eq!(host_part("localhost"), "localhost");
        assert_eq!(host_part("[::1]:8766"), "::1");
    }

    #[test]
    fn tp_w2_02a_foreign_host_rejected() {
        assert!(allowed(&req("127.0.0.1:8766", None, None, None, "/api/state"), 8766));
        assert!(allowed(&req("LOCALHOST:8766", None, None, None, "/api/state"), 8766));
        assert!(!allowed(&req("evil.example", None, None, None, "/api/state"), 8766));
        assert!(!allowed(&req("127.0.0.1.evil:8766", None, None, None, "/api/state"), 8766));
        assert!(!allowed(&req("[::1]:8766", None, None, None, "/api/state"), 8766));
        let none = Incoming { host: None, ..req("x", None, None, None, "/") };
        assert!(!allowed(&none, 8766));
    }

    #[test]
    fn tp_w2_02b_origin_same_only() {
        assert!(allowed(&req("127.0.0.1", Some("http://127.0.0.1:8766"), None, None, "/api/x"), 8766));
        assert!(allowed(&req("127.0.0.1", Some("http://localhost:8766"), None, None, "/api/x"), 8766));
        assert!(allowed(&req("127.0.0.1", Some(""), None, None, "/api/x"), 8766));
        assert!(!allowed(&req("127.0.0.1", Some("http://evil.example"), None, None, "/api/x"), 8766));
        assert!(!allowed(&req("127.0.0.1", Some("http://127.0.0.1:9999"), None, None, "/api/x"), 8766));
        assert!(!allowed(&req("127.0.0.1", Some("null"), None, None, "/api/x"), 8766));
    }

    #[test]
    fn tp_w2_02c_sec_fetch_site() {
        for site in ["cross-site", "same-site"] {
            assert!(!allowed(&req("127.0.0.1", None, Some(site), Some("cors"), "/api/state"), 8766), "{site}");
            assert!(!allowed(&req("127.0.0.1", None, Some(site), Some("navigate"), "/api/state"), 8766), "{site} nav api");
            assert!(!allowed(&req("127.0.0.1", None, Some(site), Some("navigate"), "/agent/api/state"), 8766), "{site} nav agent api");
            assert!(!allowed(&req("127.0.0.1", None, Some(site), Some("navigate"), "/API/state"), 8766), "{site} case");
            assert!(allowed(&req("127.0.0.1", None, Some(site), Some("navigate"), "/"), 8766), "{site} page nav");
        }
        for site in ["same-origin", "none"] {
            assert!(allowed(&req("127.0.0.1", None, Some(site), Some("cors"), "/api/state"), 8766), "{site}");
        }
        let post = Incoming { is_get: false, ..req("127.0.0.1", None, Some("cross-site"), Some("navigate"), "/") };
        assert!(!allowed(&post, 8766));
    }

    #[test]
    fn segments() {
        assert!(starts_with_segment("/api", "/api"));
        assert!(starts_with_segment("/api/x", "/api"));
        assert!(!starts_with_segment("/apix", "/api"));
        assert!(!starts_with_segment("/a", "/api"));
        assert_eq!(effective_path("/agent/api/state"), "/api/state");
        assert_eq!(effective_path("/agent/x"), "/agent/x");
    }
}
