//! *Where* a date might be written on a card. Reading it is `atlas-dates`' job.
//!
//! This module answers one question: which substrings of a card are worth asking
//! `atlas-dates` about. It answers it by size and structure, never by words:
//!
//! * a **short run** is a label — at most [`SHORT_WORDS`] words and [`SHORT_CHARS`]
//!   characters — and is a candidate as it stands (`"2 hours ago"`,
//!   `"05.11.2026"`, `"Nov 5, 2026"`), shortest first;
//! * inside a short run, a **window around a number** is a candidate too
//!   (`"Kestrel Times · 2 hours ago"` contains `"2 hours ago"`): the window grows
//!   from the number, one word at a time, and from the number backwards, so the
//!   engine's label survives the site name printed next to it.
//!
//! There is no month table, no "ago" list, no locale list and no date format here:
//! `atlas-dates` owns every one of those rules and this crate never repeats them.
//! A candidate that is not a date costs one refused answer, which is why the list
//! is shortest-first and capped ([`MAX_CANDIDATES`]).
//!
//! A long run is not mined for dates. A result page prints its date as a label;
//! chasing every number in a paragraph would cost a process per sentence to find
//! nothing, so a card whose date is buried in prose has no date as far as this
//! crate is concerned.

/// A run this short is a label and is tried whole.
pub const SHORT_WORDS: usize = 8;

/// The character cap for a label.
pub const SHORT_CHARS: usize = 60;

/// The longest window one number may grow into.
pub const WINDOW_WORDS: usize = 4;

/// The most candidates one card is asked about.
pub const MAX_CANDIDATES: usize = 6;

/// Date candidates for one card, most label-like first.
pub fn candidates(runs: &[String]) -> Vec<String> {
    let mut short: Vec<&String> = runs
        .iter()
        .filter(|text| is_short(text))
        .collect();
    // Shortest first: `2 hours ago` before `Kestrel Times · 2 hours ago`.
    short.sort_by_key(|text| (text.split_whitespace().count(), text.chars().count()));
    let mut out: Vec<String> = Vec::new();
    for text in short.iter().take(3) {
        push(&mut out, text);
    }
    for text in short {
        for window in windows(text) {
            push(&mut out, &window);
            if out.len() >= MAX_CANDIDATES {
                return out;
            }
        }
    }
    out
}

fn is_short(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= SHORT_CHARS
        && text.split_whitespace().count() <= SHORT_WORDS
}

fn push(out: &mut Vec<String>, text: &str) {
    let trimmed = trim_edges(text);
    if trimmed.is_empty() || out.iter().any(|existing| existing == &trimmed) {
        return;
    }
    out.push(trimmed);
}

/// Windows around a number inside one run: forwards from the number (1, 2, 3, 4
/// words) and backwards to it (2 and 3 words).
fn windows(text: &str) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<String> = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if !word.chars().any(|ch| ch.is_ascii_digit()) {
            continue;
        }
        for length in 1..=WINDOW_WORDS {
            let end = (index + length).min(words.len());
            if end <= index {
                continue;
            }
            out.push(words[index..end].join(" "));
        }
        for back in 2..=3usize {
            let start = index.saturating_sub(back);
            if start == index {
                continue;
            }
            out.push(words[start..=index].join(" "));
        }
        break;
    }
    out
}

/// Drop the separators a page wraps a label in (`·`, `|`, dashes, brackets,
/// colons, quotes) so `· 05.11.2026 ·` reaches `atlas-dates` as `05.11.2026`.
pub fn trim_edges(text: &str) -> String {
    let trim = |ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '·' | '|'
                    | '•'
                    | '-'
                    | '–'
                    | '—'
                    | ':'
                    | ','
                    | ';'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '"'
                    | '\''
                    | '>'
                    | '<'
                    | '/'
            )
    };
    text.trim_matches(trim).to_string()
}
