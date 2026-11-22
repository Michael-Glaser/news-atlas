//! `tokens` — which words identify a story.
//!
//! Decides what a word is (2 to 40 characters, alphanumerics plus `-`, `'`, `&`
//! inside it), which words are noise (`STOPWORDS`, English and German), that
//! comparison is lowercased, that a word is reported once, and that at most
//! `max_words` words come back.
//!
//! It does not stem, translate or rank: two spellings of one name stay two
//! tokens. Matching two stories on these tokens is `atlas-core`'s `similar`.

use std::collections::HashSet;

/// The most words one `tokenize` answer can carry, and the ceiling the request
/// envelope clamps `max_words` to.
///
/// Public because a request that names a cap has to be held to this one instead
/// of restating `32` somewhere else — the envelope, the binary and a caller must
/// agree on one number.
pub const MAX_WORDS: usize = 32;

/// How many words `tokenize` returns when a request names no `max_words`.
pub const DEFAULT_MAX_WORDS: usize = 8;


/// Words that carry no signal for matching: articles, pronouns, prepositions and
/// the auxiliaries of both languages the feeds arrive in.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "for", "from", "had", "has",
    "have", "he", "her", "his", "in", "is", "it", "its", "of", "on", "or", "she", "that", "the",
    "their", "them", "then", "there", "they", "this", "to", "was", "were", "will", "with", "you",
    "your", "der", "die", "das", "und", "oder", "ein", "eine", "einer", "ist", "sind", "war",
    "waren", "mit", "von", "zu", "zum", "zur", "im", "in", "am", "an", "auf", "für", "fuer",
    "über", "ueber", "nach", "bei", "aus", "wie", "als", "auch", "nicht", "sich", "werden",
    "wird", "hat", "haben", "es", "er", "sie", "wir", "ich",
];

/// Lowercase words, stopwords dropped, no markup, at most `max_words` words.
pub fn tokenize(input: &str, max_words: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut word = String::new();
    let stop: HashSet<&str> = STOPWORDS.iter().copied().collect();
    let flush = |word: &mut String, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if word.is_empty() {
            return;
        }
        let w = word.to_lowercase();
        word.clear();
        if w.chars().count() < 2 || w.chars().count() > 40 || stop.contains(w.as_str()) {
            return;
        }
        if !w.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '\'' || c == '&') {
            return;
        }
        if seen.insert(w.clone()) {
            out.push(w);
        }
    };
    for ch in input.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '\'' || ch == '&' || ch == '.' {
            word.push(ch);
        } else {
            flush(&mut word, &mut out, &mut seen);
        }
        if out.len() >= max_words {
            break;
        }
    }
    flush(&mut word, &mut out, &mut seen);
    out.truncate(max_words);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_drops_stopwords_and_markup() {
        assert_eq!(tokenize("AI chips and <b>TSMC</b>", 8), vec!["ai", "chips", "tsmc"]);
        assert_eq!(tokenize("the and of", 8).len(), 0);
        assert_eq!(tokenize("one two three four five", 3).len(), 3);
        assert_eq!(tokenize("AI ai AI", 8), vec!["ai"]);
    }
}
