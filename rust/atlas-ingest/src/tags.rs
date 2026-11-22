//! Tags and categories: model output that ends up in the UI, in filters and in
//! prompts, so it is rebuilt from tokens instead of being trusted.
//!
//! Port of `ingest._tag_report`, `ingest._category_report`, `ingest._label_ok`,
//! `ingest._ensure_category` and `ingest._apply_tags`.
//!
//! A tag is a word the guard's tokeniser keeps: lowercase, at most four words,
//! at most forty characters, never markup, never a duplicate, never stopwords
//! only. A category must be a label the database already knows or a new short
//! one (starts with a letter, at most three words, no markup).
//!
//! Everything the model sent that does not survive is **named** in the returned
//! `rejected` list — "ignored: tag <x> (not a word)" — so the model can correct
//! itself instead of wondering why its tags vanished.

use std::collections::HashSet;

use crate::rules::Rules;

/// `ingest.TAG_MAX_CHARS`.
pub const TAG_MAX_CHARS: usize = 40;
/// `ingest.TAG_MAX_WORDS`.
pub const TAG_MAX_WORDS: usize = 4;
/// `ingest.CATEGORY_MAX_CHARS`.
pub const CATEGORY_MAX_CHARS: usize = 40;
/// `ingest.CATEGORY_MAX_WORDS`.
pub const CATEGORY_MAX_WORDS: usize = 3;
/// Categories one article may carry (`_norm_cats` default).
pub const MAX_CATEGORIES: usize = 8;

/// `ingest._STOP` — words that are never a tag on their own.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "being", "by", "for", "from", "has", "have",
    "had", "how", "in", "into", "is", "it", "its", "of", "on", "or", "that", "the", "their", "they",
    "this", "to", "was", "were", "will", "with", "what", "when", "who", "why", "not", "no", "over",
    "under", "more", "most", "than", "then", "there", "these", "those", "such", "but", "if", "so",
    "we", "you", "your", "our", "i", "he", "she", "his", "her", "them", "also", "said", "says",
    "say", "after", "before", "about", "amid", "via", "just", "still", "yet", "can", "could",
    "would", "should", "may", "might",
];

/// What survived and what did not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub kept: Vec<String>,
    /// One line per rejected item, with the reason — what the model reads back.
    pub dropped: Vec<String>,
}

/// `_tag_report` — the model's tags, as tokens the database can hold.
///
/// # A tag is a label, and a label is not a marker
///
/// The tag on an article is read back by the model in `list_tags`, so it is a
/// channel: whatever a page can get into a tag, the model reads. Measured before
/// this rule, `tag_article` with the tag `"--- END ---"` stored `--- end ---` — the
/// two tokens of a fence marker — because the tokeniser keeps a run of hyphens as
/// a token and every check here was about *letters*, not about *shape*. That tag
/// then came back in `list_tags` as a marker-shaped line.
///
/// So two structural rules were added, and neither is about words:
///
/// * a token with no letter and no digit is punctuation, not a word, and is
///   dropped ([`atlas_ai::prompts::defang`] is the same judgement the prompt fence
///   makes about a page);
/// * a tag that would still carry a run of three or more hyphen-like characters is
///   refused, because that is the shape of the only markers this app has.
///
/// What a tag *means* stays the model's decision: a page can still get the word
/// "system" or "ignore" into a tag, and it should be able to — those are words.
pub fn tag_report(rules: &dyn Rules, raw: &[String], limit: usize) -> Report {
    let limit = limit.max(1);
    let mut report = Report::default();
    for item in raw {
        if report.kept.len() >= limit {
            report.dropped.push(short(item, 40));
            continue;
        }
        let text = item.trim();
        if text.contains('<') || text.contains('>') {
            report
                .dropped
                .push(format!("{} (markup)", short(text, 32)));
            continue;
        }
        let words: Vec<String> = match rules.tokenize(text, TAG_MAX_WORDS) {
            // A tokeniser failure is not "keep it anyway": a tag that was not
            // rebuilt from tokens never reaches a column.
            Ok(words) => words
                .into_iter()
                // punctuation is not a word (see the module comment above)
                .filter(|word| word.chars().any(|ch| ch.is_alphanumeric()))
                .take(TAG_MAX_WORDS)
                .collect(),
            Err(_) => Vec::new(),
        };
        let tag: String = words.join(" ").chars().take(TAG_MAX_CHARS).collect();
        let reason = if tag.chars().count() < 2 || !tag.chars().any(|ch| ch.is_alphanumeric()) {
            "not a word"
        } else if atlas_ai::prompts::defang(&tag) != tag {
            // three or more hyphen-like characters in a row: a marker, not a label
            "not a label"
        } else if !words.is_empty() && words.iter().all(|word| is_stop(word)) {
            "stopwords only"
        } else if report.kept.iter().any(|kept| kept == &tag) {
            "duplicate"
        } else {
            ""
        };
        if !reason.is_empty() {
            report
                .dropped
                .push(format!("{} ({})", short(text, 32), reason));
            continue;
        }
        report.kept.push(tag);
    }
    report
}

/// `_category_report` — existing labels, plus new short ones, nothing else.
pub fn category_report(raw: &[String], known: &HashSet<String>, limit: usize) -> Report {
    let limit = limit.max(1);
    let mut report = Report::default();
    for item in raw {
        if report.kept.len() >= limit {
            report
                .dropped
                .push(format!("{} (limit {})", short(item, 32), limit));
            continue;
        }
        let cleaned = item.split_whitespace().collect::<Vec<&str>>().join(" ");
        if cleaned.is_empty() {
            continue;
        }
        let low = cleaned.to_lowercase();
        if report.kept.iter().any(|kept| kept.to_lowercase() == low) {
            continue;
        }
        if !known.contains(&low) && !label_ok(&cleaned) {
            report
                .dropped
                .push(format!("{} (not a short label)", short(&cleaned, 32)));
            continue;
        }
        report
            .kept
            .push(cleaned.chars().take(CATEGORY_MAX_CHARS).collect());
    }
    report
}

/// `_label_ok` — starts with a letter, no markup, at most three words.
///
/// `ingest._CATEGORY_LABEL` was a regular expression; the workspace pins no regex
/// crate, so the same shape is checked character by character here.
pub fn label_ok(name: &str) -> bool {
    let text = name.trim();
    if text.is_empty() || text.chars().count() > CATEGORY_MAX_CHARS {
        return false;
    }
    if text.split_whitespace().count() > CATEGORY_MAX_WORDS {
        return false;
    }
    if !text.chars().any(|ch| ch.is_alphabetic()) {
        return false;
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| {
        ch.is_ascii_alphanumeric()
            || ch == ' '
            || ch == '&'
            || ch == '\''
            || ch == '/'
            || ch == '+'
            || ch == '.'
            || ch == '-'
    })
}

/// Split one model string ("chips, semiconductors; export rules") the way
/// `_tag_report` did (`re.split(r"[,;\n]+")`).
pub fn split_list(raw: &str) -> Vec<String> {
    raw.split(|ch| ch == ',' || ch == ';' || ch == '\n')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

/// Model output as a list of strings: an array of strings, one string with
/// separators, or nothing. Anything else is not a tag list.
pub fn value_list(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::String(text) => Some(text.clone()),
                serde_json::Value::Number(number) => Some(number.to_string()),
                _ => None,
            })
            .collect(),
        Some(serde_json::Value::String(text)) => split_list(text),
        _ => Vec::new(),
    }
}

/// Is one topic keyword a filter, rather than a word every page contains?
///
/// `add_topic` is a tool the model calls, so its keyword list is model output —
/// and the keyword list is what `atlas-topics` matches an article against, i.e.
/// the one gate `save_article` puts between "the model was told to store this
/// page" and the archive. Measured: `add_topic(name: "Off topic", keywords:
/// "the, and, of")` made `save_article` store a page about cooking pasta under
/// that topic, because every English page contains all three words. A keyword has
/// to be able to *miss*: at least two characters, at least one letter or digit,
/// and not a word [`tag_report`] already refuses as a tag on its own.
///
/// What this does not do: decide what a topic is. `chip export`, `nvidia`,
/// `supply chains` all pass, and the model still chooses the words.
pub fn keyword_ok(term: &str) -> bool {
    let text = term.trim();
    if text.chars().count() < 2 || text.chars().count() > TAG_MAX_CHARS {
        return false;
    }
    if !text.chars().any(|ch| ch.is_alphanumeric()) {
        return false;
    }
    !is_stop(text)
}

fn is_stop(word: &str) -> bool {
    STOPWORDS.contains(&word.to_lowercase().as_str())
}

fn short(text: &str, limit: usize) -> String {
    let collapsed = crate::scrub(text);
    collapsed.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_must_look_like_one() {
        assert!(label_ok("Person"));
        assert!(label_ok("United States"));
        assert!(label_ok("Science & Tech"));
        assert!(!label_ok(""));
        assert!(!label_ok("<script>"));
        assert!(!label_ok("Person Entity Kind Name"));
        assert!(!label_ok("1st category"));
        assert!(!label_ok(&"x".repeat(41)));
    }

    #[test]
    fn a_known_label_passes_even_when_it_is_odd() {
        let mut known: HashSet<String> = HashSet::new();
        known.insert("ai & ml".to_string());
        let report = category_report(&["AI & ML".to_string()], &known, 8);
        assert_eq!(report.kept, vec!["AI & ML".to_string()]);
        assert!(report.dropped.is_empty());
    }

    #[test]
    fn markup_is_dropped_with_a_reason() {
        let report = category_report(&["<b>x</b>".to_string()], &HashSet::new(), 8);
        assert!(report.kept.is_empty());
        assert!(report.dropped[0].contains("not a short label"));
    }

    #[test]
    fn a_string_of_tags_is_split() {
        let list = split_list("chips, semiconductors; export rules\n trade");
        assert_eq!(list, vec!["chips", "semiconductors", "export rules", "trade"]);
    }

    #[test]
    fn model_output_of_the_wrong_type_is_not_a_tag_list() {
        let value = serde_json::json!({"tags": ["x"]});
        assert!(value_list(Some(&value)).is_empty());
        let value = serde_json::json!(["a", 2, null, {"x": 1}]);
        assert_eq!(value_list(Some(&value)), vec!["a".to_string(), "2".to_string()]);
    }
}
