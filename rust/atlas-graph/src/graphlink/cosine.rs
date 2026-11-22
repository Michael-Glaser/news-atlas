//! The text-similarity half of `recompute_links()`: one vector per article,
//! `cosine = dot / (|a| |b|)` over an inverted index, `top_per_doc` partners per
//! article — the routine `atlas-core similar` runs.
//!
//! It is reproduced here rather than called as a subcommand because a library may
//! not spawn a process, and `atlas-core` ships this routine only behind its
//! binary's `similar` subcommand (it has no `lib` target). The loop is line for
//! line the same: `MAX_TOKENS`, `MAX_POSTINGS`, the `> 1.0` clamp, `min_score`
//! applied before the per-document cap, and the `(score desc, doc asc)` selection.
//! `db_tests` checks the port against the binary's own answer.
//!
//! # One difference: the order the dot products are summed in
//!
//! `postings` is a `HashMap`, so walking it directly summed a pair's dot product
//! in an order that changed with the map's per-instance hash seed — the same
//! vectors could differ in the last bit from one run to the next, which is enough
//! to land on either side of a 4-decimal rounding boundary and hand two runs of
//! the same rebuild two different `article_links` rows. The tokens are walked in
//! sorted order instead: same arithmetic, one fixed order, so the same input is
//! the same output twice in a row.

use std::collections::{HashMap, HashSet};

pub const MAX_TOKENS: usize = 5_000;
/// Posting lists longer than this are skipped: a token that appears in that
/// many documents is not informative and would cost quadratic work.
pub const MAX_POSTINGS: usize = 10_000;

/// `(a, b, score)` with `a < b`, sorted by score desc then *document index* — the
/// order `atlas-core similar` answers in. The index (not the id) matters: the
/// caller's loops keep the first pair they meet, so a score tie is broken the way
/// the Python bridge sees it.
pub fn compute(
    docs: &[(i64, Vec<(String, f64)>)],
    top_per_doc: usize,
    min_score: f64,
) -> Vec<(i64, i64, f64)> {
    // unit-length vectors, tokens sorted, exactly as `similar::run` builds them
    let mut unit: Vec<Vec<(String, f64)>> = Vec::with_capacity(docs.len());
    let mut ids: Vec<i64> = Vec::with_capacity(docs.len());
    for (id, tokens) in docs {
        if tokens.is_empty() || tokens.len() > MAX_TOKENS {
            continue;
        }
        let mut sum_sq = 0.0f64;
        for (_token, weight) in tokens {
            sum_sq += weight * weight;
        }
        let norm = sum_sq.sqrt();
        if !(norm > 0.0) || !norm.is_finite() {
            continue;
        }
        let mut sorted: Vec<(String, f64)> = tokens
            .iter()
            .map(|(token, weight)| (token.clone(), weight / norm))
            .collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        unit.push(sorted);
        ids.push(*id);
    }

    // Inverted index: token -> (doc, unit weight).
    let mut postings: HashMap<&str, Vec<(usize, f64)>> = HashMap::new();
    for (index, doc) in unit.iter().enumerate() {
        for (token, weight) in doc {
            postings
                .entry(token.as_str())
                .or_insert_with(Vec::new)
                .push((index, *weight));
        }
    }
    // Sorted, not `postings.values()`: the sum below must add the same terms in
    // the same order on every run (see the module note), and a `HashMap` does not
    // promise one. The keys are already unique, so this costs one sort per call.
    let mut tokens: Vec<&str> = postings.keys().copied().collect();
    tokens.sort_unstable();
    let mut dots: HashMap<(usize, usize), f64> = HashMap::new();
    for token in tokens {
        let list = &postings[token];
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
    let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); unit.len()];
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
        for (other, _) in list.iter().take(top_per_doc) {
            let key = if index < *other {
                (index, *other)
            } else {
                (*other, index)
            };
            selected.insert(key);
        }
    }
    let mut out: Vec<(usize, usize, f64)> = selected
        .into_iter()
        .map(|(left, right)| {
            let raw = dots.get(&(left, right)).copied().unwrap_or(0.0);
            let clamped = if raw > 1.0 { 1.0 } else { raw };
            (left, right, clamped)
        })
        .collect();
    out.sort_by(|x, y| {
        y.2.partial_cmp(&x.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.0.cmp(&y.0))
            .then_with(|| x.1.cmp(&y.1))
    });
    out.into_iter()
        .map(|(left, right, score)| (ids[left], ids[right], score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pair worth connecting at the default floor.
    fn docs() -> Vec<(i64, Vec<(String, f64)>)> {
        vec![
            (
                10,
                vec![
                    ("alpha".to_string(), 1.0),
                    ("beta".to_string(), 0.5),
                    ("gamma".to_string(), 0.25),
                ],
            ),
            (
                20,
                vec![
                    ("alpha".to_string(), 0.8),
                    ("beta".to_string(), 0.6),
                    ("delta".to_string(), 0.3),
                ],
            ),
            (
                30,
                vec![("alpha".to_string(), 1.0), ("delta".to_string(), 0.9)],
            ),
        ]
    }

    #[test]
    fn the_same_vectors_give_the_same_pairs_twice_in_a_row() {
        // A long, mixed-magnitude posting list: summing these terms in two
        // different orders gives two different doubles, and the sums are compared
        // exactly here — not through the 4-decimal formatter — so an unordered
        // accumulation fails this test.
        let mut long: Vec<(i64, Vec<(String, f64)>)> = Vec::new();
        for doc in 0..12i64 {
            let mut tokens: Vec<(String, f64)> = Vec::new();
            for token in 0..400usize {
                let magnitude = if token % 7 == 0 { 1e-3 } else { 1.0 + token as f64 };
                let weight = magnitude / (1.0 + (doc as f64));
                tokens.push((format!("t{:04}", token), weight));
            }
            tokens.push((format!("only{}", doc), 0.5));
            long.push((doc, tokens));
        }
        let first = compute(&long, 5, 0.05);
        assert!(!first.is_empty(), "the vectors must actually be compared");
        for _ in 0..4 {
            let again = compute(&long, 5, 0.05);
            assert_eq!(
                first, again,
                "the same vectors must sum in the same order every time"
            );
        }
        assert_eq!(
            compute(&docs(), 5, 0.05),
            compute(&docs(), 5, 0.05),
            "a small vector set is stable too"
        );
    }

    #[test]
    fn a_pair_is_reported_once_with_the_lower_copy_first() {
        let pairs = compute(&docs(), 5, 0.05);
        assert!(!pairs.is_empty());
        let mut seen: HashSet<(i64, i64)> = HashSet::new();
        for (a, b, score) in &pairs {
            assert!(a < b, "the lower id comes first: ({}, {})", a, b);
            assert!(seen.insert((*a, *b)), "a pair is reported once");
            assert!(score.is_finite() && (0.0..=1.0).contains(score), "{}", score);
        }
    }

    #[test]
    fn a_document_keeps_only_its_strongest_partners() {
        // The strongest pair of the three is (10, 20); the second strongest is
        // (20, 30). With one partner per document, 10 picks 20, 30 picks 20 and
        // 20 picks 10 — the answer is the union of those picks, so (10, 30) never
        // appears and a document's own list is what bounds the work.
        let capped = compute(&docs(), 1, 0.0);
        assert_eq!(capped.iter().map(|(a, b, _)| (*a, *b)).collect::<Vec<_>>(), vec![
            (10, 20),
            (20, 30)
        ]);
    }

    #[test]
    fn an_empty_or_over_long_vector_is_skipped_not_guessed() {
        let mut oversized: Vec<(String, f64)> = Vec::new();
        for token in 0..(MAX_TOKENS + 1) {
            oversized.push((format!("w{}", token), 1.0));
        }
        let input = vec![
            (1i64, Vec::new()),
            (2i64, vec![("zero".to_string(), 0.0)]),
            (3i64, oversized),
            (4i64, vec![("alpha".to_string(), 1.0)]),
        ];
        assert!(
            compute(&input, 5, 0.0).is_empty(),
            "no vector, no pair: nothing to compare, nothing invented"
        );
    }

    #[test]
    fn a_score_above_one_is_clamped_to_one() {
        // Unit weights on the same token: the dot is exactly 1.0, never above.
        let input = vec![
            (1i64, vec![("alpha".to_string(), 2.0)]),
            (2i64, vec![("alpha".to_string(), 2.0)]),
        ];
        for (_a, _b, score) in compute(&input, 4, 0.0) {
            assert!(score <= 1.0, "{}", score);
        }
    }
}
