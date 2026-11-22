//! Which country an outlet belongs to — the answer of last resort.
//!
//! Decides: how a host is taken out of a URL without a URL parser (scheme,
//! `//`, credentials, port, brackets, the `www.` prefix, casing), when a
//! domain matches a host as a whole label (`bbc.co.uk`, never
//! `notbbc.co.uk.evil.com`), how a publisher name is normalised before it is
//! compared, and — when several table rows match — that the **most specific** one
//! answers: "The Times of India" is the Indian paper, not "The Times".
//!
//! Does not decide: what the text says. An outlet country is only ever reported
//! when no place was found, and it is flagged so the map can show it as such;
//! it is never the primary answer.

use crate::data;
use crate::phrase::occurs_as_words;

/// `urlparse(url).hostname`, without a URL parser.
pub fn host_of(url: &str) -> String {
    let cleaned: String = url
        .trim_start_matches(|c: char| (c as u32) <= 0x20)
        .chars()
        .filter(|c| *c != '\t' && *c != '\r' && *c != '\n')
        .collect();
    // A netloc only exists after "scheme:" or as a protocol-relative "//host".
    let rest = match scheme_end(&cleaned) {
        Some(i) => &cleaned[i + 1..],
        None => cleaned.as_str(),
    };
    let netloc = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            &after[..end]
        }
        None => "",
    };
    if netloc.is_empty() {
        return String::new();
    }
    let hostpart = match netloc.rfind('@') {
        Some(i) => &netloc[i + 1..],
        None => netloc,
    };
    let host = if let Some(inner) = hostpart.strip_prefix('[') {
        match inner.find(']') {
            Some(i) => &inner[..i],
            None => inner,
        }
    } else {
        match hostpart.find(':') {
            Some(i) => &hostpart[..i],
            None => hostpart,
        }
    };
    let lower = host.to_lowercase();
    match lower.strip_prefix("www.") {
        Some(stripped) => stripped.to_string(),
        None => lower,
    }
}

fn scheme_end(s: &str) -> Option<usize> {
    let first = s.chars().next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let i = s.find(':')?;
    if i == 0 {
        return None;
    }
    let ok = s[..i]
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.');
    if ok {
        Some(i)
    } else {
        None
    }
}

fn ends_with_label(host: &str, domain: &str) -> bool {
    if host.len() <= domain.len() {
        return false;
    }
    let cut = host.len() - domain.len();
    host.as_bytes()[cut - 1] == b'.' && host[cut..] == *domain
}

/// `outlet_country(url, publisher)`.
///
/// The longest matching row answers, not the first one listed: "The Times of
/// India" contains "the times", and a table's row order is not evidence about
/// which paper a name means. A host or a name that matches nothing is `""`.
pub fn outlet_country(url: &str, publisher: &str) -> &'static str {
    let host = host_of(url);
    if !host.is_empty() {
        let mut best: Option<(usize, &'static str)> = None;
        for &(domain, iso) in data::OUTLET_COUNTRY {
            if host == domain || ends_with_label(&host, domain) {
                if best.map_or(true, |(length, _)| domain.len() > length) {
                    best = Some((domain.len(), iso));
                }
            }
        }
        if let Some((_, iso)) = best {
            return iso;
        }
    }
    let name = publisher.trim().to_lowercase();
    if !name.is_empty() {
        let mut best: Option<(usize, &'static str)> = None;
        for &(key, iso) in data::OUTLET_NAME_COUNTRY {
            if occurs_as_words(&name, key) && best.map_or(true, |(length, _)| key.len() > length) {
                best = Some((key.len(), iso));
            }
        }
        if let Some((_, iso)) = best {
            return iso;
        }
    }
    ""
}
