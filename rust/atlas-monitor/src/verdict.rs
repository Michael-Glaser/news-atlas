//! `verdict` — one score for one article, and the terms behind it.
//!
//! Decisions: what a verdict is allowed to contain (a score in 0–10, up to
//! [`MAX_TERMS_KEPT`] terms, a short reason, a short summary, up to
//! [`MAX_ENTITY_CHARS`]-long entity names), that every string is cleaned as
//! untrusted text before it is stored, and how a verdict explains itself to the
//! reader ("why it fired": topic, score, terms).
//!
//! It does not: ask the model ([`crate::judge`] does), decide a threshold
//! ([`crate::spec::Monitor::threshold`] does), or write anything.
//!
//! The one function that reads terms out of *text* rather than out of a model
//! answer is [`query_terms`], and it only ever reads the monitor's own query — the
//! user's words. There is no vocabulary in this file.

use serde_json::{json, Value};

use crate::spec::Monitor;

/// Who scored this article.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scorer {
    /// The local model answered the relevance question.
    Model,
    /// The model could not be asked, so the monitor's own query terms decided.
    QueryTerms,
}

impl Scorer {
    /// The word the alert and the check log store.
    pub fn as_str(self) -> &'static str {
        match self {
            Scorer::Model => "model",
            Scorer::QueryTerms => "query-terms",
        }
    }
}

/// How many terms a verdict keeps — the model is asked for the words that
/// actually matched, and eight is more than a reader can use.
pub const MAX_TERMS_KEPT: usize = 8;

/// A verdict's own reason, as the model said it.
pub const MAX_WHY_CHARS: usize = 200;

/// The "why it fired" line, which also carries the topic, the score and the
/// terms — `monitor.py` stored `analysis` at 300 characters.
pub const MAX_ALERT_WHY_CHARS: usize = 300;

/// The summary column's width.
pub const MAX_SUMMARY_CHARS: usize = 400;

/// The title column's width.
pub const MAX_TITLE_CHARS: usize = 300;

/// One entity name's width; the graph caps names at 120 and an alert needs five
/// of them at most.
pub const MAX_ENTITY_CHARS: usize = 60;

/// How many entities an alert keeps.
pub const MAX_ENTITIES_KEPT: usize = 8;

/// One article's relevance verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub scorer: Scorer,
    /// 0–10 (see [`MAX_SCORE`](crate::spec::MAX_THRESHOLD)).
    pub score: i64,
    /// The words the score is about: the model's own terms when it answered,
    /// else the terms of the monitor's query.
    pub terms: Vec<String>,
    /// Short reason, safe to show and to store.
    pub why: String,
    /// The article in one or two sentences, from the model when it answered.
    pub summary: String,
    /// What the article is about, cleaned and capped.
    pub entities: Vec<String>,
}

impl Verdict {
    /// Build a verdict from untrusted parts: clean and cap every string, drop empty
    /// terms, and keep `score` inside 0–10.
    ///
    /// The clamp is a type invariant, not a decision about a model's answer: a
    /// `Verdict` is documented as holding a 0–10 score and nothing may hold
    /// something else. Whose number it is is decided before one is built — a figure
    /// the model stated outside the scale is refused at the answer boundary
    /// ([`crate::prompt::parse_answer`]), and the fallback's number is this crate's
    /// own arithmetic ([`crate::terms::score_of`]). Nothing may use the clamp to
    /// turn "the model said 42" into a 10 that fires an alert.
    pub fn new(
        scorer: Scorer,
        score: i64,
        terms: Vec<String>,
        why: &str,
        summary: &str,
        entities: Vec<String>,
    ) -> Verdict {
        Verdict {
            scorer,
            score: score.clamp(0, 10),
            terms: clean_terms(terms),
            why: clean_field(why, MAX_WHY_CHARS),
            summary: clean_field(summary, MAX_SUMMARY_CHARS),
            entities: {
                let mut out: Vec<String> = Vec::new();
                for name in entities {
                    let name = clean_field(&name, MAX_ENTITY_CHARS);
                    if !name.is_empty() && !out.iter().any(|kept| kept.eq_ignore_ascii_case(&name)) {
                        out.push(name);
                    }
                    if out.len() >= MAX_ENTITIES_KEPT {
                        break;
                    }
                }
                out
            },
        }
    }

    /// Did this cross the monitor's threshold? A threshold of 0 alerts on any
    /// article the check could judge, which is what "0" means.
    pub fn fired(&self, threshold: i64) -> bool {
        self.score >= threshold.clamp(0, 10)
    }

    /// Why it fired, in one line: the topic, the score, the terms, and the reason
    /// the model gave.
    ///
    /// This is the string the alert stores and the reader sees, so it names the
    /// monitor's own topic and never a category this crate invented.
    ///
    /// Order is by what the reader can least do without. The sentence the model
    /// wrote is the answer to "why did this alert?" and it is composed *before*
    /// the terms, so a long topic and eight terms can drop terms but can never
    /// truncate the reason — which is what the tail-truncated version did.
    pub fn why_text(&self, monitor: &Monitor) -> String {
        let mut out = format!(
            "topic \"{}\" scored {}/10",
            topic_label(monitor),
            self.score
        );
        if self.scorer == Scorer::QueryTerms {
            // The alert row a reader sees carries this sentence and not the scorer
            // column, so the caveat has to be in the sentence — and in the same
            // words [`crate::alert::Why::line`] uses, so the two surfaces agree.
            out.push_str(" without the model");
        }
        if !self.why.is_empty() {
            out.push_str(" — ");
            out.push_str(&self.why);
        }
        if !self.terms.is_empty() {
            // The terms are the evidence line: they go in only when they fit. One
            // term is kept even at the margin, because "which words" is the whole
            // point of asking the model for them.
            let with_terms = format!("{}: {}", out, self.terms.join(", "));
            if with_terms.chars().count() <= MAX_ALERT_WHY_CHARS {
                out = with_terms;
            } else if let Some(first) = self.terms.first() {
                let one = format!("{}: {}", out, first);
                if one.chars().count() <= MAX_ALERT_WHY_CHARS {
                    out = one;
                }
            }
        }
        clean_field(&out, MAX_ALERT_WHY_CHARS)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "scorer": self.scorer.as_str(),
            "score": self.score,
            "terms": self.terms,
            "why": self.why,
            "summary": self.summary,
            "entities": self.entities,
        })
    }
}

/// How much of the monitor's own label the "why" line keeps. The rest of the 300
/// characters belong to the score, the terms and the model's reason — a monitor
/// named with a whole sentence must not crowd out the sentence that explains the
/// alert.
pub const MAX_TOPIC_IN_WHY: usize = 60;

/// The monitor's label, cut to [`MAX_TOPIC_IN_WHY`] with a visible elision so the
/// reader can see that the name was shortened.
fn topic_label(monitor: &Monitor) -> String {
    let label = monitor.label().trim();
    if label.chars().count() > MAX_TOPIC_IN_WHY {
        let mut out: String = label.chars().take(MAX_TOPIC_IN_WHY - 1).collect();
        out.push('…');
        out
    } else {
        label.to_string()
    }
}

/// Untrusted text as one clean line of at most `max` characters.
///
/// Two steps, each with one owner. Control and whitespace runs become a single
/// space first, because `atlas-guard`'s `inline` drops a control character —
/// a newline included — without leaving a separator, which is right for a
/// paragraph of HTML and wrong for a title that has to stay readable. Then the
/// guard removes what must never be stored (invisibles, bidi overrides) and this
/// caps the result.
pub fn clean_field(text: &str, max: usize) -> String {
    let mut spaced = String::with_capacity(text.len().min(max.saturating_mul(4) + 16));
    let mut gap = false;
    for ch in text.chars() {
        if ch.is_control() || ch.is_whitespace() {
            gap = !spaced.is_empty();
            continue;
        }
        if gap {
            spaced.push(' ');
            gap = false;
        }
        spaced.push(ch);
    }
    let inline = atlas_guard::cleantext::inline(&spaced);
    let collapsed = inline.split_whitespace().collect::<Vec<&str>>().join(" ");
    if collapsed.chars().count() > max {
        collapsed.chars().take(max).collect()
    } else {
        collapsed
    }
}

/// Clean, dedupe (case-insensitively) and cap a list of terms.
pub fn clean_terms(terms: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for term in terms {
        let term = clean_field(&term, 40).to_lowercase();
        if term.chars().count() < 2 {
            continue;
        }
        if out.iter().any(|kept| kept == &term) {
            continue;
        }
        out.push(term);
        if out.len() >= MAX_TERMS_KEPT {
            break;
        }
    }
    out
}

/// The terms of a monitor's query: lower-cased, deduped, at least two characters,
/// at most twelve.
///
/// This is `monitor.py::_terms`, and it reads nothing but the caller's string:
/// quotes and commas separate, surrounding punctuation is trimmed. It is the only
/// place in this crate where a score can come from text rather than from the
/// model, and it is fed by the user's query alone.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let normalised = query.replace(['"', ','], " ");
    for raw in normalised.split_whitespace() {
        let term = raw
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    ' ' | '.' | ',' | ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']' | '{' | '}'
                        | '<' | '>' | '\'' | '`' | '"'
                )
            })
            .to_lowercase();
        if term.chars().count() > 1 && !out.iter().any(|kept| kept == &term) {
            out.push(term);
        }
        if out.len() >= 12 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_score_outside_zero_to_ten_stays_inside_the_type_but_is_not_a_models_answer() {
        // This test used to be `a_score_outside_zero_to_ten_is_clamped_not_refused`,
        // and the clamp it asserted was reachable from a model answer: `42` arrived
        // as a 10 with `scorer: model` on the alert. The clamp is now only the
        // type's invariant (a `Verdict` never holds something outside 0–10); the
        // decision moved to `crate::prompt::parse_answer`, which refuses the figure
        // and names it, so no alert can be built from an out-of-range answer. Both
        // halves are asserted here so neither can drift back.
        assert_eq!(Verdict::new(Scorer::Model, 42, vec![], "", "", vec![]).score, 10);
        assert_eq!(Verdict::new(Scorer::Model, -3, vec![], "", "", vec![]).score, 0);
        let refused = crate::prompt::parse_answer("{\"relevance\": 42}").expect("an object");
        assert_eq!(refused.relevance, None, "42 is not a score");
        assert_eq!(refused.refused_score, Some(42), "and the log can say so");
    }

    #[test]
    fn untrusted_parts_are_cleaned_and_capped() {
        let verdict = Verdict::new(
            Scorer::Model,
            7,
            vec!["Chip Export".to_string(), "chip export".to_string(), "a".to_string()],
            "why\nwith\u{7}control",
            "summary",
            vec!["  Acme  Corp ".to_string(), "acme corp".to_string()],
        );
        assert_eq!(verdict.terms, vec!["chip export".to_string()]);
        assert!(!verdict.why.contains('\n'));
        assert_eq!(verdict.entities, vec!["Acme Corp".to_string()]);
    }

    #[test]
    fn the_threshold_decides_and_zero_alerts_on_anything_judged() {
        let verdict = Verdict::new(Scorer::Model, 6, vec![], "", "", vec![]);
        assert!(verdict.fired(6));
        assert!(!verdict.fired(7));
        assert!(verdict.fired(0));
    }

    #[test]
    fn query_terms_are_the_users_own_words() {
        assert_eq!(
            query_terms("chip export, \"rules\" (2026)"),
            vec!["chip", "export", "rules", "2026"]
        );
        assert!(query_terms("").is_empty());
    }

    #[test]
    fn why_it_fired_names_the_topic_the_score_and_the_terms() {
        let monitor = Monitor {
            id: 1,
            name: "Chips".to_string(),
            query: "chip export".to_string(),
            threshold: 6,
            ..Monitor::default()
        };
        let verdict = Verdict::new(
            Scorer::Model,
            8,
            vec!["export".to_string(), "licence".to_string()],
            "new rule this week",
            "",
            vec![],
        );
        let why = verdict.why_text(&monitor);
        assert!(why.starts_with("topic \"Chips\" scored 8/10"));
        assert!(why.contains("export, licence"));
        assert!(why.contains("new rule this week"));
    }

    #[test]
    fn a_long_topic_and_many_terms_never_cut_the_models_reason() {
        // The reason is the sentence the reader opened the alert for. A monitor
        // named with a paragraph and a model that named eight long terms used to
        // push it past 300 characters, where `clean_field` truncated the tail.
        let monitor = Monitor {
            id: 1,
            name: "n".repeat(MAX_NAME_CHARS_LONG),
            query: "chip export".to_string(),
            threshold: 6,
            ..Monitor::default()
        };
        let terms: Vec<String> = (0..MAX_TERMS_KEPT)
            .map(|index| format!("term{}andmore", index))
            .collect();
        let reason = "r".repeat(MAX_WHY_CHARS);
        let verdict = Verdict::new(Scorer::Model, 9, terms, &reason, "", vec![]);
        let why = verdict.why_text(&monitor);
        assert!(
            why.chars().count() <= MAX_ALERT_WHY_CHARS,
            "the line still fits the column: {} chars",
            why.chars().count()
        );
        assert!(
            why.contains(&reason),
            "the model's own reason survives whole: {why}"
        );
        assert!(why.contains("…"), "the topic says it was shortened: {why}");
    }

    /// The name column's width, spelled out so the test above reads without
    /// importing `spec` for one number.
    const MAX_NAME_CHARS_LONG: usize = 120;
}
