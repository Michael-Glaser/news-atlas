//! A link on a result page: resolve it, unwrap it, gate it, index it.
//!
//! Four decisions, and each one is narrower than it looks:
//!
//! * [`resolve`] turns an `href` into an absolute URL against the engine's base.
//!   Relative, schemeless and fragment-only links are not results.
//! * [`unwrap`] reads a **redirect wrapper**: engines hide the real link behind
//!   their own host (`/l/?uddg=<url>`, `/ck/a?…&u=a1<base64url>`, `/url?q=<url>`).
//!   The rule is the shape, not the engine: a query parameter whose value *is* an
//!   absolute http(s) URL — percent-encoded, or base64url-encoded, with or
//!   without a short version prefix — is the real link. The wrapper itself is
//!   never the answer, so a wrapper can never smuggle a loopback address past the
//!   gate: the unwrapped URL is what gets gated.
//! * [`engine_owns`] drops the engine's own navigation: its search links, its
//!   settings page, its "next page". That is the page's furniture, not a
//!   publisher. It is matched against the **configured** engine host — there is
//!   no list of search hosts in this crate.
//! * [`shadow_key`] is an *index*, never a verdict. Two links that could be the
//!   same page get the same shadow key (host and path, ignoring case and the
//!   query); when they collide, `atlas-text`'s `norm_url` decides whether they
//!   really are duplicates. That is why the tool does not spawn a process per
//!   result: the rule still runs wherever it can change the answer.
//!
//! It does not decide whether a URL is safe — `atlas-net::gate` (which is
//! `atlas-guard`'s verdict plus the service-port policy) does — and it does not
//! fetch anything.

use url::Url;

/// Where the real link lives in a wrapper, if it does.
///
/// Tried in order, and only ever on a *value*: a parameter whose value parses as
/// an absolute http(s) URL wins; failing that, a value that decodes from
/// base64url to one. Two rounds, so a wrapper inside a wrapper still resolves;
/// a third would only ever be a hostile page's idea of a joke.
pub fn unwrap(url: &Url) -> Option<Url> {
    let mut found: Option<Url> = None;
    let mut current = url.clone();
    for _ in 0..2 {
        match inner_link(&current) {
            Some(inner) => {
                current = inner.clone();
                found = Some(inner);
            }
            None => break,
        }
    }
    found
}

/// One round of unwrapping: the real link a wrapper points at, if any.
fn inner_link(url: &Url) -> Option<Url> {
    if url.query().is_none() {
        return None;
    }
    for (_, value) in url.query_pairs() {
        if is_link(&value) {
            if let Ok(inner) = Url::parse(&value) {
                return Some(inner);
            }
        }
        if let Some(decoded) = decode_embedded(&value) {
            if let Ok(inner) = Url::parse(&decoded) {
                return Some(inner);
            }
        }
    }
    None
}

/// An absolute http(s) URL, as text.
fn is_link(value: &str) -> bool {
    let low = value.trim().to_ascii_lowercase();
    (low.starts_with("https://") || low.starts_with("http://")) && low.len() > 8
}

/// A base64url payload inside one parameter value, decoded.
///
/// The offsets are the point: engines prefix the payload with a version token
/// (`a1`, `u`, `1`), and a two-character prefix is not part of the encoding. The
/// first offset that decodes to an absolute link wins, so the rule stays "find
/// the link in the value" and not "know engine X's parameter".
pub fn decode_embedded(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.len() < 12 {
        return None;
    }
    for skip in 0..=2usize {
        if trimmed.len() <= skip {
            return None;
        }
        let Some(part) = trimmed.get(skip..) else {
            continue;
        };
        if !part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
        {
            continue;
        }
        let Some(decoded) = base64url_decode(part) else {
            continue;
        };
        if is_link(&decoded) {
            return Some(decoded);
        }
    }
    None
}

/// Base64url (RFC 4648 §5) without padding, tolerant: decoding stops at the
/// first character that is not part of the alphabet.
pub fn base64url_decode(text: &str) -> Option<String> {
    fn sextet(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some((byte - b'A') as u32),
            b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
            b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
            b'-' | b'+' => Some(62),
            b'_' | b'/' => Some(63),
            _ => None,
        }
    }
    let mut out: Vec<u8> = Vec::with_capacity(text.len());
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let value = sextet(byte)?;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    if out.is_empty() {
        return None;
    }
    String::from_utf8(out).ok()
}

/// The absolute URL one `href` points at, or `None` when it points nowhere.
///
/// `base` is the engine's own URL (the page that was fetched), so a relative
/// `href` resolves the way the browser that would have shown the page does.
pub fn resolve(href: &str, base: &str) -> Option<Url> {
    let raw = href.trim();
    if raw.is_empty() || raw.starts_with('#') {
        return None;
    }
    // A `javascript:` / `mailto:` / `data:` href is not a result.
    let low = raw.to_ascii_lowercase();
    if let Some(colon) = low.find(':') {
        let scheme = &low[..colon];
        if !scheme.is_empty() && !scheme.contains('/') && scheme != "http" && scheme != "https" {
            return None;
        }
    }
    let base_url = Url::parse(base).ok()?;
    let joined = match Url::parse(raw) {
        Ok(url) => url,
        Err(_) => base_url.join(raw).ok()?,
    };
    match joined.scheme() {
        "http" | "https" => Some(joined),
        _ => None,
    }
}

/// The host of a URL, lowercased, without a port. `""` when unreadable.
pub fn host(url: &Url) -> String {
    url.host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// The host of a URL written as text. `""` when unreadable.
pub fn host_of_url(text: &str) -> String {
    match Url::parse(text.trim()) {
        Ok(url) => host(&url),
        Err(_) => String::new(),
    }
}

/// Is this host the engine's own? Its own domain, either direction, is
/// navigation rather than a result.
pub fn engine_owns(host: &str, engine_host: &str) -> bool {
    if host.is_empty() || engine_host.is_empty() {
        return false;
    }
    let host = host.to_ascii_lowercase();
    let engine = engine_host.to_ascii_lowercase();
    if host == engine {
        return true;
    }
    let under = |name: &str, parent: &str| {
        name.len() > parent.len() + 1
            && name.ends_with(parent)
            && name.as_bytes()[name.len() - parent.len() - 1] == b'.'
    };
    under(&host, &engine) || under(&engine, &host)
}

/// The coarse index two links collide on: host and path, case-insensitive host,
/// no trailing slash, the query ignored.
///
/// This is not the canonical form — `atlas-text`'s `norm_url` is, and it is
/// asked whenever two links collide here. What this guarantees is the other
/// direction: two links `norm_url` calls equal always collide here, so the
/// confirmation is never skipped.
pub fn shadow_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let mut path = url.path().to_string();
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    if path.is_empty() {
        path.push('/');
    }
    format!("{}{}", host, path)
}

/// The URL as text, with the fragment dropped: a fragment is never sent.
pub fn href_of(url: &Url) -> String {
    let mut clean = url.clone();
    clean.set_fragment(None);
    clean.to_string()
}
