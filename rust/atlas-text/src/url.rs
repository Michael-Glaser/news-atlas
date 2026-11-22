//! `url` — the canonical form of a URL, for duplicate detection.
//!
//! Decides what is dropped (the scheme, `utm_*` and the other tracking
//! parameters, the fragment, a trailing slash, the case of the host), what is
//! kept (the path, the remaining query pairs, sorted so `?a=1&b=2` and `?b=2&a=1`
//! are one URL), and that an empty path becomes `/`.
//!
//! It does not decide whether a URL is safe to fetch (`atlas-guard`'s `urlsafe`
//! does), does not follow redirects, and does not know that two different URLs can
//! be the same page: it only makes one URL look like itself.

/// Canonical form for duplicate detection: scheme dropped, tracking parameters and
/// the fragment removed, lowercased, no trailing slash.
pub fn norm_url(url: &str) -> String {
    let raw = url.trim();
    let after_scheme = match raw.find("://") {
        Some(i) => &raw[i + 3..],
        None => raw,
    };
    let (authority, rest) = match after_scheme.find(|c| c == '/' || c == '?' || c == '#') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or(authority).to_lowercase();
    let host = host.trim_end_matches(':').to_string();
    let path = match rest.find(|c| c == '?' || c == '#') {
        Some(i) => &rest[..i],
        None => rest,
    };
    let mut path = path.trim_end_matches('/').to_string();
    if path.is_empty() {
        path.push('/');
    }
    let mut keep: Vec<&str> = Vec::new();
    if let Some(q) = rest.find('?') {
        let query = &rest[q + 1..];
        let query = match query.find('#') {
            Some(i) => &query[..i],
            None => query,
        };
        for pair in query.split('&') {
            let key = pair.split('=').next().unwrap_or("");
            let k = key.to_lowercase();
            if k.starts_with("utm_")
                || matches!(
                    k.as_str(),
                    "fbclid" | "gclid" | "ref" | "source" | "cmpid" | "at_medium" | "at_campaign"
                        | "s" | "smid" | "share" | "igshid" | "mc_cid" | "mc_eid"
                )
            {
                continue;
            }
            keep.push(pair);
        }
    }
    keep.sort_unstable();
    let mut out = host;
    out.push_str(&path);
    if !keep.is_empty() {
        out.push('?');
        out.push_str(&keep.join("&"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_url_strips_tracking_and_fragment() {
        assert_eq!(
            norm_url("https://Example.com/a/b/?utm_source=x&id=7#frag"),
            "example.com/a/b?id=7"
        );
        assert_eq!(norm_url("https://example.com/"), "example.com/");
        assert_eq!(norm_url("http://example.com"), "example.com/");
    }
}
