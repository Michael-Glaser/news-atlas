//! How a surface form becomes a lookup key.
//!
//! Decides: what counts as a word run (Python's `[^\W_]+`), how a run is
//! lowered, how a multi-word name is keyed ("Washington, D.C." ->
//! `washington d c`), and what it means for a word or phrase to occur in a text
//! as a whole word (`crate::scan`'s cue words and `crate::outlet`'s publisher
//! names are the same question, so they ask it here, once).
//! Does not decide: whether a key exists in the index, or what a hit is worth.

/// Push `s` into `buf`, lowered character by character: exactly what
/// `str.lower()` does per character, with no locale and no context.
pub(crate) fn lower_into(buf: &mut String, s: &str) {
    for c in s.chars() {
        for l in c.to_lowercase() {
            buf.push(l);
        }
    }
}

/// Every maximal run of alphanumeric characters — Python's `[^\W_]+`.
pub(crate) fn for_each_run<F: FnMut(&str)>(s: &str, mut f: F) {
    let mut start: Option<usize> = None;
    for (b, c) in s.char_indices() {
        if c.is_alphanumeric() {
            if start.is_none() {
                start = Some(b);
            }
        } else if let Some(from) = start.take() {
            f(&s[from..b]);
        }
    }
    if let Some(from) = start {
        f(&s[from..]);
    }
}

/// Python's `\w` for the purpose of a `\b` assertion.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn boundary(hay: &str, at: usize) -> bool {
    let before = hay[..at].chars().next_back().map(is_word_char).unwrap_or(false);
    let after = hay[at..].chars().next().map(is_word_char).unwrap_or(false);
    before != after
}

/// Does `needle` occur in `hay` between word boundaries? The `\b(?:…)\b` test the
/// cue words and the publisher names both need. `hay` must already be lowered.
pub(crate) fn occurs_as_words(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(needle) {
        let at = from + rel;
        let stop = at + needle.len();
        if boundary(hay, at) && boundary(hay, stop) {
            return true;
        }
        // One char forward, never past the end.
        from = at + hay[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        if from >= hay.len() {
            break;
        }
    }
    false
}

/// `_phrase_key`: 'Washington, D.C.' -> 'washington d c'.
pub(crate) fn phrase_key(surface: &str, case_sensitive: bool) -> String {
    let mut out = String::new();
    let mut first = true;
    for_each_run(surface, |run| {
        if !first {
            out.push(' ');
        }
        first = false;
        if case_sensitive {
            out.push_str(run);
        } else {
            lower_into(&mut out, run);
        }
    });
    out
}
