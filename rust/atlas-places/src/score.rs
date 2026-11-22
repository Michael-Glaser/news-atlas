//! Ranking: which place the text is actually about, and how sure we are.
//!
//! Decides, in this order: how much a mention is worth in the title, the summary
//! and the body, the per-zone caps that stop repetition from winning, the floor
//! below which nothing is located, which of two same-named cities the text means
//! (`crate::namesake`), the city-beats-its-own-country promotion, the tie-break
//! order, and the confidence the answer reports.
//!
//! Does not decide: how a mention is found (that is `crate::scan`) or how the
//! answer is written out (that is `crate::report`). Every constant that scores
//! anything lives here, so one file answers "why did this place win?".

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::index::{index, Kind};
use crate::namesake;
use crate::outlet::outlet_country;
use crate::scan::scan;

/// How much body text is scanned at all.
pub(crate) const BODY_CHARS: usize = 20000;
/// Zone weights and per-zone mention caps: one title hit outweighs any amount
/// of body repetition.
const ZONE_WEIGHT: [f64; 3] = [6.0, 3.0, 1.0];
const ZONE_CAP: [f64; 3] = [2.0, 2.0, 4.0];
/// A body-only place has to be said twice.
const MIN_BODY_MENTIONS: usize = 2;
/// Below this nothing is located.
const MIN_SCORE: f64 = 2.0;
/// Ambiguous name without supporting context.
pub(crate) const AMBIG_WEIGHT: f64 = 0.15;
/// Country adjective ("French officials").
pub(crate) const ADJ_WEIGHT: f64 = 0.25;
/// A city beats its own country at equal weight.
const CITY_BONUS: f64 = 1.3;

/// The first `limit` characters of `s`.
fn truncate_chars(s: &str, limit: usize) -> &str {
    match s.char_indices().nth(limit) {
        Some((b, _)) => &s[..b],
        None => s,
    }
}

/// One place in one article: what each zone said about it, what it scored, and
/// — when it did not make it — the one phrase that says why.
pub(crate) struct Cand {
    pub(crate) kind: Kind,
    pub(crate) place: &'static str,
    pub(crate) iso: &'static str,
    pub(crate) lat: f64,
    pub(crate) lon: f64,
    pub(crate) w: [f64; 3],
    pub(crate) n: [usize; 3],
    pub(crate) pw: f64,
    pub(crate) adj: usize,
    pub(crate) pos: (usize, usize),
    pub(crate) score: f64,
    pub(crate) rank: f64,
    pub(crate) note: &'static str,
    /// The highest confidence this candidate may report: `1.0` unless the answer
    /// rests on something weaker than the text (`crate::namesake`).
    pub(crate) conf_cap: f64,
}

/// `_score_all`: every candidate for one article.
pub(crate) fn score_all(
    title: &str,
    summary: &str,
    text: &str,
    url: &str,
    publisher: &str,
) -> (Vec<usize>, Vec<Cand>) {
    let idx = index();
    let body = truncate_chars(text, BODY_CHARS);
    let mut cands: Vec<Cand> = Vec::new();
    let mut by_key: HashMap<(u8, &'static str, &'static str), usize> = HashMap::new();

    for (zi, zone) in [title, summary, body].iter().enumerate() {
        let hits = scan(zone, idx);
        for (&(kind, place, iso), slot) in hits.iter() {
            let ci = match by_key.get(&(kind, place, iso)) {
                Some(&ci) => ci,
                None => {
                    let fresh = cands.len();
                    cands.push(Cand {
                        kind: slot.kind,
                        place,
                        iso,
                        lat: slot.lat,
                        lon: slot.lon,
                        w: [0.0; 3],
                        n: [0; 3],
                        pw: 0.0,
                        adj: 0,
                        pos: (zi, slot.pos),
                        score: 0.0,
                        rank: 0.0,
                        note: "",
                        conf_cap: 1.0,
                    });
                    by_key.insert((kind, place, iso), fresh);
                    fresh
                }
            };
            let c = &mut cands[ci];
            c.w[zi] = slot.w;
            c.n[zi] = slot.n;
            c.pw += slot.pw;
            c.adj += slot.adj;
            if (zi, slot.pos) < c.pos {
                c.pos = (zi, slot.pos);
            }
        }
    }

    for c in cands.iter_mut() {
        let mut score = 0.0;
        for zi in 0..3 {
            score += ZONE_WEIGHT[zi] * c.w[zi].min(ZONE_CAP[zi]);
        }
        c.score = score;
        c.rank = score * if c.kind == Kind::City { CITY_BONUS } else { 1.0 };
        if c.n[0] == 0 && c.n[1] == 0 && c.n[2] < MIN_BODY_MENTIONS {
            c.note = "body mention only";
        } else if c.pw == 0.0 && c.adj < 2 {
            c.note = "adjectival only";
        } else if c.score < MIN_SCORE {
            c.note = "below threshold";
        }
    }

    // A name that two countries use is two places: the rest of the text, or failing
    // that nothing, decides between them. This runs before ranking so a namesake
    // the text does not mean cannot win on its table position.
    namesake::resolve(&mut cands, outlet_country(url, publisher));

    let mut ranked: Vec<usize> = (0..cands.len())
        .filter(|&i| cands[i].note.is_empty())
        .collect();
    ranked.sort_by(|&a, &b| rank_cmp(&cands[a], &cands[b]));

    // A city outranks its own country even when the country was named more
    // often: "South Korea" is the crate, Seoul is where it happened.
    if let Some(&top) = ranked.first() {
        if cands[top].kind == Kind::Country {
            let top_iso = cands[top].iso;
            let floor = cands[top].score * 0.5;
            let found = (1..ranked.len()).find(|&k| {
                let c = &cands[ranked[k]];
                c.kind == Kind::City && c.iso == top_iso && c.score >= floor
            });
            if let Some(at) = found {
                let city = ranked.remove(at);
                ranked.insert(0, city);
            }
        }
    }

    (ranked, cands)
}

/// `_rank_key`: (-rank, -score, pos, place).
fn rank_cmp(a: &Cand, b: &Cand) -> Ordering {
    b.rank
        .partial_cmp(&a.rank)
        .unwrap_or(Ordering::Equal)
        .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
        .then_with(|| a.pos.cmp(&b.pos))
        .then_with(|| a.place.cmp(b.place))
}

pub(crate) fn rows_order(cands: &[Cand]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| rank_cmp(&cands[a], &cands[b]));
    order
}

/// Python's `round(x, 2)`: correctly rounded decimal, ties to even.
pub(crate) fn round2(v: f64) -> f64 {
    if !v.is_finite() || v == 0.0 {
        return v;
    }
    format!("{:.2}", v).parse::<f64>().unwrap_or(v)
}

/// `_conf`.
pub(crate) fn conf_of(c: &Cand, rival: f64) -> f64 {
    let mut base = c.score / (c.score + rival + 0.5);
    if c.n[0] > 0 {
        base = (base + 0.1).min(1.0);
    }
    round2(base.min(1.0))
}
