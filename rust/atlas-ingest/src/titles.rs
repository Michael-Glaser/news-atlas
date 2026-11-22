//! When two headlines are the same story.
//!
//! Port of `ingest._title_tokens`, `ingest.norm_title` and `ingest._same_title`.
//!
//! `atlas-store`'s `norm_title` is the *key* two rows are deduplicated on (it is
//! deliberately aggressive: lower-case, NFKD, punctuation gone). This module is the
//! softer comparison the ingest path uses to recognise a reprint: identical after
//! stopwords, or four shared words, or the same first four words — a search feed
//! that republishes a headline with a different tail.
//!
//! Both exist in `ingest.py` for the same reason and neither replaces the other.

use std::collections::HashSet;

/// `ingest._STOP` — function words that do not distinguish two stories.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "being", "by", "for", "from", "has", "have",
    "had", "how", "in", "into", "is", "it", "its", "of", "on", "or", "that", "the", "their", "they",
    "this", "to", "was", "were", "will", "with", "what", "when", "who", "why", "not", "no", "over",
    "under", "more", "most", "than", "then", "there", "these", "those", "such", "but", "if", "so",
    "we", "you", "your", "our", "i", "he", "she", "his", "her", "them", "also", "said", "says",
    "say", "after", "before", "about", "amid", "via", "just", "still", "yet", "can", "could",
    "would", "should", "may", "might",
];

/// `_title_tokens` — lower-case words that carry the story, punctuation gone.
pub fn title_tokens(title: &str) -> Vec<String> {
    let lowered = title.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in lowered.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            word.push(ch);
        } else {
            flush(&mut word, &mut out);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn flush(word: &mut String, out: &mut Vec<String>) {
    if word.is_empty() {
        return;
    }
    let candidate = std::mem::take(word);
    if candidate.chars().count() < 2 {
        return;
    }
    if candidate.chars().all(|ch| ch.is_ascii_digit()) {
        return;
    }
    if STOPWORDS.contains(&candidate.as_str()) {
        return;
    }
    out.push(candidate);
}

/// `ingest.norm_title` — the tokens as one key.
pub fn norm_title(title: &str) -> String {
    title_tokens(title).join(" ")
}

/// `_same_title` — true when two headlines describe the same story.
///
/// Identical tokens, or four shared tokens covering at least 80 % of the longer
/// one, or the same first four tokens (a feed that pastes a different tail).
pub fn same_title(a: &str, b: &str) -> bool {
    let left = title_tokens(a);
    let right = title_tokens(b);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if left == right {
        return true;
    }
    let left_set: HashSet<&String> = left.iter().collect();
    let right_set: HashSet<&String> = right.iter().collect();
    let shared = left_set.intersection(&right_set).count();
    let longest = left_set.len().max(right_set.len());
    if shared >= 4 && (shared as f64) >= 0.8 * longest as f64 {
        return true;
    }
    left.len() >= 4 && right.len() >= 4 && left[..4] == right[..4]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopwords_and_punctuation_do_not_distinguish_two_stories() {
        assert!(same_title(
            "Chip exports rise as demand recovers",
            "Chip exports rise, as demand recovers!"
        ));
        assert_eq!(
            norm_title("The chip exports rise in the market"),
            "chip exports rise market"
        );
    }

    #[test]
    fn a_republished_headline_with_a_new_tail_is_the_same_story() {
        assert!(same_title(
            "Chip export rules tighten for AI accelerators",
            "Chip export rules tighten for AI accelerators in Asia"
        ));
    }

    #[test]
    fn two_different_stories_stay_different() {
        assert!(!same_title(
            "Chip exports rise as demand recovers",
            "Central bank raises interest rates again"
        ));
        assert!(!same_title("", "anything"));
        assert!(!same_title("the a of", "and but for"));
    }
}
