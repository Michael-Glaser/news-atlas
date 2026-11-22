//! `similar` — cosine similarity over sparse token vectors.
//!
//! Decides how two unit vectors are compared (dot product), which pairs are
//! worth returning (a per-document top list and a minimum score), how the pairs
//! are ordered, and that a pair is reported once, with the lower id first.
//!
//! It does not build the vectors, choose the threshold or hold anything between
//! calls: the caller supplies `Doc`s and the counts, and this module answers with
//! pairs. Posting lists longer than `MAX_POSTINGS` are skipped because a token in
//! that many documents is not informative and would cost quadratic work.

use std::collections::{HashMap, HashSet};

use crate::json::{self, Json};

const MAX_TOKENS: usize = 5000;
/// Posting lists longer than this are skipped: a token that appears in that
/// many documents is not informative and would cost quadratic work.
const MAX_POSTINGS: usize = 10_000;

pub struct Doc {
    pub id_json: String,
    /// Unit-length sparse vector, tokens sorted.
    pub unit: Vec<(String, f64)>,
}

pub fn run(root: &Json) -> Result<String, String> {
    let raw_docs = match root.get("docs").and_then(Json::as_arr) {
        Some(items) => items,
        None => return Err("missing \"docs\" array".to_string()),
    };
    let top_per_doc = root
        .get("top_per_doc")
        .and_then(Json::as_usize)
        .unwrap_or(8);
    let min_score = root.get("min_score").and_then(Json::as_f64).unwrap_or(0.25);

    let mut docs: Vec<Doc> = Vec::with_capacity(raw_docs.len());
    for (index, raw) in raw_docs.iter().enumerate() {
        let id_json = match raw.get("id") {
            Some(id) => json::to_string(id),
            None => index.to_string(),
        };
        let mut weights: HashMap<&str, f64> = HashMap::new();
        if let Some(Json::Obj(items)) = raw.get("tokens") {
            for (token, weight) in items {
                if token.is_empty() {
                    continue;
                }
                if let Json::Num(value) = weight {
                    if value.is_finite() {
                        let slot = weights.entry(token.as_str()).or_insert(0.0);
                        *slot += *value;
                    }
                }
            }
        }
        if weights.is_empty() || weights.len() > MAX_TOKENS {
            continue;
        }
        let mut sum_sq = 0.0f64;
        for value in weights.values() {
            sum_sq += value * value;
        }
        let norm = sum_sq.sqrt();
        if !(norm > 0.0) || !norm.is_finite() {
            continue;
        }
        let mut unit: Vec<(String, f64)> = weights
            .into_iter()
            .map(|(token, value)| (token.to_string(), value / norm))
            .collect();
        unit.sort_by(|a, b| a.0.cmp(&b.0));
        docs.push(Doc { id_json, unit });
    }

    let pairs = compute(&docs, top_per_doc, min_score);

    let mut out = String::from("{\"pairs\":[");
    for (i, (a, b, score)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"a\":");
        out.push_str(docs.get(*a).map(|d| d.id_json.as_str()).unwrap_or("null"));
        out.push_str(",\"b\":");
        out.push_str(docs.get(*b).map(|d| d.id_json.as_str()).unwrap_or("null"));
        out.push_str(",\"score\":");
        out.push_str(&json::fmt_score(*score));
        out.push('}');
    }
    out.push_str("]}");
    Ok(out)
}

/// Accepted pairs, de-duplicated and sorted by score desc, then index.
pub fn compute(docs: &[Doc], top_per_doc: usize, min_score: f64) -> Vec<(usize, usize, f64)> {
    // Inverted index: token -> (doc, unit weight).
    let mut postings: HashMap<&str, Vec<(usize, f64)>> = HashMap::new();
    for (index, doc) in docs.iter().enumerate() {
        for (token, weight) in &doc.unit {
            postings
                .entry(token.as_str())
                .or_insert_with(Vec::new)
                .push((index, *weight));
        }
    }

    let mut dots: HashMap<(usize, usize), f64> = HashMap::new();
    for list in postings.values() {
        if list.len() < 2 || list.len() > MAX_POSTINGS {
            continue;
        }
        for x in 0..list.len() {
            let (left_doc, left_weight) = list[x];
            for y in (x + 1)..list.len() {
                let (right_doc, right_weight) = list[y];
                if left_doc == right_doc {
                    continue;
                }
                let key = if left_doc < right_doc {
                    (left_doc, right_doc)
                } else {
                    (right_doc, left_doc)
                };
                *dots.entry(key).or_insert(0.0) += left_weight * right_weight;
            }
        }
    }
    if dots.is_empty() {
        return Vec::new();
    }

    // With unit-length vectors the dot product already is the cosine.
    let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); docs.len()];
    for (pair, score) in dots.iter() {
        if !score.is_finite() || *score < min_score {
            continue;
        }
        let clamped = if *score > 1.0 { 1.0 } else { *score };
        adjacency[pair.0].push((pair.1, clamped));
        adjacency[pair.1].push((pair.0, clamped));
    }

    let mut selected: HashSet<(usize, usize)> = HashSet::new();
    for index in 0..adjacency.len() {
        let list = &mut adjacency[index];
        if list.is_empty() {
            continue;
        }
        list.sort_by(|x, y| {
            y.1.partial_cmp(&x.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| x.0.cmp(&y.0))
        });
        let keep: Vec<usize> = list
            .iter()
            .take(top_per_doc)
            .map(|(other, _)| *other)
            .collect();
        for other in keep {
            let key = if index < other {
                (index, other)
            } else {
                (other, index)
            };
            selected.insert(key);
        }
    }

    let mut out: Vec<(usize, usize, f64)> = selected
        .into_iter()
        .map(|(a, b)| {
            let raw = dots.get(&(a, b)).copied().unwrap_or(0.0);
            let clamped = if raw > 1.0 { 1.0 } else { raw };
            (a, b, clamped)
        })
        .collect();
    out.sort_by(|x, y| {
        y.2.partial_cmp(&x.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.0.cmp(&y.0))
            .then_with(|| x.1.cmp(&y.1))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn doc(id: &str, tokens: &[(&str, f64)]) -> Doc {
        Doc {
            id_json: id.to_string(),
            unit: tokens.iter().map(|(t, w)| ((*t).to_string(), *w)).collect(),
        }
    }

    #[test]
    fn cosine_scores_shared_vectors() {
        let docs = vec![
            doc("1", &[("nvidia", 1.0)]),
            doc("2", &[("nvidia", 0.8), ("chip", 0.6)]),
            doc("3", &[("gold", 1.0)]),
        ];
        let pairs = compute(&docs, 8, 0.25);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, 0);
        assert_eq!(pairs[0].1, 1);
        assert!((pairs[0].2 - 0.8).abs() < 1e-9);
    }

    #[test]
    fn top_per_doc_limits_and_dedupes_pairs() {
        let docs = vec![
            doc("1", &[("shared", 1.0)]),
            doc("2", &[("shared", 1.0)]),
            doc("3", &[("shared", 1.0)]),
        ];
        let pairs = compute(&docs, 1, 0.1);
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().all(|(a, b, score)| a != b && *score > 0.99));
    }

    #[test]
    fn similar_run_emits_doc_ids_and_scores() {
        let input = "{\"docs\":[{\"id\":1,\"tokens\":{\"nvidia\":0.9,\"chip\":0.2}},\
                      {\"id\":2,\"tokens\":{\"nvidia\":0.7,\"chip\":0.3}}],\
                      \"top_per_doc\":8,\"min_score\":0.25}";
        let root = json::parse(input).expect("valid json");
        let out = run(&root).expect("similar ok");
        assert!(out.starts_with("{\"pairs\":[{\"a\":1,\"b\":2,\"score\":0.9"), "got {}", out);
        let parsed = json::parse(&out).expect("output is json");
        let pairs = parsed
            .get("pairs")
            .and_then(json::Json::as_arr)
            .expect("pairs");
        assert_eq!(pairs.len(), 1);
    }
}
