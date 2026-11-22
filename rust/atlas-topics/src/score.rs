//! What a keyword is worth, and when a topic is relevant at all.
//!
//! Decides: title 3.0, summary 1.5, body 0.5; the 0.25 bonus per matched
//! keyword; that a keyword counts once, at the best field it appears in; the
//! `min_matches` threshold; that any exclude hit ends the topic immediately; and
//! the one [`Verdict`] — a [`Kind`] plus the numbers behind it — that the three
//! callers report. Because one struct carries `matched`, `terms`, `excluded_by`,
//! the raw score and the hit score together, no answer can fill one of them and
//! leave another empty for the same pair.
//!
//! Does not decide: how a keyword is compiled or searched (`crate::terms`), or
//! how a verdict is written out (`crate::matching`, `crate::ranking`, and the
//! `score` answer below, which is the bare relevance of one pair).

use atlas_lib::json::{self, Json};

use crate::model::{article_of, blob_for, one_topic, Fields, Topic};
use crate::terms;
use crate::wire::{write_reason, write_string_list};

/// What a keyword is worth at each field, in Python's order of preference.
pub(crate) const WEIGHT: [f64; 3] = [3.0, 1.5, 0.5];
pub(crate) const FIELD_NAME: [&str; 3] = ["title", "summary", "content"];
/// Every matched keyword also adds this to the score.
pub(crate) const BONUS: f64 = 0.25;
/// `reason` is cut at 300 characters, like the Python column.
pub(crate) const REASON_MAX: usize = 300;
/// Evidence is a nicety: bound the work and the output.
pub(crate) const EVIDENCE_MAX: usize = 32;
pub(crate) const EVIDENCE_BUDGET: usize = 4_096;

/// What one topic did with one article.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// No exclude term hit and at least `min_matches` keywords were found.
    Hit,
    /// An exclude term hit, so the topic ends there (`score` and `terms_hit`
    /// stay at zero: the Python never scored an excluded topic either).
    Excluded,
    /// Keywords were found, but not enough of them.
    Below,
}

/// The verdict for one (topic, article) pair. Every field the answers report is
/// on this one struct, so the three subcommands cannot disagree about what
/// happened: `matched` and `terms` are filled together or not at all, and an
/// exclusion names the term that caused it.
pub(crate) struct Verdict {
    pub kind: Kind,
    /// What a hit is worth; zero unless `kind` is `Hit`.
    pub score: f64,
    /// What the keywords were worth whether or not the pair is a hit.
    pub raw: f64,
    pub terms_hit: usize,
    /// One entry per matched keyword, as `"term (field)"`.
    pub matched: Vec<String>,
    /// The keyword spellings behind `matched`, in the same order.
    pub terms: Vec<String>,
    /// The exclude terms that hit, which is what a reader has to remove to
    /// unblock the topic. Empty for every verdict but `Excluded`.
    pub excluded_by: Vec<String>,
}

/// Score one topic against one article's folded fields.
pub(crate) fn evaluate(topic: &Topic, fields: &Fields, blob: Option<&str>) -> Verdict {
    if let Some(blob) = blob {
        // Every exclude term that hits is named, not just the first: a reader
        // who removes one and keeps the other would otherwise be back here.
        let excluded_by: Vec<String> = topic
            .exclude
            .iter()
            .filter(|(_, compiled)| terms::search(blob, compiled))
            .map(|(text, _)| text.clone())
            .collect();
        if !excluded_by.is_empty() {
            return Verdict {
                kind: Kind::Excluded,
                score: 0.0,
                raw: 0.0,
                terms_hit: 0,
                matched: Vec::new(),
                terms: Vec::new(),
                excluded_by,
            };
        }
    }
    let mut score = 0.0f64;
    let mut matched: Vec<String> = Vec::new();
    let mut terms: Vec<String> = Vec::new();
    for (term, compiled) in &topic.terms {
        // A keyword counts once, at the field where it hits best:
        // title before summary before body.
        for index in 0..3 {
            if terms::search(fields.field(index), compiled) {
                score += WEIGHT[index];
                matched.push(format!("{} ({})", term, FIELD_NAME[index]));
                terms.push(term.clone());
                break;
            }
        }
    }
    let terms_hit = terms.len();
    let raw = round3(score + terms_hit as f64 * BONUS);
    let kind = if terms_hit >= topic.min {
        Kind::Hit
    } else {
        Kind::Below
    };
    Verdict {
        kind,
        score: if kind == Kind::Hit { raw } else { 0.0 },
        raw,
        terms_hit,
        matched,
        terms,
        excluded_by: Vec::new(),
    }
}

/// `round(x, 3)` for sums of 0.25/0.5/1.5/3.0 — exact in binary anyway,
/// rounded here so the value matches the Python `round(…, 3)`.
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

pub fn score(root: &Json) -> Result<String, String> {
    let topic = one_topic(root)?;
    let article = article_of(root)?;
    let fields = Fields::build(&article);
    let blob = blob_for(std::slice::from_ref(&topic), &fields);
    let verdict = evaluate(&topic, &fields, blob.as_deref());
    let hit = verdict.kind == Kind::Hit;
    let mut out = String::with_capacity(256);
    out.push_str("{\"id\":");
    json::write(&mut out, &topic.id);
    out.push_str(",\"hit\":");
    out.push_str(if hit { "true" } else { "false" });
    out.push_str(",\"score\":");
    out.push_str(&json::fmt_score(verdict.score));
    out.push_str(",\"raw_score\":");
    out.push_str(&json::fmt_score(verdict.raw));
    out.push_str(",\"excluded\":");
    out.push_str(if verdict.kind == Kind::Excluded {
        "true"
    } else {
        "false"
    });
    out.push_str(",\"terms_hit\":");
    out.push_str(&json::num_str(verdict.terms_hit as f64));
    out.push_str(",\"min_matches\":");
    out.push_str(&json::num_str(topic.min as f64));
    write_string_list(&mut out, "matched", &verdict.matched);
    write_string_list(&mut out, "terms", &verdict.terms);
    write_string_list(&mut out, "excluded_by", &verdict.excluded_by);
    out.push_str(",\"reason\":");
    write_reason(&mut out, &verdict.matched);
    out.push('}');
    Ok(out)
}
