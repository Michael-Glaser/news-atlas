//! `terms` — how a word matches an article, and what a set of matches scores.
//!
//! Decisions: what "this term appears in this text" means ([`starts_a_word`]: a
//! match at a word start, Python's `\b` without a regex engine), which of a list of
//! terms a text holds ([`matched_terms`]), and what coverage is worth
//! ([`coverage`] / `score_of` — `monitor.py::_keyword_score`).
//!
//! It does not: read a query ([`crate::verdict::query_terms`] does), ask the model
//! ([`crate::judge`] does), or know what a threshold is.
//!
//! The terms are never a vocabulary of this crate's: they come from the monitor's
//! own query or from the model's answer. This file only answers whether the
//! article contains them.

/// Does `term` appear in `haystack` at the start of a word?
///
/// Both sides are expected lower-cased. A match is at a word start when the
/// character before it is not alphanumeric — Python's `\b`, without a regex engine.
pub fn starts_a_word(haystack: &str, term: &str) -> bool {
    if term.is_empty() || haystack.len() < term.len() {
        return false;
    }
    let mut from = 0usize;
    while let Some(found) = haystack[from..].find(term) {
        let at = from + found;
        let boundary = match haystack[..at].chars().next_back() {
            None => true,
            Some(before) => !before.is_alphanumeric(),
        };
        if boundary {
            return true;
        }
        // Step past the first character of this match and look again.
        from = at + haystack[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        if from >= haystack.len() {
            break;
        }
    }
    false
}

/// Every term of this list that `text` contains, in the order the terms arrived.
///
/// The text is lower-cased once here, so a caller may pass a page as it was read.
pub fn matched_terms(terms: &[String], text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    terms
        .iter()
        .filter(|term| starts_a_word(&lower, term))
        .cloned()
        .collect()
}

/// The score the query's own terms earn against one article: the fallback number,
/// and nothing else. `ModelJudge` never uses it — a score the model did not state
/// is not the model's to lend.
pub fn coverage(terms: &[String], article: &crate::candidates::Article) -> i64 {
    if terms.is_empty() {
        return 0;
    }
    let title = article.title.to_lowercase();
    let body = article.body().to_lowercase();
    let hits: Vec<&String> = terms
        .iter()
        .filter(|term| starts_a_word(&body, term) || starts_a_word(&title, term))
        .collect();
    let in_title = hits.iter().any(|term| starts_a_word(&title, term));
    score_of(hits.len(), terms.len(), in_title)
}

/// `2 + 6 * (hits / terms)`, plus one for a title hit, at least 1 and at most 10
/// — `monitor.py::_keyword_score`, in one place so the two callers that need it
/// cannot drift.
pub fn score_of(hits: usize, terms: usize, in_title: bool) -> i64 {
    if terms == 0 || hits == 0 {
        return 0;
    }
    let mut score = 2.0 + 6.0 * (hits as f64 / terms as f64);
    if in_title {
        score += 1.0;
    }
    (score.round() as i64).clamp(1, 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidates::Article;

    fn article(title: &str, summary: &str) -> Article {
        Article {
            id: 1,
            title: title.to_string(),
            summary: summary.to_string(),
            ..Article::default()
        }
    }

    #[test]
    fn a_term_matches_only_at_a_word_start() {
        assert!(starts_a_word("the chip export rule", "chip"));
        assert!(!starts_a_word("microchip exports", "chip"));
        assert!(starts_a_word("chip", "chip"));
        assert!(!starts_a_word("chip", "chips"));
        assert!(!starts_a_word("", "chip"));
        assert!(!starts_a_word("chip", ""));
    }

    #[test]
    fn a_punctuation_mark_counts_as_a_word_boundary_and_a_letter_does_not() {
        assert!(starts_a_word("export-control rule", "control"));
        assert!(starts_a_word("the (chip) rule", "chip"));
        // Python's `\b` says a hyphen ends a word, so `re-export` holds `export`.
        assert!(starts_a_word("re-export", "export"));
        // A letter does not: `microchip` does not hold `chip`.
        assert!(!starts_a_word("microchip", "chip"));
    }

    #[test]
    fn only_the_terms_the_text_holds_come_back() {
        let terms = vec!["quota".to_string(), "tariff".to_string()];
        assert_eq!(
            matched_terms(&terms, "The QUOTA was raised"),
            vec!["quota".to_string()],
            "the match is case-insensitive and keeps the caller's spelling"
        );
        assert!(matched_terms(&[], "anything").is_empty());
    }

    #[test]
    fn coverage_scores_matches_and_a_title_hit() {
        // Both terms, one of them in the title: 2 + 6 + 1 = 9.
        assert_eq!(
            coverage(
                &["chip".to_string(), "export".to_string()],
                &article("New chip rules", "the export ban")
            ),
            9
        );
        // One of two terms, in the body and not the title: 2 + 3 = 5.
        assert_eq!(
            coverage(
                &["chip".to_string(), "tariff".to_string()],
                &article("New rules", "the chip ban")
            ),
            5
        );
        assert_eq!(coverage(&[], &article("t", "s")), 0);
        assert_eq!(
            coverage(&["tariff".to_string()], &article("t", "s")),
            0,
            "no match is a zero, not a floor of 1"
        );
    }
}
