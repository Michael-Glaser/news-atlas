//! `publisher` — a presentable outlet name out of a URL.
//!
//! Decides how a host is found (`host_of`: scheme dropped, userinfo dropped,
//! port dropped, `www.` and a trailing dot stripped, lowercased), which labels
//! are registrable-domain noise (`co`, `com`, `news`, `www` and friends), and how
//! the remaining label is presented (`news.example.co.uk` -> `Example`).
//!
//! It does not ask the network, does not know a publisher list, and does not
//! decide whether the domain is a feed's real owner: a mirror domain yields a
//! mirror name. `atlas-guard` decides whether a URL may be fetched at all.

/// A presentable outlet name from a URL host: `news.example.co.uk` -> `Example`.
pub fn publisher(url: &str) -> String {
    let host = match host_of(url) {
        Some(h) => h,
        None => return String::new(),
    };
    let mut labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    while labels.len() > 1
        && matches!(
            labels[0],
            "www" | "m" | "amp" | "news" | "edition" | "beta" | "web" | "mobile" | "de" | "en"
        )
    {
        labels.remove(0);
    }
    if labels.len() > 1 {
        let tld = labels[labels.len() - 1];
        let second = labels[labels.len() - 2];
        let second_is_suffix = matches!(tld, "uk" | "de" | "au" | "jp" | "br" | "in" | "cn" | "nz" | "za")
            || second == "com" || second == "co" || second == "org" || second == "net" || second == "ac";
        if second_is_suffix {
            labels.pop();
        }
    }
    if labels.len() > 1 {
        labels.pop();
    }
    let core = labels.last().copied().unwrap_or("");
    let mut out = String::with_capacity(core.len());
    let mut start = true;
    for ch in core.chars() {
        if start {
            out.extend(ch.to_uppercase());
            start = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.trim();
    let after_scheme = match rest.find("://") {
        Some(i) => &rest[i + 3..],
        None => rest,
    };
    let host_end = after_scheme
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..host_end];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = match host.rfind(':') {
        Some(i) if !host[i..].contains(']') => &host[..i],
        _ => host,
    };
    let host = host.trim_start_matches("www.").trim_end_matches('.').to_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_reads_the_domain() {
        assert_eq!(publisher("https://www.reuters.com/tech/x"), "Reuters");
        assert_eq!(publisher("https://news.bbc.co.uk/x"), "Bbc");
        assert_eq!(publisher("https://theopenpress.in/a"), "Theopenpress");
        assert_eq!(publisher(""), "");
    }
}
