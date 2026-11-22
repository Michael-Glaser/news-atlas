//! `redact` — secrets and e-mail addresses out, before anything is logged or
//! put in a model prompt.
//!
//! Decides what a credential looks like (known key prefixes, `name = value`
//! assignments — including a name that carries the credential word after an
//! underscore and a value that looks like nothing in particular — an
//! authentication scheme followed by its credential, JWTs) and what an e-mail
//! address looks like; hands a PEM private-key block to [`crate::pemkey`], which
//! owns that one shape; merges overlapping hits so no partial secret survives and
//! one secret is one hit; and counts the hits.
//!
//! It does not decide whether the rest of the text may be kept, it is not a
//! validator (a redacted value still reached the process), and it keeps no
//! third-party secret list: the shapes are written down here.

use crate::json::{self, jnum, jobj, jstr, Json};
use crate::field;
use crate::pemkey;

const PLACEHOLDER: &str = "[redacted]";
const EMAIL: &str = "[email]";

pub struct Outcome {
    pub text: String,
    pub hits: usize,
}

/// Key shapes that are unmistakable in practice: (prefix, minimum tail).
const PREFIXES: &[(&str, usize)] = &[
    ("sk-", 8),
    ("sk_live_", 8),
    ("sk_test_", 8),
    ("rk_live_", 8),
    ("pk_live_", 8),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("github_pat_", 20),
    ("glpat-", 16),
    ("hf_", 16),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("xoxa-", 10),
    ("xoxr-", 10),
    ("xoxs-", 10),
    ("AKIA", 16),
    ("ASIA", 16),
    ("AIza", 30),
    ("ya29.", 20),
    ("npm_", 16),
    ("pypi-", 16),
    ("shpat_", 20),
    ("dop_v1_", 20),
    ("doo_v1_", 20),
    ("SG.", 16),
    ("eyJ", 20),
];

/// `key = value` forms, and how much of what follows the name is the credential.
///
/// Order matters only for readability; word boundaries decide. A name is read
/// with `_` and `-` as the *same* separator (HTTP spells a header `X-Api-Key`
/// and a config line `x_api_key`; they are one name), and a name may go on after
/// the credential word (`AWS_SECRET_ACCESS_KEY`, `token_id`) because the value
/// follows the whole name.
const ASSIGN: &[(&str, Value)] = &[
    ("private_key", Value::Field),
    ("client_secret", Value::Field),
    ("access_token", Value::Field),
    ("refresh_token", Value::Field),
    ("auth_token", Value::Field),
    ("passphrase", Value::Field),
    ("password", Value::Field),
    ("passwd", Value::Field),
    ("apikey", Value::Field),
    ("api_key", Value::Field),
    ("secret", Value::Field),
    ("token", Value::Field),
    ("pwd", Value::Field),
    ("authorization", Value::Auth),
    ("bearer", Value::Auth),
    // A *cookie*: `a=1; b=2` is one header carrying several credentials, so the
    // whole value goes, not the first token. Nothing else in the table has that
    // shape, and a cookie value is a bearer token for whatever the page was
    // logged in to — it survived every pass before this entry existed.
    ("cookie", Value::Header),
    ("set_cookie", Value::Header),
    // A URL query is where a credential hides under a short name the rest of the
    // table does not carry: a presigned URL (`X-Amz-Signature`,
    // `X-Amz-Credential`), an OAuth callback (`?code=`, `?sig=`, `?key=`). The
    // names are the *protocol's*, not a guess about the content; a colon in
    // prose stays prose (see `Value::Field`).
    ("key", Value::Field),
    ("sig", Value::Field),
    ("signature", Value::Field),
    ("credential", Value::Field),
    ("code", Value::Field),
];

/// What a name in [`ASSIGN`] means for the text after the separator.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Value {
    /// `name = value`. The value is the token after the separator whatever it
    /// looks like — but a colon is also how prose is written, so a colon value
    /// must look like a value before it is taken.
    Field,
    /// `name: <scheme> <credential>` — an authentication header. The scheme word
    /// (`Basic`, `Bearer`, `Negotiate`) stays; the credential after it goes.
    Auth,
    /// `name: <rest of the line>` — a cookie header, whose whole value is the
    /// credential. Attributes (`Path=/`, `HttpOnly`) are part of the same header
    /// and go with it.
    Header,
}

fn lower(c: u8) -> u8 {
    if c.is_ascii_uppercase() {
        c + 32
    } else {
        c
    }
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn is_space(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\n' || c == b'\r'
}

fn is_token_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

/// Where a key *name* may begin: not in the middle of a word.
///
/// `_` and `-` are separators, not letters. A log line carries
/// `db_password=hunter2` and `AWS_SECRET_ACCESS_KEY=…`, and treating `_` as part
/// of the word hid the credential word inside a longer name — the one place a
/// secret is most likely to be written down.
fn starts_a_word(b: &[u8], i: usize) -> bool {
    i == 0 || !b[i - 1].is_ascii_alphanumeric()
}

/// ASCII case-insensitive match of `word` at `i`, with word boundaries so
/// "token" does not fire inside "tokens" and "pwd" not inside "pwdless".
///
/// A name may be spelled with `-` around the credential word (`X-Api-Key`,
/// `x-client-secret`) — [`starts_a_word`] already treats a separator as a word
/// boundary, so each half of such a name is read on its own, which is why the
/// credential words in [`ASSIGN`] are the short ones (`key`, `token`, `secret`)
/// next to the compound ones (`api_key`, `client_secret`).
fn word_at(b: &[u8], i: usize, word: &str) -> bool {
    let w = word.as_bytes();
    if i + w.len() > b.len() {
        return false;
    }
    for k in 0..w.len() {
        if lower(b[i + k]) != w[k] {
            return false;
        }
    }
    if !starts_a_word(b, i) {
        return false;
    }
    if i + w.len() < b.len() {
        // A letter or digit right after the word means it is part of a longer
        // word ("tokens", "pwdless"). A separator inside a name is not, and the
        // caller walks the rest of the name to find the value.
        let next = b[i + w.len()];
        if next.is_ascii_alphanumeric() {
            return false;
        }
    }
    true
}

/// Where a bare value ends: whitespace, a quote, punctuation that usually
/// follows a value in prose, a header or a JSON blob. `[` is included so a
/// placeholder written by an earlier pass is never eaten again.
fn value_end(b: &[u8], start: usize) -> usize {
    let mut j = start;
    while j < b.len() {
        match b[j] {
            b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b',' | b';' | b')' | b'}'
            | b']' | b'[' | b'<' | b'>' | b'&' => break,
            _ => j += 1,
        }
    }
    j
}

/// Replace every (start, end) span, merging overlaps so one secret is one
/// hit and no partial secret ever survives.
fn apply_spans(s: &str, mut spans: Vec<(usize, usize)>, replacement: &str) -> (String, usize) {
    spans.retain(|span: &(usize, usize)| {
        span.0 < span.1
            && span.1 <= s.len()
            && s.is_char_boundary(span.0)
            && s.is_char_boundary(span.1)
    });
    spans.sort();
    let mut out = String::with_capacity(s.len());
    let mut hits = 0usize;
    let mut pos = 0usize;
    let mut last_end = 0usize;
    for (start, end) in spans {
        if start < last_end {
            continue; // inside a span already replaced
        }
        if start > pos {
            if let Some(chunk) = s.get(pos..start) {
                out.push_str(chunk);
            }
        }
        out.push_str(replacement);
        hits += 1;
        pos = end;
        last_end = end;
    }
    if pos < s.len() {
        if let Some(tail) = s.get(pos..) {
            out.push_str(tail);
        }
    }
    (out, hits)
}

fn key_spans(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let mut matched = false;
        for (prefix, min_tail) in PREFIXES {
            let p = prefix.as_bytes();
            if i + p.len() > b.len() {
                continue;
            }
            if &b[i..i + p.len()] != p {
                continue;
            }
            if i > 0 && is_token_byte(b[i - 1]) {
                continue; // must start a token
            }
            let start = i;
            let mut j = i + p.len();
            while j < b.len() {
                if is_token_byte(b[j]) {
                    j += 1;
                    continue;
                }
                // Allow dots between token bytes so a JWT is taken whole.
                if b[j] == b'.' && j + 1 < b.len() && is_token_byte(b[j + 1]) {
                    j += 1;
                    continue;
                }
                break;
            }
            if j < start + p.len() + *min_tail {
                continue;
            }
            spans.push((start, j));
            i = j;
            matched = true;
            break;
        }
        if !matched {
            i += 1;
        }
    }
    spans
}

fn assignment_spans(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let mut advanced = false;
        for (key, mode) in ASSIGN {
            if !word_at(b, i, key) {
                continue;
            }
            // The name can go on: `AWS_SECRET_ACCESS_KEY=`, `token_id=`,
            // `secret_key=`, `X-Api-Key:`. The value follows the whole name, so
            // the separator is looked for after it — not in the middle, where
            // there is none.
            let mut name_end = i + key.len();
            if b.get(name_end) == Some(&b'_') {
                while name_end < b.len() && is_word_byte(b[name_end]) {
                    name_end += 1;
                }
            }
            let mut j = name_end;
            // A JSON object writes the name inside quotes: `{"password": "…"}`.
            // The closing quote sits between the name and the separator, and
            // reading it as "no separator here" left *every* JSON body
            // unredacted (measured: `{"password":"hunter2"}` gave hits 0,
            // `{"cookie":"…"}` too). A quote is punctuation, not the end of the
            // name.
            if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                j += 1;
            }
            while j < b.len() && is_space(b[j]) {
                j += 1;
            }
            let mut sep: Option<u8> = None;
            if j < b.len() && (b[j] == b'=' || b[j] == b':') {
                sep = Some(b[j]);
                j += 1;
                while j < b.len() && is_space(b[j]) {
                    j += 1;
                }
            }
            let header_value = *mode == Value::Header && sep == Some(b':');
            if sep.is_none() && *mode != Value::Auth {
                continue;
            }
            let mut start = j;
            if *mode == Value::Auth {
                // Step over an authentication scheme word, so what gets redacted
                // is the credential and not the word in front of it.
                start = after_scheme_word(b, start);
            }
            let mut quoted = false;
            let end;
            if start < b.len() && (b[start] == b'"' || b[start] == b'\'') {
                let q = b[start];
                let inner = start + 1;
                let mut k = inner;
                while k < b.len() && b[k] != q {
                    k += 1;
                }
                start = inner;
                end = k;
                quoted = true;
            } else if header_value {
                // A cookie header's value is everything to the end of the line:
                // `session=…; csrftoken=…; Path=/` are five tokens of one
                // credential, and stopping at the first `;` (or at the space
                // after the first `=`) would leave the rest of it in the log.
                end = match s.get(start..).and_then(|rest| rest.find('\n')) {
                    Some(rel) => start + rel,
                    None => b.len(),
                };
            } else {
                end = value_end(b, start);
            }
            if end <= start {
                continue;
            }
            let count = end - start;
            let has_digit = match s.get(start..end) {
                Some(value) => value.bytes().any(|c| c.is_ascii_digit()),
                None => false,
            };
            let redact_it = if header_value {
                // Every cookie value is the credential, however short.
                count >= 1
            } else if *mode == Value::Auth {
                // `authorization` and `bearer` are English words as well as header names, and
                // this branch is the one that runs with **no separator**: the noun swallows
                // the word after it. Measured on four real bodies: "expanded its buyback
                // authorization by $150 billion" lost `$150`, twice per body, and "no
                // authorization was given" lost `given`.
                //
                // So a separator, or a value long enough to be a credential and not a word.
                // `has_digit` is *not* part of this test, and the measurement is why: `$150`
                // carries a digit, so a digit test keeps the price and calls it a key. What
                // separates a key from a word here is length — a real token after a scheme
                // word (`Authorization: Bearer abcdef123456`, `authorization: Negotiate
                // YIIF…`) is far longer than any English word that follows these nouns.
                sep.is_some() || count >= 12
            } else if quoted {
                true
            } else if sep == Some(b'=') {
                // `name=value` is an assignment, so the value is a value
                // whatever it looks like: `password=letmein` has no digit and is
                // shorter than the length heuristic, and it is still a password.
                count >= 3
            } else if sep.is_some() {
                // A colon is also how prose is written ("the token: sampler of
                // the series"), so it only counts when the value looks like one.
                count >= 6 && (has_digit || count >= 12)
            } else {
                false
            };
            if redact_it {
                spans.push((start, end));
                i = end;
                advanced = true;
                break;
            }
        }
        if !advanced {
            i += 1;
        }
    }
    spans
}

/// Where the credential starts when a value begins with a scheme word.
///
/// `Authorization: Basic dXNlcjpwYXNz` used to redact `Basic` and leave the
/// base64 credentials in the log line. A scheme word is not a list to keep: it
/// is the first word when that word is short, alphabetic, and followed by more
/// value (`Basic`, `Bearer`, `Negotiate`, `NTLM`, …).
fn after_scheme_word(b: &[u8], start: usize) -> usize {
    let mut k = start;
    while k < b.len() && b[k].is_ascii_alphabetic() {
        k += 1;
    }
    let len = k - start;
    if !(2..=16).contains(&len) || k >= b.len() || !is_space(b[k]) {
        return start;
    }
    let mut rest = k;
    while rest < b.len() && is_space(b[rest]) {
        rest += 1;
    }
    if rest < b.len() {
        rest
    } else {
        start
    }
}

fn is_email_local(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'%' || c == b'+' || c == b'-'
}

fn is_email_domain(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'.' || c == b'-'
}

fn email_spans(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'@' {
            i += 1;
            continue;
        }
        let mut start = i;
        while start > 0 && is_email_local(b[start - 1]) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < b.len() && is_email_domain(b[end]) {
            end += 1;
        }
        let local = s.get(start..i).unwrap_or("");
        let domain = s.get(i + 1..end).unwrap_or("");
        let tld_ok = match domain.rsplit('.').next() {
            Some(tld) => tld.len() >= 2 && tld.bytes().all(|c| c.is_ascii_alphabetic()),
            None => false,
        };
        let valid = !local.is_empty()
            && local.len() <= 64
            && !local.starts_with('.')
            && !local.ends_with('.')
            && !domain.is_empty()
            && domain.len() <= 253
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && !domain.contains("..")
            && tld_ok;
        if valid {
            spans.push((start, end));
            i = end;
        } else {
            i += 1;
        }
    }
    spans
}

/// The password in a URL's userinfo: `scheme://user:password@host/…`.
///
/// A URL is a string that is copied into a log line whole, and RFC 3986 allows a
/// credential inside it. The e-mail rule used to be the only thing that caught
/// this, and only when the host had a dot — `https://bob:hunter2@localhost/x`
/// went through untouched.
///
/// The rule is positional, not a vocabulary: after `://`, inside the authority
/// (which ends at the first `/`, `?`, `#` or space), a `:` followed by an `@`
/// means the bytes between them are a password. `http://host:8080/` has the
/// colon and no `@`; `?u=a@b` is past the authority and is not userinfo.
fn userinfo_spans(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= b.len() {
        if &b[i..i + 3] != b"://" {
            i += 1;
            continue;
        }
        let mut j = i + 3;
        let mut colon: Option<usize> = None;
        let mut at: Option<usize> = None;
        while j < b.len() {
            match b[j] {
                b'/' | b'?' | b'#' | b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b'<' | b'>'
                | b')' | b'(' | b'[' | b']' | b',' | b';' => break,
                b':' => {
                    if colon.is_none() {
                        colon = Some(j);
                    }
                }
                b'@' => {
                    at = Some(j);
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        match (colon, at) {
            (Some(from), Some(to)) if to > from + 1 => {
                spans.push((from + 1, to));
                i = to + 1;
            }
            _ => i = j.max(i + 3),
        }
    }
    spans
}

/// The shortest run that is a token and not a word.
///
/// Chosen by measurement against the shapes the feeds and the app actually carry: a UUID is
/// 36 characters, a DOI's longest segment is shorter than this, and a news slug is
/// lower-case words joined by hyphens. A 40-character run of `[A-Za-z0-9_-]` that mixes case
/// *and* carries a digit is not a word in any of the six languages this workspace reads; it
/// is a key, an id or a signature.
///
/// One character shorter is left alone on purpose: the boundary has to be somewhere a
/// person can point at, and this one is stated, tested (39 stays, 40 goes) and cheap to move.
const TOKEN_MIN_CHARS: usize = 40;

/// Spans that are **token-shaped**: a long run of token bytes, in a *value* position.
///
/// The rule is structural and has no vocabulary — that is the point. The named rule in
/// [`ASSIGN`] catches `token=…`; this one catches the token nobody named, which is how a
/// credential usually arrives: pasted into a query parameter the app does not know
/// (`?t=…`), set as the whole value of a key (`search_engine = Zm9v…`), or dropped into a
/// path segment.
///
/// A run qualifies when **all** of these hold:
///
/// * it is at least [`TOKEN_MIN_CHARS`] bytes of `[A-Za-z0-9_-]`;
/// * it contains at least **two** digits, **two** upper-case and **two** lower-case letters.
/// Spans that are **token-shaped**: a long run of token bytes, in a *value* position.
///
/// The rule is structural and has no vocabulary — that is the point. The named rule in
/// [`ASSIGN`] catches `token=…`; this one catches the token nobody named, which is how a
/// credential usually arrives: pasted into a query parameter the app does not know
/// (`?t=…`), set as the whole value of a key (`search_engine = Zm9v…`), or named in a quoted
/// value.
///
/// A run qualifies when **all** of these hold:
///
/// * it is at least [`TOKEN_MIN_CHARS`] bytes of `[A-Za-z0-9_-]`;
/// * it contains at least **two** digits, **two** upper-case and **two** lower-case letters.
///   One of each was measured to be too loose: a real publisher URL whose slug follows a
///   percent-encoded byte (`…/20261120-%F0%9F%A6%9B-usa-fiona-cincinnati-zoo-s-famous-hippo-has-died`)
///   carries exactly one upper-case letter (the `B` of `%9B`) in a 50-byte lower-case run, and
///   the one-of-each version redacted the slug. Two of each is still satisfied by every real
///   key, id and signature (they are base64 or hex-ish, so they carry many of both) and by no
///   slug: a lower-case slug has zero upper-case letters, and a UUID (hex, 36 bytes) and a DOI
///   (`10.1038/s41586-021-03819-2`) are shorter than the cap;
/// * it sits in a **value position**: the first non-space character before it is `=`, `:`
///   or `&`, optionally through one quote (`"k":"…"`), or it is the whole trimmed text, or it
///   is the query or fragment of a location (a URL). **Never its path** — a path segment is
///   the publisher's own locator, measured: 231 of 1439 stored URLs are Google News
///   `CBMi…` path ids, and redacting them loses the story a citation points at.
///
/// A token-shaped run in the middle of a sentence is **not** redacted. That is a deliberate
/// hole and it is written down here rather than hidden: prose is what a headline, a
/// citation and a quotation are, and a 40-character mixed-case word there is far more likely
/// to be a product name than a key. A value whose credential has no separator at all is a
/// shape this rule cannot see; the caller that wants a guarantee puts the value where a
/// separator names it.
fn token_spans(s: &str) -> Vec<(usize, usize)> {
    token_spans_where(s, true)
}

/// The same, without the quoted-value rule: for a *gate* rather than a printer.
///
/// See [`credential_in`]; `redact()` keeps the quoted rule because a log line or a prompt
/// should lose a token wherever it sits inside a value, while a gate that refuses data must
/// not fire on the app's own workflow JSON.
fn credential_token_spans(s: &str) -> Vec<(usize, usize)> {
    token_spans_where(s, false)
}

fn token_spans_where(s: &str, quoted_values: bool) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    token_runs(s)
        .into_iter()
        .filter(|&(start, end)| in_value_position(b, start, end, quoted_values))
        .collect()
}

/// Every token-shaped run in `text`, wherever it sits.
///
/// The shape test only: 40+ bytes of `[A-Za-z0-9_-]` — [`TOKEN_MIN_CHARS`] — carrying two
/// digits, two upper-case and two lower-case bytes.
///
/// Two of each class, not one: a single upper-case letter in a long lower-case run is what a
/// percent-escape in a publisher slug looks like (`%9B-usa-fiona-…`), and it is not a token. A
/// real key carries many of both.
///
/// The caller decides where such a run counts. [`token_spans_where`] asks the position test;
/// [`credential_in_a_setting`] does not, for the one case where the position test is wrong.
fn token_runs(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if !is_token_byte(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && is_token_byte(b[i]) {
            i += 1;
        }
        let end = i;
        if end - start < TOKEN_MIN_CHARS {
            continue;
        }
        let (mut digit, mut upper, mut lower_case) = (0usize, 0usize, 0usize);
        for byte in &b[start..end] {
            if byte.is_ascii_digit() {
                digit += 1;
            } else if byte.is_ascii_uppercase() {
                upper += 1;
            } else if byte.is_ascii_lowercase() {
                lower_case += 1;
            }
        }
        if digit < 2 || upper < 2 || lower_case < 2 {
            continue;
        }
        spans.push((start, end));
    }
    spans
}

/// Is this text **a credential** — the question a gate asks, as opposed to "should this be
/// printed with a marker in it".
///
/// Three shapes only, in order:
///
/// 1. a **named** credential, anywhere: `password = …`, `api_key=…`, `Authorization: Bearer …`,
///    the userinfo of a URL (`https://alice:secret@…`) — [`key_spans`], [`assignment_spans`],
///    [`userinfo_spans`], the same passes `redact()` runs;
/// 2. the value **is** a token-shaped run: the whole text, or what follows `=`/`:`, or the
///    query or fragment of a location;
/// 3. nothing else.
///
/// What it deliberately does **not** count is a token-shaped run inside an ordinary quoted
/// string. `redact()` redacts one, because a log line should lose it; a gate must not refuse
/// the value for it, because the app's own `research_result` row is 8011 characters of
/// workflow JSON in which every step names a URL, and refusing that row is refusing the app's
/// data (`atlas-ingest/tests/redaction_corpus.rs::real_meta_values_are_not_refused_as_secrets`).
///
/// Returns the first span found, so a caller can point at what it saw.
pub fn credential_in(text: &str) -> Option<(usize, usize)> {
    let mut named: Vec<(usize, usize)> = Vec::new();
    named.extend(key_spans(text));
    named.extend(assignment_spans(text));
    named.extend(userinfo_spans(text));
    if let Some(span) = named.into_iter().min_by_key(|span| span.0) {
        return Some(span);
    }
    credential_token_spans(text).into_iter().next()
}

/// Is this **setting value** a credential? [`credential_in`], plus the one case where its
/// position test is the wrong question.
///
/// A value that *is* a location is a place the app will go, not a citation in a document: a
/// token-shaped path segment there is that location's key (a key-in-the-path webhook,
/// `https://api.example/<opaque>/rss`), and it is stored where `?t=<opaque>` would already be
/// refused. Measured: `search_engine = https://search.example/<opaque>/rss?q={query}` was
/// stored verbatim by `Store::set_meta` while the same token in the query was refused
/// (`atlas-store/tests/config_value_secrets.rs::an_unnamed_long_opaque_token_never_reaches_the_archive`).
///
/// A location quoted inside something longer — the archive's own `research_result` workflow
/// JSON, whose every step names a Google News URL — is still a document, so its path ids stay
/// citations. That is the difference between the two callers, and it is why this is a
/// separate function instead of a third pass inside `credential_in`: the caller is the one who
/// knows whether it is holding a setting or a document.
pub fn credential_in_a_setting(value: &str) -> Option<(usize, usize)> {
    if let Some(span) = credential_in(value) {
        return Some(span);
    }
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    let leading = value.len() - value.trim_start().len();
    token_runs(trimmed)
        .first()
        .map(|&(start, end)| (start + leading, end + leading))
}
/// Is the run at `[start, end)` a value rather than a word in a sentence?
fn in_value_position(b: &[u8], start: usize, end: usize, quoted_values: bool) -> bool {
    // 1. Inside one whitespace-free run that carries a scheme: a location (URL).
    //
    //    **Only its query and fragment are values; its path is a citation.** Measured against
    //    the archive (`atlas-ingest/tests/redaction_corpus.rs`): treating every segment of a
    //    URL as a value position redacted 231 of 1439 stored URLs — every Google News article
    //    id, a base64url *path* segment that is the publisher's own locator, not a secret.
    //    `/rss/articles/CBMirAFBVV95…?oc=5` is how a reader got to the story; losing it loses
    //    the story. A credential in a location is in the query (`?key=…`, `&token=…`) or the
    //    fragment, and that half stays.
    if let Some((left, right)) = location_holding(b, start, end) {
        let mark = b[left..right]
            .iter()
            .position(|byte| *byte == b'?' || *byte == b'#')
            .map(|offset| left + offset);
        return match mark {
            Some(mark) => start > mark,
            None => false,
        };
    }
    // 2. After a separator, across spaces and at most one quote: `k=…`, `k: …`, `k = "…"`,
    //    `&k=…` and the JSON member `"k":"…"`.
    let mut at = start;
    while at > 0 && is_space(b[at - 1]) {
        at -= 1;
    }
    if at > 0 && matches!(b[at - 1], b'=' | b':' | b'&') {
        return true;
    }
    if at > 0 && matches!(b[at - 1], b'"' | b'\'') {
        let mut before = at - 1;
        while before > 0 && is_space(b[before - 1]) {
            before -= 1;
        }
        if before > 0 && matches!(b[before - 1], b'=' | b':') {
            return true;
        }
    }
    // 3. Inside a quoted string that is itself a value: `{"query": "… <token> …"}`. A JSON
    //    (or header) value is a value however long its sentence is — that is where a reader
    //    pastes a token into a question — while a quotation in running text is prose.
    //
    //    This rule is for `redact()`, whose job is output (a log line, a prompt): a token
    //    anywhere inside a value is worth removing there. A *gate* ("refuse this value")
    //    must not use it — see [`credential_in`] — because an article's own workflow JSON
    //    quotes URLs inside strings, and refusing that row is refusing the app's data.
    if quoted_values && inside_a_quoted_value(b, start) {
        return true;
    }
    // 4. The whole text is the token (`search_engine = <token>` when the caller hands the
    //    value alone, before any probe string was built around it).
    let mut first = 0usize;
    while first < b.len() && is_space(b[first]) {
        first += 1;
    }
    let mut last = b.len();
    while last > first && is_space(b[last - 1]) {
        last -= 1;
    }
    start == first && end == last
}

/// The location (a whitespace-free run carrying `scheme://`) that holds `[start, end)`.
///
/// `None` when the run is not inside one — the common case, since most text is prose.
fn location_holding(b: &[u8], start: usize, end: usize) -> Option<(usize, usize)> {
    let mut left = start;
    while left > 0 && !is_space(b[left - 1]) && !ends_a_url(b[left - 1]) {
        left -= 1;
    }
    let mut right = end;
    while right < b.len() && !is_space(b[right]) && !ends_a_url(b[right]) {
        right += 1;
    }
    if b[left..right].windows(3).any(|window| window == b"://") {
        Some((left, right))
    } else {
        None
    }
}

/// Is the run inside a `"…"` string whose opening quote starts a value?
///
/// The opening quote must be preceded (across spaces) by `:`, `=` or `[` — the separators a
/// value can begin after in JSON, in a header and in an array. A quotation in prose
/// (`He said "…"`) is preceded by a letter, so it stays prose.
fn inside_a_quoted_value(b: &[u8], start: usize) -> bool {
    let mut at = start;
    // Back to the quote that opens the string holding this run. A `{`, a `}` or a line
    // break before one means there is no enclosing quoted string: this is not JSON text.
    let mut open: Option<usize> = None;
    while at > 0 {
        at -= 1;
        match b[at] {
            b'"' => {
                // An escaped quote (`\"`) is part of the string, not its end.
                let escaped = at > 0 && b[at - 1] == b'\\';
                if !escaped {
                    open = Some(at);
                }
                break;
            }
            b'{' | b'}' | b'\n' | b'\r' => break,
            _ => {}
        }
    }
    let Some(open) = open else {
        return false;
    };
    let mut before = open;
    while before > 0 && is_space(b[before - 1]) {
        before -= 1;
    }
    before > 0 && matches!(b[before - 1], b':' | b'=' | b'[')
}

/// Punctuation that closes a location in prose (a URL inside brackets, a link in a sentence).
fn ends_a_url(c: u8) -> bool {
    matches!(
        c,
        b'"' | b'\'' | b'<' | b'>' | b'(' | b')' | b'[' | b']' | b',' | b';'
    )
}

pub fn redact(text: &str) -> Outcome {
    let (credential_free, hits) = strip_credentials(text);
    let (mailed, h5) = apply_spans(&credential_free, email_spans(&credential_free), EMAIL);
    Outcome {
        text: mailed,
        hits: hits + h5,
    }
}

/// Every pass that removes *credential material*, in the order `redact` runs them: PEM
/// private keys, prefixed keys (`sk-`, `ghp_`, …), named assignments, URL userinfo, and
/// token-shaped spans in a value position. What it leaves out is the e-mail pass, because an
/// e-mail address is personal data and not a credential.
///
/// Split out for [`carries_a_credential`]: the queue doors need the credential passes without
/// the e-mail one. A reader asking "what happened to analyst@example.com" is asking a
/// question, and refusing it because it names an address would be the store inventing a rule
/// of its own.
fn strip_credentials(text: &str) -> (String, usize) {
    let (keys_gone, h1) = pemkey::strip_private_keys(text);
    let (prefixed, h2) = apply_spans(&keys_gone, key_spans(&keys_gone), PLACEHOLDER);
    let (assigned, h3) = apply_spans(&prefixed, assignment_spans(&prefixed), PLACEHOLDER);
    let (userinfo_gone, h4) = apply_spans(&assigned, userinfo_spans(&assigned), PLACEHOLDER);
    let (token_gone, h6) = apply_spans(&userinfo_gone, token_spans(&userinfo_gone), PLACEHOLDER);
    (token_gone, h1 + h2 + h3 + h4 + h6)
}

/// Does this text carry credential material anywhere — the question a **queue door** asks
/// before it writes a row?
///
/// This is `redact`'s credential passes without the e-mail pass: a request is the reader's own
/// words, so a pasted token anywhere in it is a credential (there is no "this is a citation"
/// excuse for text the reader typed next to their own question — that is what `redact`'s
/// position test is for), while an address in a question is a subject, not a secret.
///
/// A location's *path* is still a citation here: `{"query":"see
/// https://news.google.com/rss/articles/CBMi…?oc=5"}` is a question about a story, and the
/// path id is how the reader reached it. Measured: before the position test knew about paths,
/// the door refused that payload — the same defect the archive measured on its 1439 stored
/// URLs (231 redacted).
pub fn carries_a_credential(text: &str) -> bool {
    strip_credentials(text).1 > 0
}

pub fn run(root: &Json) -> Result<String, String> {
    let text = field::required(root, "text")?;
    let chars = text.chars().count();
    if chars > field::MAX_FIELD {
        // Truncating here could cut a secret in half and still leak it.
        return Err(field::too_large("text", chars));
    }
    let outcome = redact(text);
    Ok(json::to_string(&jobj(vec![
        ("text", jstr(&outcome.text)),
        ("hits", jnum(outcome.hits as f64)),
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::MAX_FIELD;
    use crate::tests::json_of;

    #[test]
    fn redact_removes_prefixed_api_keys() {
        let out = redact("key sk-abcdefgh12345 here");
        assert!(out.text.contains("[redacted]"), "{}", out.text);
        assert!(!out.text.contains("abcdefgh12345"), "{}", out.text);
        assert_eq!(out.hits, 1);
        assert!(out.text.starts_with("key "), "{}", out.text);
    }

    #[test]
    fn redact_removes_github_aws_and_google_shapes() {
        let body = "ghp_abcdefghijklmnopqrstuvwxyz AKIAIOSFODNN7EXAMPLE \
                    AIzaSyA1234567890abcdefghijklmnopqrstu";
        let out = redact(body);
        assert_eq!(out.hits, 3, "{}", out.text);
        assert!(!out.text.contains("AKIAIOSFODNN7EXAMPLE"), "{}", out.text);
        assert!(!out.text.contains("AIzaSy"), "{}", out.text);
    }

    #[test]
    fn redact_removes_bearer_tokens() {
        let out = redact("Authorization: Bearer abcdef123456");
        assert_eq!(out.text, "Authorization: Bearer [redacted]");  // the scheme may stay, the secret may not
        assert_eq!(out.hits, 1);
        let lower = redact("curl -H 'authorization: bearer zzzz9999'");
        assert!(!lower.text.contains("zzzz9999"), "{}", lower.text);
    }

    #[test]
    fn redact_removes_assignments_but_not_prose() {
        let out = redact("password=hunter2! user=analyst");
        assert!(!out.text.contains("hunter2"), "{}", out.text);
        assert!(out.text.contains("password=[redacted]"), "{}", out.text);
        assert!(out.text.contains("user=analyst"), "{}", out.text);
        // A colon in prose is not an assignment.
        let prose = redact("the token: sampler of the series");
        assert_eq!(prose.hits, 0, "{}", prose.text);
        assert_eq!(prose.text, "the token: sampler of the series");
        // A quoted value is a value whatever it looks like.
        let quoted = redact("api_key = \"abc\"");
        assert!(!quoted.text.contains("abc"), "{}", quoted.text);
    }

    #[test]
    fn redact_removes_a_jwt_as_one_token() {
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let out = redact(&format!("header {}", token));
        assert!(!out.text.contains("eyJzdWIi"), "{}", out.text);
        assert_eq!(out.hits, 1, "{}", out.text);
    }

    #[test]
    fn redact_removes_emails() {
        let out = redact("write to analyst@example.com now");
        assert_eq!(out.text, "write to [email] now");
        assert_eq!(out.hits, 1);
        let plain = redact("no addresses here, just prose about @handles");
        assert_eq!(plain.hits, 0, "{}", plain.text);
    }

    #[test]
    fn redact_leaves_plain_prose_alone() {
        let body = "Nvidia rose 3 percent on Tuesday after TSMC reported demand.";
        let out = redact(body);
        assert_eq!(out.text, body);
        assert_eq!(out.hits, 0);
    }

    #[test]
    fn redact_finds_the_credential_word_inside_a_longer_name() {
        // A config line names its secret after an underscore. `_` used to count
        // as part of the word, so every one of these reached the log intact.
        for body in [
            "db_password=hunter2",
            "AWS_SECRET_ACCESS_KEY=abcd1234efgh",
            "secret_key=abcdefgh",
            "my_token=abc123xyz",
            "openai_api_key=abcdefgh",
            "MYSQL_PWD=letmein",
        ] {
            let out = redact(body);
            assert_eq!(out.hits, 1, "{} gave {:?}", body, out.text);
            assert!(out.text.contains(PLACEHOLDER), "{} gave {:?}", body, out.text);
            assert_eq!(
                out.text,
                format!("{}={}", body.split('=').next().unwrap(), PLACEHOLDER),
                "{} kept part of the value: {:?}",
                body,
                out.text
            );
        }
        // A name the credential word is only *part* of is still a name, and a
        // value with no separator after it is prose, not an assignment.
        assert_eq!(redact("the tokens are counted here").hits, 0);
        assert_eq!(redact("token_count=42").hits, 0, "a count is not a secret");
    }

    #[test]
    fn redact_assignment_takes_the_value_whatever_it_looks_like() {
        // `password=letmein` has no digit and is shorter than the length
        // heuristic: it used to survive. `=` is an assignment, so the value is a
        // value.
        for body in ["password=letmein", "pwd=opensesame", "password=let-me-in-now"] {
            let out = redact(body);
            assert!(!out.text.contains(body.split('=').nth(1).unwrap()), "{:?}", out.text);
            assert_eq!(out.hits, 1, "{:?}", out.text);
        }
        // The prose guard on a colon still holds (see the test above): only an
        // `=` assignment is taken on the key name alone.
        let prose = redact("the token: sampler of the series");
        assert_eq!(prose.hits, 0, "{:?}", prose.text);
    }

    #[test]
    fn redact_keeps_the_scheme_word_and_takes_the_credential_after_it() {
        // Any scheme, not just `bearer`: the credential is what follows the
        // scheme word, and it used to be exactly what survived.
        let basic = redact("Authorization: Basic dXNlcjpwYXNzd29yZA==");
        assert!(
            !basic.text.contains("dXNlcjpwYXNzd29yZA"),
            "{}",
            basic.text
        );
        assert_eq!(basic.text, "Authorization: Basic [redacted]");
        assert_eq!(basic.hits, 1);
        let negotiate = redact("authorization: Negotiate YIIFgAYKKwYBBAGCNwICCg==");
        assert!(!negotiate.text.contains("YIIFgAYK"), "{}", negotiate.text);
        // One word is not a scheme followed by a credential.
        let single = redact("Authorization: denied");
        assert_eq!(single.text, "Authorization: [redacted]");
    }

    #[test]
    fn redact_takes_a_cookie_header_to_the_end_of_its_line() {
        // A `Cookie:` header value *is* the credential: `session=…` is a bearer
        // token for whatever the page was logged in to. It survived a redaction
        // pass untouched (hits 0) because no name in the table is `cookie`, and
        // the attributes after the first `;` are part of the same header, so a
        // value-only redaction would leave `csrftoken=…` behind.
        for body in [
            "Cookie: session=8f3a9c2b1d4e; csrftoken=abc123def456",
            "Set-Cookie: sid=AAAbbbCCCddd; Path=/; HttpOnly",
            "cookie: a=1; b=2",
            "COOKIE:a=1;b=2",
        ] {
            let out = redact(body);
            assert_eq!(out.hits, 1, "{} gave {:?}", body, out.text);
            for secret in ["8f3a9c2b1d4e", "abc123def456", "AAAbbbCCCddd", "a=1", "b=2"] {
                assert!(
                    !out.text.contains(secret),
                    "{} kept {}: {:?}",
                    body,
                    secret,
                    out.text
                );
            }
        }
        // The header name stays, so the line still says what it was.
        let named = redact("Cookie: session=8f3a9c2b1d4e");
        assert_eq!(named.text, "Cookie: [redacted]");
        // A cookie *assignment* in prose or a JSON body: the value is the
        // credential, the rest of the sentence is not header syntax.
        let inline = redact("here is my header: cookie=abc123def456ghi and nothing else");
        assert!(!inline.text.contains("abc123def456ghi"), "{}", inline.text);
        assert!(inline.text.ends_with("and nothing else"), "{}", inline.text);
    }

    #[test]
    fn redact_takes_the_password_out_of_url_userinfo() {
        // `https://user:password@host/` puts a credential in the URL itself. It
        // used to survive whenever the host had no dot for the e-mail rule to
        // match (`…@localhost/`), and the e-mail rule is not the place that
        // decides this in any case.
        for (body, secret) in [
            ("https://bob:hunter2@localhost/x", "hunter2"),
            ("https://bob:hunter2@host.example/x", "hunter2"),
            ("fetch https://alice:s3cr3t@127.0.0.1:8080/p failed", "s3cr3t"),
            ("redis://:letmein@cache.example/0", "letmein"),
            ("ftp://deploy:Zx9-uP@files.example/dir", "Zx9-uP"),
        ] {
            let out = redact(body);
            assert!(
                !out.text.contains(secret),
                "{} kept its password: {:?}",
                body,
                out.text
            );
            assert!(out.text.contains("[redacted]") || out.text.contains("[email]"), "{}", out.text);
        }
        // A mailto: is a user and a host, not a password — the e-mail rule runs
        // and the userinfo rule must not eat the address twice.
        assert_eq!(redact("mail bob@example.com").text, "mail [email]");
        // An `@` that is a handle, not userinfo, is untouched.
        assert_eq!(redact("ask @atlas about it").hits, 0);
    }

    #[test]
    fn redact_takes_the_value_after_a_hyphenated_header_name() {
        // A header name is often written with `-` where a config key uses `_`,
        // and the credential word in it is a whole word either way
        // (`starts_a_word` treats a separator as a boundary). None of these
        // headers were read before: `X-Api-Key` names no word in the table, and
        // the value travelled with it (hits 0).
        for body in [
            "X-Api-Key: 9f8e7d6c5b4a3210",
            "Api-Key: 9f8e7d6c5b4a3210",
            "X-API-KEY: 9f8e7d6c5b4a3210",
            "X-Access-Token: 9f8e7d6c5b4a3210",
            "x-client-secret: 9f8e7d6c5b4a3210",
        ] {
            let out = redact(body);
            assert_eq!(out.hits, 1, "{} gave {:?}", body, out.text);
            assert!(!out.text.contains("9f8e7d6c5b4a3210"), "{} kept it: {:?}", body, out.text);
        }
        // A hyphenated *word* that is not a name followed by a separator is not
        // an assignment: prose keeps both words.
        assert_eq!(redact("a well-known token: sampler of the series").hits, 0);
        // The plural is a word about keys, not a name with a value after it.
        assert_eq!(redact("the api-keys are listed in the archive").hits, 0);
    }

    #[test]
    fn redact_takes_a_signed_url_query_to_the_credential_it_carries() {
        // A presigned URL is a bearer credential in its query string: whoever
        // holds `X-Amz-Signature` (or an OAuth `sig`, a `key`, a `credential`)
        // is the user. None of these names is `token` or `secret`, so the
        // whole URL travelled unredacted (hits 0).
        for (body, secret) in [
            (
                "https://s3.example/b?X-Amz-Signature=deadbeef0011223344",
                "deadbeef0011223344",
            ),
            (
                "https://ex.example/p?a=1&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20260101",
                "AKIAIOSFODNN7EXAMPLE",
            ),
            ("https://ex.example/p?sig=9f8e7d6c5b4a3210", "9f8e7d6c5b4a3210"),
            ("https://ex.example/p?signature=9f8e7d6c5b4a3210", "9f8e7d6c5b4a3210"),
            ("https://ex.example/p?access_key=9f8e7d6c5b4a3210", "9f8e7d6c5b4a3210"),
            ("GET /r?code=9f8e7d6c5b4a3210&state=x", "9f8e7d6c5b4a3210"),
        ] {
            let out = redact(body);
            assert!(
                !out.text.contains(secret),
                "{} kept its credential: {:?}",
                body,
                out.text
            );
            assert_eq!(out.hits, 1, "{} gave {:?}", body, out.text);
        }
        // The rest of the URL survives: redaction is not a filter on the line.
        let path = redact("https://s3.example/topics/market?X-Amz-Signature=deadbeef0011223344");
        assert!(path.text.starts_with("https://s3.example/topics/market?"), "{}", path.text);
        // A benign query parameter is not a credential.
        assert_eq!(redact("https://ex.example/p?page=2&sort=newest").hits, 0);
    }

    #[test]
    fn redact_reads_a_json_member_name_through_its_closing_quote() {
        // A JSON object is how a config dump, a tool call's arguments and an
        // HTTP request body are written into an error message. `{"password":"…"}`
        // put the name's closing quote between the name and the `:`, and every
        // one of these reached the line intact (measured, hits 0).
        for (body, secret) in [
            (r#"{"password":"hunter2"}"#, "hunter2"),
            (r#"{"access_token": "abc123def456"}"#, "abc123def456"),
            (r#"{"X-Api-Key": "9f8e7d6c5b4a3210"}"#, "9f8e7d6c5b4a3210"),
            (r#"{'client_secret': 'shh-1234'}"#, "shh-1234"),
            (r#"{"headers":{"cookie":"session=8f3a9c2b1d4e"}}"#, "8f3a9c2b1d4e"),
        ] {
            let out = redact(body);
            assert!(
                !out.text.contains(secret),
                "{} kept its credential: {:?}",
                body,
                out.text
            );
        }
        // The JSON stays JSON: the *value* is replaced, the object is not eaten.
        let shaped = redact(r#"{"password":"hunter2","page":2}"#);
        assert_eq!(shaped.text, r#"{"password":"[redacted]","page":2}"#);
        // A quoted word in prose is not a JSON member.
        assert_eq!(redact(r#"the "token": sampler of the series"#).hits, 0);
    }

    #[test]
    fn redact_run_returns_text_and_hits() {
        let out = run(&json_of("{\"text\":\"mail analyst@example.com\"}")).expect("ok");
        assert_eq!(out, "{\"text\":\"mail [email]\",\"hits\":1}");
    }

    #[test]
    fn redact_refuses_an_oversized_field_instead_of_cutting_a_secret() {
        let big = "a".repeat(MAX_FIELD + 1);
        let input = format!("{{\"text\":\"{}\"}}", big);
        let root = json_of(&input);
        assert!(run(&root).is_err());
    }
}

#[cfg(test)]
mod token_shape_tests {
    use super::*;

    /// 39 bytes of `[A-Za-z0-9]`: one character short of the cap, mixed case, with digits.
    const SHORT: &str = "Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2P";
    /// The same run with one more character: 40 bytes, so it is a token.
    const LONG: &str = "Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2Pq";

    #[test]
    fn the_two_fixtures_are_the_lengths_they_claim() {
        assert_eq!(SHORT.len(), 39);
        assert_eq!(LONG.len(), 40);
    }

    #[test]
    fn a_token_that_is_one_character_short_of_the_cap_stays() {
        // The cap is stated, not implied: moving `TOKEN_MIN_CHARS` must break this test.
        assert_eq!(TOKEN_MIN_CHARS, 40);
        let out = redact(&format!("search_engine = {SHORT}"));
        assert_eq!(out.hits, 0, "{}", out.text);
        assert!(out.text.contains(SHORT), "{}", out.text);
    }

    #[test]
    fn a_forty_byte_mixed_case_value_is_redacted() {
        let out = redact(&format!("search_engine = {LONG}"));
        assert_eq!(out.hits, 1, "{}", out.text);
        assert!(!out.text.contains(LONG), "{}", out.text);
        assert_eq!(out.text, "search_engine = [redacted]");
    }

    #[test]
    fn a_token_in_a_query_is_redacted_but_the_path_is_a_citation() {
        // Flipped, and the flip is the fix the archive asked for. `?t=<token>` is a value a
        // search template carries a key in; `/…/<token>/rss` is a locator, and treating it as
        // a value redacted 231 of the archive's 1439 stored URLs.
        let query = redact(&format!("https://search.example/?q={{query}}&t={LONG}"));
        assert!(!query.text.contains(LONG), "{}", query.text);
        assert!(query.text.contains("t=[redacted]"), "{}", query.text);

        let path = redact(&format!("https://search.example/{LONG}/rss?q={{query}}"));
        assert_eq!(path.hits, 0, "the path is not a value position: {}", path.text);
        assert!(path.text.contains(LONG), "{}", path.text);
    }

    #[test]
    fn a_google_news_article_id_in_a_path_stays() {
        // The measured case, in full: the id is 235 characters of base64url in the *path*, and
        // it is how the reader gets to the story. Redacting it loses the citation.
        let url = "https://news.google.com/rss/articles/CBMirAFBVV95cUxNNlM4aDBCQjFpWWp6NFp3aU1wUVI2LXJLeFlFSmo3c2RiZ1hhbDgxOFJaWWtHc1JiYzBXd29mVXlsbzl3ZHF2a3RzQlZ1SFAtWVFWWXdITXpJMXU0a1RfZFVfQ1E5aHRGYjJtMFo3Y2p0dlpFZU1qbFd3S0dNYVVZZVM3cm1Lc3hkcDd0MGJBQkVRaHE4V21VVlc1cjdVaDZkTGlCQXFrdTByQXVj?oc=5";
        let out = redact(url);
        assert_eq!(out.hits, 0, "{}", out.text);
        assert_eq!(out.text, url);
        // And the query half of the same URL is still a value position: a key pasted there is
        // redacted, because that is where a credential in a location actually lives.
        let with_key = format!("{url}&key={LONG}");
        let out = redact(&with_key);
        assert!(!out.text.contains(LONG), "{}", out.text);
        assert!(out.text.contains("key=[redacted]"), "{}", out.text);
    }

    #[test]
    fn credential_in_tells_a_gate_from_a_citation() {
        // The predicate the two refusal gates ask: "is this value a credential", not "does
        // this text contain one". A path id is a citation; a named key, a whole-value token
        // and a query value are credentials.
        assert!(credential_in(&format!("search_engine = {LONG}")).is_some());
        assert!(credential_in("api_key = sk-live-abcdefghijklmnop").is_some());
        assert!(credential_in("password = hunter2").is_some());
        assert!(credential_in(&format!(
            "search_engine = https://search.example/?q={{query}}&t={LONG}"
        ))
        .is_some());
        // A citation, quoted inside a workflow JSON, is not a credential.
        let research = format!(
            "research_result = {{\"steps\":[{{\"args\":{{\"url\":\"https://news.google.com/rss/articles/CBMirAFBVV95cUxNNlM4aDBCQjFpWWp6NFp3aU1wUVI2LXJLeFlFSmo3c2RiZ1hhbDgxOFJaWWtHc1JiYzBXd29mVXlsbzl3ZHF2a3RzQlZ1SFAtWVFWWXdITXpJMXU0a1RfZFVfQ1E5aHRGYjJtMFo3Y2p0dlpFZU1qbFd3S0dNYVVZZVM3cm1Lc3hkcDd0MGJBQkVRaHE4V21VVlc1cjdVaDZkTGlCQXFrdTByQXVj?oc=5\"}},{{\"args\":{{\"url\":\"https://search.example/?q={{query}}&format=RSS\"}}}}]}}"
        );
        assert!(
            credential_in(&research).is_none(),
            "the app's own workflow JSON is not a credential: {:?}",
            credential_in(&research)
        );
        // `redact()` stays liberal for output: the same text, printed, still gets its very
        // long quoted runs marked — a log line is not a gate.
        assert!(redact(&research).hits == 0, "and the paths are still citations");
    }


    #[test]
    fn a_token_in_a_json_member_is_redacted() {
        let out = redact(&format!("{{\"query\":\"{LONG}\"}}"));
        assert!(!out.text.contains(LONG), "{}", out.text);
    }

    #[test]
    fn a_real_article_url_is_left_alone() {
        // The brief's own shape, and the slug is 42 bytes of lower-case words with digits —
        // no upper case, so it is never a candidate. A news URL is a citation; redacting it
        // would corrupt every log line that mentions a story.
        //
        // The Google News entry is the one that made this test lie: its *path* is a 235-byte
        // base64url id that satisfies every class test, so the rule used to redact it and its
        // whole `CBMi…` id vanished (231 of the archive's 1439 stored URLs). The path is not
        // a value position any more, so both of these survive intact.
        for url in [
            "https://www.reuters.com/technology/tsmc-halts-china-chip-shipments-2026-11-21/",
            "https://news.google.com/rss/articles/CBMirAFBVV95cUxNNlM4aDBCQjFpWWp6NFp3aU1wUVI2LXJLeFlFSmo3c2RiZ1hhbDgxOFJaWWtHc1JiYzBXd29mVXlsbzl3ZHF2a3RzQlZ1SFAtWVFWWXdITXpJMXU0a1RfZFVfQ1E5aHRGYjJtMFo3Y2p0dlpFZU1qbFd3S0dNYVVZZVM3cm1Lc3hkcDd0MGJBQkVRaHE4V21VVlc1cjdVaDZkTGlCQXFrdTByQXVj?oc=5",
        ] {
            let out = redact(url);
            assert_eq!(out.hits, 0, "{}", out.text);
            assert_eq!(out.text, url);
        }
    }

    #[test]
    fn a_publisher_url_with_a_percent_escape_in_its_slug_stays() {
        // Measured on the archive (231 of 1435 URLs are Google News click-through blobs, 1 of
        // 23 France24 URLs looked token-shaped): the slug below carries exactly one
        // upper-case letter, the `B` of the percent-escape `%9B`, and the one-of-each version
        // of this rule redacted it — a real citation, mangled. Two of each class fixes it and
        // still catches every key.
        let url = "https://www.france24.com/en/video/20261120-%F0%9F%A6%9B-usa-fiona-cincinnati-zoo-s-famous-hippo-has-died";
        let out = redact(url);
        assert_eq!(out.hits, 0, "{}", out.text);
        assert_eq!(out.text, url);
    }

    #[test]
    fn a_doi_and_a_uuid_and_a_slug_stay() {
        for text in [
            "doi:10.1038/s41586-021-03819-2",
            "10.1000/182",
            "https://doi.org/10.1126/science.abq1234",
            // a UUID: hex, 36 bytes, no upper case
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            // a slug in a URL: lower case words joined by hyphens, with a date
            "https://example.test/2026/11/21/nvidia-beats-estimates-as-chip-demand-surges-again/",
        ] {
            let out = redact(text);
            assert_eq!(out.hits, 0, "{text} → {}", out.text);
            assert_eq!(out.text, text);
        }
    }

    #[test]
    fn a_headline_and_a_sentence_are_not_value_positions() {
        // Constraint (a) of the rule, and its cost: a token-shaped run in running text is
        // left alone. A headline, a quotation and a citation are prose, and a 40-byte
        // mixed-case word there is far more likely to be a product name than a key. The
        // sentence below is deliberately hostile — it carries the very run that a value
        // position would redact.
        let prose = format!("Nvidia's {LONG} launch slipped to November, analysts said.");
        let out = redact(&prose);
        assert_eq!(out.hits, 0, "{}", out.text);
        assert_eq!(out.text, prose);
        // Nor is a mid-sentence colon enough on its own: the run must follow the separator.
        let later = redact(&format!("the analyst said: this one is {LONG}"));
        assert_eq!(later.hits, 0, "{}", later.text);
    }

    #[test]
    fn a_run_without_one_of_the_three_classes_is_a_word() {
        // All three classes are required, so a lower-case slug, an upper-case acronym and a
        // long numeric id never qualify however long they are.
        for text in [
            format!("k = {}", "a".repeat(60)),
            format!("k = {}", "A".repeat(60)),
            format!("k = {}", "1234567890".repeat(6)),
            format!("k = {}", "abcdefgh-".repeat(8)),
        ] {
            let out = redact(&text);
            assert_eq!(out.hits, 0, "{text} → {}", out.text);
        }
    }

    #[test]
    fn a_quoted_value_is_a_value_and_a_quotation_in_prose_is_not() {
        // A reader pastes a token into a question: the JSON member is a value, so the run
        // inside the sentence is redacted. A quotation in running text is preceded by a
        // letter, so it is prose and stays — that is the whole position test.
        let json = format!("{{\"query\":\"look up {LONG} for me\"}}");
        assert!(!redact(&json).text.contains(LONG), "{}", redact(&json).text);

        let quoted = format!("He said \"have a {LONG}\" and left.");
        let out = redact(&quoted);
        assert_eq!(out.hits, 0, "{}", out.text);
        assert_eq!(out.text, quoted);
    }

    #[test]
    fn the_rule_reaches_the_pair_shape_set_meta_uses() {
        // `atlas_store::Store::set_meta` asks with `name = value`; that probe must be enough
        // for the shape rule, or the archive's door cannot see it.
        let probe = format!("search_engine = {}", LONG);
        assert!(redact(&probe).hits > 0);
        let probe_url = format!("search_engine = https://search.example/?q={{query}}&t={LONG}");
        assert!(redact(&probe_url).hits > 0);
    }

    #[test]
    fn the_queue_door_asks_carries_a_credential_not_the_email_rule() {
        // Two questions, two answers, and the difference is the e-mail pass: `redact` marks an
        // address because a log line should not print one, `carries_a_credential` — the queue
        // doors' predicate — counts only credential material, because a reader asking "what
        // happened to analyst@example.com?" is asking a question.
        assert!(redact("mail analyst@example.com").hits > 0);
        assert!(!carries_a_credential("mail analyst@example.com"));

        // A token in the reader's own words is a credential wherever it sits, including inside
        // a JSON string — that is the case the queue door exists for.
        assert!(carries_a_credential(&format!("{{\"query\":\"look up {LONG}\"}}")));
        // A named credential too, without any shape test involved.
        assert!(carries_a_credential(
            "{\"query\":\"https://api.example/v1?q=x&api_key=sk-live-abcdefghijklmnop\"}"
        ));
        // And a pasted citation is neither: the path id is a citation, which is F1's own
        // measurement (231 of 1439 stored URLs) applied to the door.
        let citation = format!(
            "{{\"query\":\"see https://news.google.com/rss/articles/CBMirAFBVV95cUxNNlM4aDBCQjFpWWp6NFp3aU1wUVI2LXJLeFlFSmo3c2RiZ1hhbDgxOFJaWWtHc1JiYzBXd29mVXlsbzl3ZHF2a3RzQlZ1SFAtWVFWWXdITXpJMXU0a1RfZFVfQ1E5aHRGYjJtMFo3Y2p0dlpFZU1qbFd3S0dNYVVZZVM3cm1Lc3hkcDd0MGJBQkVRaHE4V21VVlc1cjdVaDZkTGlCQXFrdTByQXVj?oc=5 {LONG}?\"}}"
        );
        // (the bare token above *is* one; the same sentence without it is a citation only)
        assert!(carries_a_credential(&citation));
        let citation = citation.replace(&format!(" {LONG}?"), "");
        assert!(!carries_a_credential(&citation), "{citation}");
    }

    #[test]
    fn a_setting_that_is_one_location_has_its_path_as_a_key_position() {
        // The caller knows what it is holding: `Store::set_meta` holds a *setting*, which the
        // app will use to go somewhere — so `https://host/<token>/rss` is a key in the path,
        // and the same token in a query was already refused. Without this the pair was stored
        // verbatim (`atlas-store/tests/config_value_secrets.rs`).
        assert!(credential_in_a_setting(&format!("https://search.example/{LONG}/rss?q={{query}}"))
            .is_some());
        assert!(credential_in_a_setting(LONG).is_some(), "the whole value is a token");
        // A document is not a setting: the same token inside a workflow JSON is a citation,
        // because a document quotes locations and a setting *is* one.
        let document = format!("{{\"steps\":[{{\"url\":\"https://news.google.com/rss/{LONG}?oc=5\"}}]}}");
        assert!(credential_in_a_setting(&document).is_none());
        // A location without a token-shaped run is not a credential however long its path.
        assert!(credential_in_a_setting(
            "https://www.reuters.com/technology/tsmc-halts-china-chip-shipments-2026-11-21/"
        )
        .is_none());
    }
}
