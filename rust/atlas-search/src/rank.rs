//! Relevance order, through `atlas-core`'s similarity primitive.
//!
//! The engine's order is one opinion; the model asked for specific words, so
//! results are ordered by how much of the query they contain. The arithmetic is
//! [`atlas_core::similar::compute`] — the same primitive the graph and topic
//! crates pair documents with — fed with the query as document 0 and every hit
//! after it. Each pair `(0, hit)` is that hit's cosine against the query, so one
//! call gives relevance *and* (for free) the same-story pairs among the hits.
//!
//! What this module owns: the vector alphabet (a lowercase word run), the weight
//! (term frequency), and the tie-break (the engine's own position). What it does
//! not own: the cosine, the pair selection or the threshold. Those are
//! `atlas-core`'s, called and never re-derived.
//!
//! It deliberately does not use `atlas-text`'s `tokenize`: that function decides
//! which words an article is *about* (stopwords out, four-word phrases kept) and
//! the archive's search depends on it. A relevance vector needs a stable
//! alphabet, not a curated one — a stopword shared by a query and every hit
//! contributes the same to each cosine and cannot reorder them.

use std::collections::HashMap;

use atlas_core::similar::{compute, Doc};

/// Documents one call ranks. A result page cannot offer more, and a hostile one
/// cannot make the pair scan quadratic.
pub const MAX_DOCS: usize = 120;

/// Relevance for every text, in the order given: cosine against the query,
/// 0.0 … 1.0. A text that shares no word with the query scores 0.0, which is why
/// the engine's order survives as the tie-break.
pub fn scores(query: &str, texts: &[String]) -> Vec<f64> {
    let mut out = vec![0.0f64; texts.len()];
    let query_doc = unit_vector(0, query);
    let mut docs: Vec<Doc> = Vec::with_capacity(texts.len().min(MAX_DOCS) + 1);
    docs.push(query_doc);
    for (index, text) in texts.iter().take(MAX_DOCS).enumerate() {
        docs.push(unit_vector(index + 1, text));
    }
    if docs.len() <= 1 {
        return out;
    }
    // Every pair, no threshold: a hit that scores 0.0 must keep its place in the
    // answer rather than silently vanish from it.
    let pairs = compute(&docs, docs.len(), 0.0);
    for (left, right, score) in pairs {
        let hit = match (left, right) {
            (0, other) => other,
            (other, 0) => other,
            _ => continue,
        };
        if hit == 0 || hit > out.len() {
            continue;
        }
        let slot = &mut out[hit - 1];
        if score > *slot {
            *slot = score;
        }
    }
    out
}

/// Order indices by score (descending), then by the position the engine gave them
/// (ascending). A stable sort, so nothing to decide beyond those two keys.
pub fn order(scores: &[f64], ranks: &[usize]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..scores.len()).collect();
    indices.sort_by(|left, right| {
        let by_score = scores[*right]
            .partial_cmp(&scores[*left])
            .unwrap_or(std::cmp::Ordering::Equal);
        by_score.then_with(|| {
            ranks
                .get(*left)
                .copied()
                .unwrap_or(0)
                .cmp(&ranks.get(*right).copied().unwrap_or(0))
        })
    });
    indices
}

/// One document as a unit-length sparse vector.
///
/// `atlas_core::similar::compute` documents its precondition — "with unit-length
/// vectors the dot product already is the cosine" — and this is that
/// normalisation, the same arithmetic `similar::run` performs while reading its
/// JSON input. The weight is the term frequency, clamped to a sane length so one
/// enormous snippet cannot dominate the pair scan.
fn unit_vector(id: usize, text: &str) -> Doc {
    let mut counts: HashMap<String, f64> = HashMap::new();
    for token in tokens(text) {
        *counts.entry(token).or_insert(0.0) += 1.0;
    }
    let sum_sq: f64 = counts.values().map(|value| value * value).sum();
    let norm = sum_sq.sqrt();
    let mut unit: Vec<(String, f64)> = if norm > 0.0 && norm.is_finite() {
        counts
            .into_iter()
            .map(|(token, value)| (token, value / norm))
            .collect()
    } else {
        Vec::new()
    };
    unit.sort_by(|left, right| left.0.cmp(&right.0));
    Doc {
        id_json: id.to_string(),
        unit,
    }
}

/// The vector alphabet: lowercase runs of letters and digits.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for low in ch.to_lowercase() {
                current.push(low);
            }
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}
