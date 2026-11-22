//! The character classes and string primitives Python's `re` and `str` give for
//! free, spelled out once.
//!
//! Decides: what counts as whitespace (`\s`, including the four information
//! separators Rust's `char::is_whitespace` leaves out), what counts as a word
//! character (`\w`), what `s[:n]` means (characters, not bytes), what
//! `" ".join(s.split())` means, and how to look at the last character.
//!
//! Does not decide: folding, matching or scoring. It is the only module that
//! knows Python's idea of a character class, so no other module re-derives it.

/// `\s` as Python spells it for `str` patterns: Unicode whitespace plus the
/// four information separators Rust leaves out of `char::is_whitespace`.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `\w` as Python spells it: `str.isalnum()` or underscore.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The first `n` characters of `s` (`s[:n]` in Python, counted in chars).
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `" ".join(s.split())` — runs of whitespace collapsed, ends trimmed.
pub fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split(is_space).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

pub fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// Last character of `s`, for the lookbehind tests.
pub fn last_char(s: &str) -> Option<char> {
    s.chars().next_back()
}
