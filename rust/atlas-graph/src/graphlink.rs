//! Article-to-article connections and story threads — `recompute_links()`,
//! `prune_links()` and `recompute_threads()` of `server/graph.py`.
//!
//! The scoring is Python's, number for number: cosine over IDF-weighted shared
//! entities, shrunk when only one name is shared (`shared/(shared+1.5)`, or a
//! flat `0.5`), plus `0.4 ×` the wording cosine; then the four ways a pair can
//! still qualify (two names, one very strong name, strong wording, same topic)
//! and the hard per-article cap of [`MAX_LINKS_PER_ARTICLE`].
//!
//! The pair work is driven by posting lists on both sides — the entity inverted
//! index here and [`cosine`] for the wording — so 400 articles and 4 000 entities
//! never approach `n²`.
//!
//! # The map
//!
//! * this file — the link scoring, `prune_links`, and the thread rebuild.
//! * [`cosine`] — the wording half (token runs, IDF weights, per-document cap).
//! * [`tests`] — the rebuild's own numbers and the order/title/score decisions
//!   that hold them; the cross-module proof against `server/graph.py` is in
//!   `db_tests`.

use std::collections::{BTreeMap, HashMap, HashSet};

use atlas_store::{placeholders, SqlValue, Store, StoreError};
use rusqlite::Connection;

use crate::graphdb::{
    exec, fail, fraction, idf_map, int, jnum, jobj, jstr, opt_f64, opt_i64, opt_usize, query, real,
    round4, same_value, text, window_ago, Caches, Context, Idf, IDF_FALLBACK, LINK_DAYS,
    MAX_LINKS_PER_ARTICLE, MIN_LINK_SCORE, MIN_TEXT_ONLY, MIN_TEXT_RESCUE,
    MIN_TEXT_WITHOUT_ENTITIES, TEXT_DF_MIN, TEXT_PAIR_FLOOR,
};
use crate::{
    choose_label, strip_count_suffix, thread_words, Article, Lex, ThreadCtx, Uf,
    DEFAULT_MAX_SIZE, DEFAULT_MIN_SCORE,
};

/// The wording half of the pair work: cosine over the article vectors
/// (`_TOKEN4_RE` vocabulary, IDF weights, the per-document cap). Its own file
/// because it is its own decision, and the one place in the layer where a score
/// is arithmetic rather than a relation.
pub(crate) mod cosine;

/// Is this character inside `[a-zà-ÿа-я]` — the alphabet `_TOKEN4_RE` accepts?
fn word_char(ch: char) -> bool {
    matches!(ch, 'a'..='z')
        || ('\u{e0}'..='\u{ff}').contains(&ch)
        || ('\u{430}'..='\u{44f}').contains(&ch)
}

/// `_TOKEN4_RE` — `[a-zà-ÿа-я]{4,}` over the already lower-cased text.
///
/// The quantifier counts *characters*, as Python's `re` does; a run is measured
/// in `chars().count()`, not in bytes. Byte length would let a three-character
/// run of two-byte letters through (`"èsè"` is 6 bytes, 3 characters), which is
/// a word Python's regex never yields — and a wording vector with one extra word
/// connects two articles Python keeps apart.
fn token4_runs<'a>(low: &'a str, out: &mut Vec<&'a str>) {
    let mut start: Option<usize> = None;
    let mut chars = 0usize;
    for (index, ch) in low.char_indices() {
        match (word_char(ch), start) {
            (true, None) => {
                start = Some(index);
                chars = 1;
            }
            (true, Some(_)) => chars += 1,
            (false, Some(from)) => {
                if chars >= 4 {
                    out.push(&low[from..index]);
                }
                start = None;
                chars = 0;
            }
            (false, None) => {}
        }
    }
    if let Some(from) = start {
        if chars >= 4 {
            out.push(&low[from..]);
        }
    }
}

/// `_tokens()` — the wording vector of one article, `Counter` semantics.
pub(crate) fn tokens(body: &str, lex: &Lex) -> HashMap<String, i64> {
    let low = body.to_lowercase();
    let mut runs: Vec<&str> = Vec::new();
    token4_runs(&low, &mut runs);
    let mut out: HashMap<String, i64> = HashMap::new();
    for word in runs {
        if lex.text_stop.contains(word) {
            continue;
        }
        let word = if word.ends_with('s') && word.chars().count() > 5 {
            &word[..word.len() - 1]
        } else {
            word
        };
        *out.entry(word.to_string()).or_insert(0) += 1;
    }
    out
}

/// One accepted connection, in the shape `recompute_links()` collects them.
#[derive(Clone, Debug, PartialEq)]
struct Scored {
    score: f64,
    a: i64,
    b: i64,
    shared: i64,
    why: String,
}

/// What [`recompute_links`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinksDone {
    pub links: usize,
    /// Article pairs the entity posting lists produced, before scoring.
    pub candidates: usize,
}

/// `recompute_links(days=14, top_per_article=8, max_pairs=60000)`.
///
/// Score = cosine over IDF-weighted shared entities (shrunk when only one name is
/// shared) plus a bonus for shared headline words. Each article keeps its
/// strongest `top_per_article` partners, so every story is connected to something
/// meaningful — but never to more than [`MAX_LINKS_PER_ARTICLE`] of them.
pub fn recompute_links(
    store: &Store,
    caches: &mut Caches,
    ctx: &Context,
    days: i64,
    top_per_article: usize,
    max_pairs: usize,
) -> Result<LinksDone, StoreError> {
    let idf = match caches.take_fresh_idf(days) {
        Some(idf) => idf,
        None => {
            let idf = store.in_transaction(|tx| idf_map(tx, days))?;
            caches.store_idf(days, idf.clone());
            idf
        }
    };
    store.in_transaction(|tx| recompute_links_tx(tx, ctx, days, top_per_article, max_pairs, &idf))
}

fn recompute_links_tx(
    tx: &Connection,
    ctx: &Context,
    days: i64,
    top_per_article: usize,
    max_pairs: usize,
    idf: &Idf,
) -> Result<LinksDone, StoreError> {
    let lex = Lex::new();
    // The window as the archive's own timestamp, not `datetime('now', ?)` — see
    // [`window_ago`].
    let window = window_ago(days);
    let arts = query(
        tx,
        "SELECT id,title,summary,content,published FROM articles WHERE relevant=1 \
         AND published>=? ORDER BY published DESC",
        &[text(&window)],
    )?;
    if arts.len() < 2 {
        return Ok(LinksDone::default());
    }
    let ids: Vec<i64> = arts.iter().map(|row| row.int("id").unwrap_or(0)).collect();

    // `_article_weights()`: entity -> IDF weight, per article in the window.
    let mut weights: BTreeMap<i64, HashMap<i64, f64>> = BTreeMap::new();
    for row in query(
        tx,
        "SELECT ae.article_id a, ae.entity_id e FROM article_entities ae \
         JOIN articles ar ON ar.id=ae.article_id WHERE ar.relevant=1 AND ar.published>=?",
        &[text(&window)],
    )? {
        let article = row.int("a").unwrap_or(0);
        let entity = row.int("e").unwrap_or(0);
        let weight = idf.map.get(&entity).copied().unwrap_or(IDF_FALLBACK);
        weights.entry(article).or_default().insert(entity, weight);
    }
    // `_entity_meta()`: id -> (name, type) for every entity in the window.
    let mut meta: HashMap<i64, (String, String)> = HashMap::new();
    for row in query(
        tx,
        "SELECT DISTINCT en.id, en.name, en.type FROM article_entities ae \
         JOIN entities en ON en.id=ae.entity_id JOIN articles ar ON ar.id=ae.article_id \
         WHERE ar.relevant=1 AND ar.published>=?",
        &[text(&window)],
    )? {
        meta.insert(
            row.int("id").unwrap_or(0),
            (
                row.text("name").unwrap_or("").to_string(),
                // the model's type, or "" when it gave none — never a guess
                row.text("type").unwrap_or("").to_string(),
            ),
        );
    }
    // a shared country is not a story: keep places out of the similarity vector
    let places: HashSet<i64> = meta
        .iter()
        .filter(|(_, (name, kind))| kind == "LOC" || ctx.is_place(name))
        .map(|(id, _)| *id)
        .collect();
    weights.retain(|_, vec| {
        vec.retain(|entity, _| !places.contains(entity));
        !vec.is_empty()
    });
    let mut norms: HashMap<i64, f64> = HashMap::with_capacity(weights.len());
    for (article, vec) in &weights {
        let mut sum_sq = 0.0f64;
        for weight in vec.values() {
            sum_sq += weight * weight;
        }
        let norm = sum_sq.sqrt();
        norms.insert(*article, if norm == 0.0 { 1.0 } else { norm });
    }
    let df_cap = std::cmp::max(12, fraction(arts.len(), 0.2));
    let mut inverted: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for (article, vec) in &weights {
        for entity in vec.keys() {
            inverted.entry(*entity).or_default().push(*article);
        }
    }
    // `pairs[(a,b)] = (dot, shared, [(weight, entity), …])`
    let mut pairs: BTreeMap<(i64, i64), (f64, i64, Vec<(f64, i64)>)> = BTreeMap::new();
    for (entity, aids) in &inverted {
        if aids.len() < 2 || aids.len() as i64 > df_cap {
            continue;
        }
        let weight = idf.map.get(entity).copied().unwrap_or(IDF_FALLBACK);
        for i in 0..aids.len() {
            for j in (i + 1)..aids.len() {
                let (a, b) = (aids[i].min(aids[j]), aids[i].max(aids[j]));
                let entry = pairs.entry((a, b)).or_insert((0.0, 0, Vec::new()));
                entry.0 += weight * weight;
                entry.1 += 1;
                if entry.2.len() < 12 {
                    entry.2.push((weight, *entity));
                }
            }
        }
    }

    // wording vectors, in the order the article rows came back — the order
    // `rustcore.similar` numbers its documents in, so its `top_per_doc` cap picks
    // the same partners.
    let mut vectors: Vec<(i64, HashMap<String, f64>)> = Vec::new();
    {
        let mut docs: Vec<(i64, HashMap<String, i64>)> = Vec::with_capacity(arts.len());
        let mut tdf: HashMap<String, i64> = HashMap::new();
        for row in &arts {
            let id = row.int("id").unwrap_or(0);
            let title = row.text("title").unwrap_or("");
            let summary = row.text("summary").unwrap_or("");
            let content: String = row.text("content").unwrap_or("").chars().take(1200).collect();
            let tf = tokens(&format!("{} {} {}", title, summary, content), &lex);
            for token in tf.keys() {
                *tdf.entry(token.clone()).or_insert(0) += 1;
            }
            docs.push((id, tf));
        }
        let doc_count = docs.len() as i64;
        let df_max = std::cmp::max(4, fraction(docs.len(), 0.3));
        for (id, tf) in docs {
            let mut vec: HashMap<String, f64> = HashMap::new();
            for (token, count) in &tf {
                let df = tdf.get(token).copied().unwrap_or(1);
                if df < TEXT_DF_MIN || df > df_max {
                    continue;
                }
                if ctx.is_place(token) {
                    // "both mention Brazil" is not shared wording
                    continue;
                }
                let weight =
                    (1.0 + (*count as f64).ln()) * (1.0 + doc_count as f64 / df as f64).ln();
                vec.insert(token.clone(), weight);
            }
            if !vec.is_empty() {
                vectors.push((id, vec));
            }
        }
    }
    let mut tnorm: HashMap<i64, f64> = HashMap::new();
    for (article, vec) in &vectors {
        let mut sum_sq = 0.0f64;
        for weight in vec.values() {
            sum_sq += weight * weight;
        }
        let norm = sum_sq.sqrt();
        tnorm.insert(*article, if norm == 0.0 { 1.0 } else { norm });
    }
    // `rustcore.similar(payload, top_per_doc=max(12, top_per_article*2), min_score=0.06)`
    let payload: Vec<(i64, Vec<(String, f64)>)> = vectors
        .iter()
        .map(|(article, vec)| {
            let norm = tnorm.get(article).copied().unwrap_or(1.0);
            let mut tokens: Vec<(String, f64)> = vec
                .iter()
                .map(|(token, weight)| (token.clone(), weight / norm))
                .collect();
            tokens.sort_by(|a, b| a.0.cmp(&b.0));
            (*article, tokens)
        })
        .collect();
    let mut tpairs: BTreeMap<(i64, i64), f64> = BTreeMap::new();
    // The order `rustcore.similar` hands the pairs to Python — score desc, then
    // document position. Python keeps the *first* pair it meets when two wording
    // scores tie, so the loops below walk this vector, not the sorted map.
    let mut tpair_order: Vec<((i64, i64), f64)> = Vec::new();
    for (a, b, score) in cosine::compute(
        &payload,
        std::cmp::max(TEXT_PAIR_FLOOR, top_per_article * 2),
        0.06,
    ) {
        // Four decimals, because that is what `rustcore.similar` hands Python:
        // `atlas-core` prints each score with `fmt_score` (4 places) and the
        // bridge parses the text back, so Python's `tcos` is the *rounded*
        // cosine. Rounding here keeps the two sides bit-identical instead of
        // differing in the fourth decimal.
        let key = (a.min(b), a.max(b));
        let score = round4(score);
        tpairs.insert(key, score);
        tpair_order.push((key, score));
    }
    let lookup: HashMap<i64, &HashMap<String, f64>> =
        vectors.iter().map(|(id, vec)| (*id, vec)).collect();
    let text_cos = |a: i64, b: i64| -> f64 {
        tpairs
            .get(&(a.min(b), a.max(b)))
            .copied()
            .unwrap_or(0.0)
    };
    // `shared_words()`: the most telling words two headlines share.
    let shared_words = |a: i64, b: i64| -> Vec<String> {
        let empty: HashMap<String, f64> = HashMap::new();
        let va = lookup.get(&a).copied().unwrap_or(&empty);
        let vb = lookup.get(&b).copied().unwrap_or(&empty);
        if va.is_empty() || vb.is_empty() {
            return Vec::new();
        }
        let mut common: Vec<(String, f64)> = va
            .iter()
            .filter(|(token, _)| vb.contains_key(*token))
            .map(|(token, weight)| {
                let other = vb.get(token).copied().unwrap_or(0.0);
                (token.clone(), weight + other)
            })
            .collect();
        // `sorted(…, key=lambda t: -(va[t] + vb[t]))` runs over a *set*: Python
        // has no order to preserve here, so the token breaks the tie and the
        // answer is the same twice in a row.
        common.sort_by(|x, y| {
            y.1.partial_cmp(&x.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| x.0.cmp(&y.0))
        });
        common
            .into_iter()
            .filter(|(token, _)| lex.is_wording_word(token))
            .map(|(token, _)| token)
            .take(3)
            .collect()
    };
    // `readable()`: the names that explain a connection, people/orgs first.
    let readable = |eids: &[i64]| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for eid in eids {
            let (name, kind) = match meta.get(eid) {
                Some(pair) => pair.clone(),
                None => (String::new(), String::new()),
            };
            if name.is_empty() || kind == "LOC" || ctx.is_place(&name) || lex.is_junk_name(&name) {
                continue;
            }
            let name = name.strip_prefix('#').unwrap_or(&name).to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            if out.len() >= 3 {
                break;
            }
        }
        out
    };

    let mut scored: Vec<Scored> = Vec::new();
    let mut have: HashSet<(i64, i64)> = HashSet::new();
    for ((a, b), (dot, shared, evidence)) in &pairs {
        let denom = norms.get(a).copied().unwrap_or(1.0) * norms.get(b).copied().unwrap_or(1.0);
        let ecos = dot / denom;
        let tcos = text_cos(*a, *b);
        // shrink entity overlap for tiny sets: sharing one name says little
        let shrink = *shared as f64 / (*shared as f64 + 1.5);
        let escore = ecos * if *shared == 1 { 0.5 } else { shrink };
        let score = 0.6 * escore + 0.4 * tcos;
        let strong =
            (*shared >= 2 && escore >= 0.12) || (*shared == 1 && ecos >= 0.45) || tcos >= 0.32;
        if !strong || score < MIN_LINK_SCORE {
            continue;
        }
        // `sorted(val[2], reverse=True)`: strongest evidence first, then the
        // higher entity id — the order Python's tuple sort produces.
        let mut ordered: Vec<(f64, i64)> = evidence.clone();
        ordered.sort_by(|x, y| {
            y.0.partial_cmp(&x.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| y.1.cmp(&x.1))
        });
        let eids: Vec<i64> = ordered.into_iter().map(|(_, eid)| eid).collect();
        let names_ok = readable(&eids);
        let words = shared_words(*a, *b);
        let mut why = if names_ok.is_empty() {
            String::new()
        } else {
            format!("shared: {}", names_ok.join(", "))
        };
        if why.is_empty() {
            // only places in common: not a connection unless the wording agrees
            if tcos < MIN_TEXT_WITHOUT_ENTITIES || words.is_empty() {
                continue;
            }
            why = format!("same wording: {}", words.join(", "));
        }
        scored.push(Scored {
            score: round4(score),
            a: *a,
            b: *b,
            shared: *shared,
            why,
        });
        have.insert((*a, *b));
    }
    // wording-only pairs: same event, no shared entity (short teasers do this)
    for ((a, b), tcos) in &tpair_order {
        if have.contains(&(*a, *b)) || *tcos < MIN_TEXT_ONLY {
            continue;
        }
        let words = shared_words(*a, *b);
        scored.push(Scored {
            score: round4(0.55 * tcos),
            a: *a,
            b: *b,
            shared: 0,
            why: wording_why(&words),
        });
        have.insert((*a, *b));
    }

    // Second signal: articles that belong to the same topic are related too. Only
    // articles with almost no connection of their own get such a filler, and each
    // one only its closest neighbour, so a topic never becomes a hairball.
    let mut tnames: HashMap<i64, String> = HashMap::new();
    for row in query(tx, "SELECT id,name FROM topics", &[])? {
        tnames.insert(
            row.int("id").unwrap_or(0),
            row.text("name").unwrap_or("").to_string(),
        );
    }
    let mut by_topic: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for row in query(
        tx,
        "SELECT at2.topic_id t, at2.article_id a FROM article_topics at2 \
         JOIN articles ar ON ar.id=at2.article_id WHERE ar.relevant=1 \
         AND ar.published>=? \
         ORDER BY at2.topic_id, ar.published DESC",
        &[text(&window)],
    )? {
        by_topic
            .entry(row.int("t").unwrap_or(0))
            .or_default()
            .push(row.int("a").unwrap_or(0));
    }
    let mut degree: HashMap<i64, i64> = HashMap::new();
    for entry in &scored {
        *degree.entry(entry.a).or_insert(0) += 1;
        *degree.entry(entry.b).or_insert(0) += 1;
    }
    for (tid, aids) in &by_topic {
        let tname = tnames
            .get(tid)
            .map(|name| name.trim().to_string())
            .unwrap_or_default();
        if tname.is_empty() || aids.len() < 2 {
            continue;
        }
        for i in 0..aids.len() {
            let upper = std::cmp::min(aids.len(), i + 3);
            for b in aids[(i + 1)..upper].iter() {
                let a = aids[i];
                let b = *b;
                let key = (a.min(b), a.max(b));
                if have.contains(&key) {
                    continue;
                }
                // `degree.get(a, 3) >= 2` — a story with no degree at all counts
                // as 3 here, so the filler only ever helps a lonely article.
                if degree.get(&a).copied().unwrap_or(3) >= 2
                    || degree.get(&b).copied().unwrap_or(3) >= 2
                {
                    continue;
                }
                have.insert(key);
                *degree.entry(a).or_insert(0) += 1;
                *degree.entry(b).or_insert(0) += 1;
                scored.push(Scored {
                    score: 0.16,
                    a: key.0,
                    b: key.1,
                    shared: 0,
                    why: format!("same topic: {}", tname),
                });
            }
        }
    }
    // Still isolated? Give the story its single best remaining match (weak
    // wording only) so nothing sits alone in the graph without inventing a
    // hairball.
    let mut best_weak: BTreeMap<i64, (f64, i64, (i64, i64))> = BTreeMap::new();
    for ((a, b), tcos) in &tpair_order {
        if have.contains(&(*a, *b)) || *tcos < MIN_TEXT_RESCUE {
            continue;
        }
        for (aid, other) in [(*a, *b), (*b, *a)] {
            if degree.get(&aid).copied().unwrap_or(0) != 0
                || degree.get(&other).copied().unwrap_or(0) >= 2
            {
                continue;
            }
            match best_weak.get(&aid) {
                Some((current, _, _)) if *current >= *tcos => {}
                _ => {
                    best_weak.insert(aid, (*tcos, other, (*a, *b)));
                }
            }
        }
    }
    for (aid, (tcos, other, key)) in &best_weak {
        if have.contains(key) {
            continue;
        }
        have.insert(*key);
        *degree.entry(*aid).or_insert(0) += 1;
        *degree.entry(*other).or_insert(0) += 1;
        let words = shared_words(key.0, key.1);
        scored.push(Scored {
            score: round4(0.55 * tcos),
            a: key.0,
            b: key.1,
            shared: 0,
            why: wording_why(&words),
        });
    }

    let cap = std::cmp::max(1, std::cmp::min(top_per_article, MAX_LINKS_PER_ARTICLE));
    scored.sort_by(|x, y| {
        y.score
            .partial_cmp(&x.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.a.cmp(&y.a))
            .then_with(|| x.b.cmp(&y.b))
    });
    if scored.len() > max_pairs {
        scored.truncate(max_pairs);
    }
    // `DELETE FROM article_links WHERE a IN (…) OR b IN (…)`, in chunks so a wide
    // window cannot hit SQLite's bound-variable limit.
    for chunk in ids.chunks(400) {
        let marks = placeholders(chunk.len());
        let mut params: Vec<SqlValue> = chunk.iter().map(|id| int(*id)).collect();
        params.extend(chunk.iter().map(|id| int(*id)));
        exec(
            tx,
            &format!(
                "DELETE FROM article_links WHERE a IN ({}) OR b IN ({})",
                marks, marks
            ),
            &params,
        )?;
    }
    // hard per-article cap: strongest pairs first, so no story becomes a hub
    let mut rows: Vec<Vec<SqlValue>> = Vec::new();
    let mut kept: HashMap<i64, i64> = HashMap::new();
    for entry in &scored {
        if kept.get(&entry.a).copied().unwrap_or(0) >= cap as i64
            || kept.get(&entry.b).copied().unwrap_or(0) >= cap as i64
        {
            continue;
        }
        *kept.entry(entry.a).or_insert(0) += 1;
        *kept.entry(entry.b).or_insert(0) += 1;
        rows.push(vec![
            int(entry.a),
            int(entry.b),
            int(entry.shared),
            real(stored_score(entry.score)),
            text(&entry.why),
        ]);
    }
    if !rows.is_empty() {
        // one plan for the whole link set, in the deterministic order `scored`
        // was capped in
        let mut stmt = tx
            .prepare(
                "INSERT INTO article_links(a,b,shared,score,why) VALUES(?,?,?,?,?) \
                 ON CONFLICT(a,b) DO UPDATE SET shared=excluded.shared, score=excluded.score, \
                 why=excluded.why",
            )
            .map_err(StoreError::from)?;
        for params in &rows {
            stmt.execute(rusqlite::params_from_iter(params.iter()))
                .map_err(StoreError::from)?;
        }
    }
    prune_links_tx(tx)?;
    Ok(LinksDone {
        links: rows.len(),
        candidates: pairs.len(),
    })
}

fn wording_why(words: &[String]) -> String {
    if words.is_empty() {
        "similar headline".to_string()
    } else {
        format!("same wording: {}", words.join(", "))
    }
}

/// The score as it is written: finite, rounded to four places, inside `0..=1`.
///
/// `score.min(1.0)` used to run before `round4`, and `f64::min` answers the
/// *other* operand when one is NaN — so a NaN score was stored as `1.0`, the
/// strongest connection the graph can express, instead of as no connection at
/// all. SQLite would also turn a NaN `REAL` into `NULL`, which the graph route
/// hands the UI as `"score": null` — a number the panel then has to guess about.
/// One guard for both: a non-finite score is not a score.
fn stored_score(score: f64) -> f64 {
    if !score.is_finite() {
        return 0.0;
    }
    round4(score.clamp(0.0, 1.0))
}

/// `prune_links()` — drop connections that point at articles no longer shown.
pub fn prune_links(store: &Store) -> Result<usize, StoreError> {
    store.in_transaction(prune_links_tx)
}

fn prune_links_tx(tx: &Connection) -> Result<usize, StoreError> {
    exec(
        tx,
        "DELETE FROM article_links WHERE a NOT IN (SELECT id FROM articles WHERE relevant=1) \
         OR b NOT IN (SELECT id FROM articles WHERE relevant=1)",
        &[],
    )
}

// ---------------------------------------------------------------- threads

/// What [`recompute_threads`] wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadsDone {
    pub threads: usize,
    pub members: usize,
}

/// One `article_links` row, in the three fields the clustering needs: `a`, `b`
/// and the score the merge order depends on. No allocation and nothing the
/// caller has to look up by name.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Link {
    a: u32,
    b: u32,
    score: f64,
}

/// Read the links a thread rebuild will merge, once, in merge order.
///
/// # Why not `query()`
///
/// `query()` answers `Vec<Row>`, and a `Row` carries a *copy of the column names*
/// with every row: on the 5 000-article archive (60 000 links) that is 180 000
/// `String` allocations for three column names the caller already knows, plus a
/// `SqlValue` per cell. This reads the three typed columns straight into one
/// vector: one statement, one pass, no per-row map.
///
/// # Why the sort is here and not in the statement
///
/// `article_links` is keyed on `(a, b)` and has no index on `score`, so
/// `ORDER BY score DESC, a, b` makes SQLite build a sorter over the whole table
/// and hand every row through the row layer before the order is used. The order
/// the clustering needs is a *total* order over `(score, a, b)` and `(a, b)` is
/// the primary key — no two rows share it — so sorting the vector in Rust is the
/// same order over the same rows, and it costs one pass we already paid for.
///
/// Measured on the 5 000-article / 60 000-link archive, release (see `tests`):
/// this read 4.3-6.3 ms against 12.7-27.7 ms through `Row`, and the whole rebuild
/// 42.5 ms against 76.3 ms, or 46.1 ms p95 against 88.0 ms
/// (`atlas-bench --only store.threads`, back to back).
fn read_links(tx: &Connection, min_score: f64) -> Result<Vec<Link>, StoreError> {
    let mut stmt = tx
        .prepare("SELECT a,b,score FROM article_links WHERE score>=?")
        .map_err(StoreError::from)?;
    let mut rows = stmt.query([min_score]).map_err(StoreError::from)?;
    let mut out: Vec<Link> = Vec::new();
    while let Some(row) = rows.next().map_err(StoreError::from)? {
        // A link with no article on one end is a corrupt row, not a thread: say
        // so and name the step that removes it, instead of either failing with a
        // column-type error nobody can act on or clustering under an id that is
        // not an article.
        let ends: (Option<i64>, Option<i64>) = (
            row.get::<usize, Option<i64>>(0).map_err(StoreError::from)?,
            row.get::<usize, Option<i64>>(1).map_err(StoreError::from)?,
        );
        let (a, b) = match ends {
            (Some(a), Some(b)) => (a, b),
            _ => {
                return Err(StoreError::new(
                    "article_links holds a link with no article on one end; run the \
                     prune step, then rebuild the threads",
                ))
            }
        };
        out.push(Link {
            a: a as u32,
            b: b as u32,
            score: row.get::<usize, f64>(2).map_err(StoreError::from)?,
        });
    }
    // Strongest first, then by endpoint: the order `ORDER BY score DESC, a, b`
    // gives, so the merges that decide a thread's shape run in the same order as
    // before.
    out.sort_unstable_by(|x, y| {
        y.score
            .total_cmp(&x.score)
            .then_with(|| x.a.cmp(&y.a))
            .then_with(|| x.b.cmp(&y.b))
    });
    Ok(out)
}

/// `recompute_threads(min_score=0.20, max_size=40)` — group connected articles
/// into focused story threads.
///
/// Only connections above `min_score` merge stories, and a thread never grows past
/// `max_size` members — otherwise one strong story swallows the archive. The
/// clustering is the engine's own (`Uf`, the group order, `choose_label`), so a
/// thread written here gets the title and the order the `threads` subcommand
/// produces for the same links; only the id is new, and it is the one SQLite
/// assigned to the row, exactly as `lastrowid` gives it to Python.
pub fn recompute_threads(
    store: &Store,
    ctx: &Context,
    min_score: f64,
    max_size: usize,
) -> Result<ThreadsDone, StoreError> {
    let lex = Lex::new();
    let max_size = std::cmp::max(1, max_size);
    store.in_transaction(|tx| {
        exec(tx, "DELETE FROM article_threads", &[])?;
        exec(tx, "DELETE FROM threads", &[])?;
        let links = read_links(tx, min_score)?;
        let mut uf = Uf::new();
        for link in &links {
            uf.union(link.a, link.b, max_size as u32);
        }
        // `for node in sorted(parent): groups[find(node)].append(node)`
        let mut nodes = uf.nodes();
        nodes.sort_unstable();
        let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for node in nodes {
            let root = uf.find(node);
            groups.entry(root).or_default().push(node);
        }
        let mut ordered: Vec<Vec<u32>> = groups.into_values().collect();
        // deterministic order: biggest thread first, then by first member id
        ordered.sort_by(|x, y| y.len().cmp(&x.len()).then_with(|| x[0].cmp(&y[0])));

        // One plan for the whole rebuild: these two statements run once per
        // thread and once per member, so preparing them inside the loops would
        // re-plan the same SQL 5 000 times.
        let mut add_member = tx
            .prepare("INSERT OR REPLACE INTO article_threads(article_id,thread_id) VALUES(?,?)")
            .map_err(StoreError::from)?;
        let mut add_thread = tx
            .prepare("INSERT INTO threads(title,size,updated) VALUES(?,?,?)")
            .map_err(StoreError::from)?;
        let mut used: HashSet<String> = HashSet::new();
        let mut done = ThreadsDone::default();
        for members in ordered {
            if members.len() < 2 {
                continue;
            }
            let mut members = members;
            members.sort_unstable();
            members.truncate(max_size);
            let mut label = thread_label(tx, &lex, ctx, &members, &used)?;
            if label.is_empty() {
                continue;
            }
            if used.contains(&label) {
                // same label twice: number it
                let base = strip_count_suffix(&label);
                let mut n = 2usize;
                while used.contains(&label) {
                    label = format!("{} ({})", base, n);
                    n += 1;
                }
            }
            // `AND relevant=1 ORDER BY published DESC, id DESC` — the Python
            // statement, with one term written as `+relevant`.
            //
            // That `+` is not decoration. Written plainly, SQLite answers the
            // `relevant=1` term from `idx_articles_relevant` and walks every relevant
            // article in the archive to answer a question about at most 40 ids; unary
            // `+` (documented: the term still filters, it just cannot choose the plan)
            // leaves the primary key to answer the `IN` list instead. Measured on the
            // 5 000-article archive, this statement alone, release: 0.143 ms per
            // thread before, 0.019 ms after. The rows are the same rows, in the same
            // `ORDER BY published DESC, id DESC` order.
            let marks = placeholders(members.len());
            let params: Vec<SqlValue> = members.iter().map(|id| int(*id as i64)).collect();
            let arts = query(
                tx,
                &format!(
                    "SELECT id,title,published FROM articles WHERE id IN ({}) \
                     AND +relevant=1 ORDER BY published DESC, id DESC",
                    marks
                ),
                &params,
            )?;
            if arts.len() < 2 {
                // Not a thread: the cluster's articles are not shown any more, so
                // nothing is written and — unlike `server/graph.py` — the label is
                // not reserved either. Reserving it numbered the *next* thread
                // that happened to share the title ("Foo (2)") while "Foo" was
                // never drawn; a number with no first thread is a lie the reader
                // has no way to check.
                continue;
            }
            // reserved only once the thread it names exists
            used.insert(label.clone());
            let title: String = label.chars().take(200).collect();
            let updated = same_value(arts[0].get("published"));
            let values = [text(&title), int(arts.len() as i64), updated];
            add_thread
                .execute(rusqlite::params_from_iter(values.iter()))
                .map_err(StoreError::from)?;
            let tid = tx.last_insert_rowid();
            done.threads += 1;
            for row in &arts {
                let values = [int(row.int("id").unwrap_or(0)), int(tid)];
                add_member
                    .execute(rusqlite::params_from_iter(values.iter()))
                    .map_err(StoreError::from)?;
                done.members += 1;
            }
        }
        Ok(done)
    })
}

/// `_thread_title()` against the database: the same statement, the same order,
/// and the label decision the engine makes ([`crate::choose_label`]).
fn thread_label(
    tx: &Connection,
    lex: &Lex,
    ctx: &Context,
    members: &[u32],
    used: &HashSet<String>,
) -> Result<String, StoreError> {
    let marks = placeholders(members.len());
    let params: Vec<SqlValue> = members.iter().map(|id| int(*id as i64)).collect();
    let rows = query(
        tx,
        &format!(
            "SELECT en.name, en.type, en.count, COUNT(DISTINCT ae.article_id) c \
             FROM article_entities ae JOIN entities en ON en.id=ae.entity_id \
             WHERE ae.article_id IN ({}) GROUP BY en.id HAVING c>=2 \
             ORDER BY c DESC, en.count DESC LIMIT 24",
            marks
        ),
        &params,
    )?;
    let mut cands: Vec<(i64, String)> = Vec::new();
    for row in &rows {
        let name = row.text("name").unwrap_or("").to_string();
        if name.is_empty() || name.starts_with('#') || lex.is_junk_name(&name) {
            continue;
        }
        if row.text("type").unwrap_or("") == "LOC" || ctx.is_place(&name) {
            continue;
        }
        cands.push((row.int("c").unwrap_or(0), name));
    }
    cands.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
    let size = members.len() as i64;
    // dominant names first, then the closest thing to a majority
    let need = std::cmp::max(2, fraction(members.len(), 0.3));
    let mut top: Vec<String> = cands
        .iter()
        .filter(|(count, _)| *count >= need)
        .map(|(_, name)| name.clone())
        .take(3)
        .collect();
    if top.is_empty() {
        top = cands.iter().take(2).map(|(_, name)| name.clone()).collect();
    }
    // Both fallbacks run their own statements in Python and see different rows:
    // `_thread_words()` reads every member's headline, while the last resort keeps
    // the relevant ones only. Both are built here, lazily, from one query.
    let rows = query(
        tx,
        &format!(
            "SELECT id,title,relevant FROM articles WHERE id IN ({})",
            marks
        ),
        &params,
    )?;
    let all_titles: Vec<Article> = rows
        .iter()
        .map(|row| Article {
            id: row.int("id").unwrap_or(0) as u32,
            title: row.text("title").unwrap_or("").to_string(),
            refs: Vec::new(),
            tags: Vec::new(),
            ts: None,
            lat: None,
            lon: None,
            relevant: true,
        })
        .collect();
    let mut relevant_titles: Vec<(u32, String)> = rows
        .iter()
        .filter(|row| row.int("relevant").unwrap_or(0) == 1)
        .filter_map(|row| {
            row.text("title")
                .map(|title| (row.int("id").unwrap_or(0) as u32, title.to_string()))
        })
        .collect();
    relevant_titles.sort_by(|x, y| x.0.cmp(&y.0));
    let by_id: HashMap<u32, &Article> = all_titles.iter().map(|a| (a.id, a)).collect();
    let thread_ctx = ThreadCtx::new(by_id, HashSet::new(), lex);
    Ok(choose_label(
        &top,
        size as u32,
        used,
        || thread_words(members, &thread_ctx),
        || relevant_titles.clone(),
    ))
}

// ---------------------------------------------------------------- commands

/// `recompute` — `{"db":…,"parts":["edges","links","threads"]}`.
///
/// Kept next to the steps it runs so the two lists cannot drift apart; the
/// dispatch table is in `lib.rs`.
pub fn cmd_recompute_links(input: &atlas_lib::json::Json) -> Result<String, String> {
    let store = crate::graphdb::open(input).map_err(fail)?;
    let ctx = Context::from_json(input);
    let days = opt_i64(input, "days", LINK_DAYS);
    let top_per_article = opt_usize(input, "top_per_article", 8);
    let max_pairs = opt_usize(input, "max_pairs", 60_000);
    let mut caches = Caches::new();
    let done = recompute_links(&store, &mut caches, &ctx, days, top_per_article, max_pairs)
        .map_err(fail)?;
    Ok(jobj(&[
        ("links", jnum(done.links as f64)),
        ("candidates", jnum(done.candidates as f64)),
        ("source", jstr("rust")),
    ]))
}

/// `recompute_threads` on its own — the one step `ai.py` and `collector.py` call
/// without a full rebuild.
pub fn cmd_recompute_threads(input: &atlas_lib::json::Json) -> Result<String, String> {
    let store = crate::graphdb::open(input).map_err(fail)?;
    let ctx = Context::from_json(input);
    let min_score = opt_f64(input, "min_score", DEFAULT_MIN_SCORE);
    let max_size = opt_usize(input, "max_size", DEFAULT_MAX_SIZE);
    let done = recompute_threads(&store, &ctx, min_score, max_size).map_err(fail)?;
    Ok(jobj(&[
        ("threads", jnum(done.threads as f64)),
        ("members", jnum(done.members as f64)),
        ("source", jstr("rust")),
    ]))
}

/// `prune_links` on its own.
pub fn cmd_prune_links(input: &atlas_lib::json::Json) -> Result<String, String> {
    let store = crate::graphdb::open(input).map_err(fail)?;
    let pruned = prune_links(&store).map_err(fail)?;
    Ok(jobj(&[
        ("pruned", jnum(pruned as f64)),
        ("source", jstr("rust")),
    ]))
}


#[cfg(test)]
mod tests;
