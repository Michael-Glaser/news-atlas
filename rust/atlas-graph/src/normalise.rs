//! One identity per entity, and weights that agree with the evidence.
//!
//! The audit of `data/atlas.db` found three rows for one person — `Trump`, `Donald`,
//! `Donald Trump`; `Jensen Huang`, `Huang`, `Jensen` — each with its own edges, so the
//! picture drew one relationship several times and the panel explained a fragment.
//!
//! The rule below is *evidence*, never a list of names:
//!
//! * one name is a leading or trailing run of whole words of the other (`Trump` in
//!   `Donald Trump`), and the short name is at least [`MIN_PART_CHARS`] long, so an
//!   initial or a corporate token never merges;
//! * both names are shaped like proper names (a capital somewhere): a lowercase
//!   fragment is never a candidate for anything;
//! * the two rows are **never** mentioned in the same article. `Biden` and
//!   `Hunter Biden` side by side in one story are two people, and that article is the
//!   evidence that says so;
//! * the kinds are one family. The model moves a person between `PERSON`, `ORG` and
//!   `MISC` answer to answer — refusing those pairs is exactly how the split identity
//!   survived — while a place, an event and a topic only merge with their own kind;
//! * both rows are linked to at least [`MIN_MENTIONS`] articles. `entities.count` is a
//!   counter, not evidence: every junk row in the audit (`Sugar-tax Tajani`, `Alltag
//!   ChatGPT`, `Wann Trennung`) carries 23 there and no article link at all;
//! * a short name that fits two longer names (`Biden` in `Joe Biden` and `Hunter Biden`)
//!   is a surname two people share and stands for neither;
//! * a name the archive's own prose writes lowercase (`Three`, `Commerce`, `City`) is a
//!   word, not a name, whatever the shape rules say;
//! * a name `entity_alias` already declares is folded in without asking the shape rules,
//!   because the declaration is the model naming the same thing twice.
//!
//! The survivor is the fuller name (more words, then more characters), then the higher
//! mention count, then the lower id: the same archive always picks the same row. A pass
//! that has merged everything plans nothing on the next run, and the rebuilt weights
//! are a pure function of `article_entities`, so running twice writes the same numbers.
//!
//! The dry run reads only — no transaction, no write — and answers the same report as
//! the real pass with `"dry_run": true` and the merges it would make.

use std::collections::{BTreeMap, HashMap, HashSet};

use atlas_lib::json::Json;
use atlas_store::{Row, Store, StoreError};
use rusqlite::Connection;

use crate::keep_ids;
use crate::EntRef;
use crate::graphdb::{
    fail, int, jfixed, jnum, jobj, jstr, open, opt_i64, query, rebuild_edges, run, text,
    window_ago, EDGE_DAYS,
};

/// A short name has to be this long before it may stand for a longer one. `Xi` is a
/// surname and `US` is a mistake: three characters is where the short forms that are
/// really names begin, and it keeps initials, `AG` and `Co` out of the pass.
pub const MIN_PART_CHARS: usize = 3;

/// Articles that must mention a row before it may be merged. Not `entities.count`:
/// that column is a stale counter — the audit found rows carrying 23 with no article
/// link at all, and every junk row (`Sugar-tax Tajani`, `Alltag ChatGPT`, `Wann
/// Trennung`) is one of them, while every split identity (`Trump` 88 articles,
/// `Donald Trump` 12) is well past two.
pub const MIN_MENTIONS: i64 = 2;

/// One row that folds into another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merge {
    pub from: i64,
    pub into: i64,
    pub from_name: String,
    pub into_name: String,
    /// The two rows disagreed about the kind and the article evidence still says they
    /// are one identity: the survivor's kind wins.
    pub kind_mismatch: bool,
}

/// What a pass planned, or did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub dry_run: bool,
    pub entities_before: usize,
    pub entities_after: usize,
    pub merges: Vec<Merge>,
    pub kind_mismatches: usize,
    pub edges_before: usize,
    pub edges_after: usize,
    /// Relations the table held that the evidence does not support, so the rebuild drops
    /// them: a pair whose two ends stopped sharing an article, or two pairs that became
    /// one when the identities under them merged.
    pub edges_dropped: usize,
    /// Relations the evidence supports that the table never held: `link_article` writes a
    /// pair per article and nothing ever ran the rebuild, so the table has been missing
    /// relations the archive can prove.
    pub edges_added: usize,
    pub weights_changed: usize,
    pub labelled: usize,
    pub ms: f64,
}

impl Report {
    pub fn json(&self) -> String {
        let merges: Vec<String> = self
            .merges
            .iter()
            .map(|merge| {
                jobj(&[
                    ("from", jnum(merge.from as f64)),
                    ("into", jnum(merge.into as f64)),
                    ("from_name", jstr(&merge.from_name)),
                    ("into_name", jstr(&merge.into_name)),
                    (
                        "kind_mismatch",
                        if merge.kind_mismatch { "true" } else { "false" }.to_string(),
                    ),
                ])
            })
            .collect();
        jobj(&[
            (
                "dry_run",
                if self.dry_run { "true" } else { "false" }.to_string(),
            ),
            ("entities_before", jnum(self.entities_before as f64)),
            ("entities_after", jnum(self.entities_after as f64)),
            ("merged", jnum(self.merges.len() as f64)),
            ("kind_mismatches", jnum(self.kind_mismatches as f64)),
            ("edges_before", jnum(self.edges_before as f64)),
            ("edges_after", jnum(self.edges_after as f64)),
            ("edges_dropped", jnum(self.edges_dropped as f64)),
            ("edges_added", jnum(self.edges_added as f64)),
            ("weights_changed", jnum(self.weights_changed as f64)),
            ("labelled", jnum(self.labelled as f64)),
            ("ms", jfixed(self.ms, 1)),
            ("merges", format!("[{}]", merges.join(","))),
        ])
    }
}

/// One entity row, as the plan needs it.
#[derive(Clone, Debug)]
struct Named {
    id: i64,
    name: String,
    kind: String,
    count: i64,
}

fn words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

/// Is `short` a leading or trailing run of whole words of `long`?
fn is_part_of(short: &str, long: &str) -> bool {
    let (short_words, long_words) = (words(short), words(long));
    if short_words.is_empty() || short_words.len() >= long_words.len() {
        return false;
    }
    long_words[..short_words.len()] == short_words[..]
        || long_words[long_words.len() - short_words.len()..] == short_words[..]
}

/// Shaped like a proper name: a capital somewhere. `high` and `continue` are refused as
/// candidates; `eBay`, `iPhone` and `McDonald` pass.
fn proper_shaped(name: &str) -> bool {
    name.chars().any(|c| c.is_uppercase())
}

/// Shaped like a name at all: a capital, no prose punctuation, and no more words or
/// characters than a name has. The same shape rules the extractor applies to a new
/// answer ([`atlas_ingest`]'s `admit`), because a merge is only ever as good as the two
/// rows it joins: `Gen-Z-Protesten in Hamburg` and `Antisemitismus und Rassismus
/// Zahlreiche Demokratie-Projekte` are answers the extractor no longer accepts, and a
/// pass that folded a name into one of them would be completing the old defect rather
/// than repairing it.
fn plausible(name: &str) -> bool {
    let words = words(name);
    let trimmed = name.trim();
    !trimmed.is_empty()
        && words.len() <= NAME_WORDS_MAX
        && trimmed.chars().count() <= NAME_CHARS_MAX
        && proper_shaped(trimmed)
        && !trimmed.chars().any(|c| {
            matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']' | '"')
        })
}

/// Words a name may hold — the extractor's ceiling, so the two ends agree.
pub const NAME_WORDS_MAX: usize = 4;
/// Characters a name may hold — the extractor's ceiling.
pub const NAME_CHARS_MAX: usize = 48;

/// The kinds one identity may carry. A person is typed `PERSON`, `ORG` or `MISC` by the
/// same model on different days; a place, an event and a topic only merge with their own
/// kind (and anything unknown only with itself, by its own name).
fn family(kind: &str) -> String {
    match kind.trim().to_uppercase().as_str() {
        "PERSON" | "ORG" | "MISC" | "" => "who".to_string(),
        "LOC" => "where".to_string(),
        "EVENT" => "when".to_string(),
        other => format!("what:{other}"),
    }
}

/// Which of two rows survives: the fuller name, then the higher count, then the lower id.
fn keeps(left: &Named, right: &Named) -> bool {
    let (left_words, right_words) = (words(&left.name).len(), words(&right.name).len());
    let (left_chars, right_chars) = (left.name.chars().count(), right.name.chars().count());
    (left_words, left_chars, left.count, std::cmp::Reverse(left.id))
        > (
            right_words,
            right_chars,
            right.count,
            std::cmp::Reverse(right.id),
        )
}

fn named(row: &Row) -> Named {
    Named {
        id: row.int("id").unwrap_or(0),
        name: row.text("name").unwrap_or("").to_string(),
        kind: row.text("type").unwrap_or("").to_string(),
        count: row.int("count").unwrap_or(0),
    }
}

/// The plan, from entity rows and the pairs of ids that share an article.
fn plan_from(
    rows: &[Row],
    alias_rows: &[Row],
    shared: &HashSet<(i64, i64)>,
    links: &HashMap<i64, i64>,
) -> Vec<Merge> {
    let rows: Vec<Named> = rows.iter().map(named).collect();
    // A candidate can only be a word of the other name, so the words are the index:
    // one map lookup per shared word instead of every pair of 4 539 rows.
    let mut by_word: HashMap<String, Vec<&Named>> = HashMap::new();
    for row in &rows {
        for word in words(&row.name) {
            if word.chars().count() >= MIN_PART_CHARS {
                by_word.entry(word).or_default().push(row);
            }
        }
    }
    let mut candidates: BTreeMap<(i64, i64), Merge> = BTreeMap::new();
    // Short name → the longer names it could stand for. A name that fits two of them
    // (`Biden` in `Joe Biden` and `Hunter Biden`) is a surname two people share, so it
    // stands for neither: the pair is left to the reader, not to the pass.
    let mut partners: HashMap<i64, HashSet<i64>> = HashMap::new();
    for group in by_word.values() {
        if group.len() < 2 {
            continue;
        }
        for (index, left) in group.iter().enumerate() {
            for right in &group[index + 1..] {
                let (left_words, right_words) = (words(&left.name).len(), words(&right.name).len());
                let (short, long) = if left_words < right_words {
                    (*left, *right)
                } else if right_words < left_words {
                    (*right, *left)
                } else {
                    continue;
                };
                // A person, split by the model between PERSON, ORG and MISC: that is
                // the defect the audit found (Trump / Donald Trump / Donald). An
                // organisation, a place or a noun phrase is left alone — `Rocky` into
                // `Rocky Balboa` is a repair, `Jugendliche` into `Deutsche Jugendliche`
                // is two fragments becoming one.
                let personish = |row: &Named| {
                    family(&row.kind) == "who" && row.kind.trim().eq_ignore_ascii_case("PERSON")
                };
                if short.id == long.id
                    || short.name.chars().count() < MIN_PART_CHARS
                    || words(&short.name).len() != 1
                    || words(&long.name).len() > 3
                    || !is_part_of(&short.name, &long.name)
                    || !plausible(&short.name)
                    || !plausible(&long.name)
                    || family(&short.kind) != family(&long.kind)
                    || !(personish(short) || personish(long))
                    // A name the archive has never linked to an article is a headline
                    // fragment until it is linked to one; see [`MIN_MENTIONS`].
                    || mentions(links, short.id) < MIN_MENTIONS
                    || mentions(links, long.id) < MIN_MENTIONS
                {
                    continue;
                }
                let key = (short.id.min(long.id), short.id.max(long.id));
                if shared.contains(&key) {
                    // mentioned side by side: two identities, whatever the names suggest
                    continue;
                }
                partners.entry(short.id).or_default().insert(long.id);
                let (from, into) = if keeps(long, short) { (short, long) } else { (long, short) };
                candidates.insert(
                    (from.id, into.id),
                    Merge {
                        from: from.id,
                        into: into.id,
                        from_name: from.name.clone(),
                        into_name: into.name.clone(),
                        kind_mismatch: !from.kind.trim().eq_ignore_ascii_case(into.kind.trim()),
                    },
                );
            }
        }
    }
    let ambiguous: HashSet<i64> = partners
        .iter()
        .filter(|(_, longs)| longs.len() > 1)
        .map(|(short, _)| *short)
        .collect();
    candidates.retain(|(from, _), _| !ambiguous.contains(from));

    // The model's own declaration, where the alias table names a row that still exists:
    // `entity_alias(alias, entity_id)` says these two names are one thing, and nothing
    // in this pass has better evidence than that.
    if !alias_rows.is_empty() {
        let by_name: HashMap<String, &Named> = rows
            .iter()
            .map(|row| (row.name.trim().to_lowercase(), row))
            .collect();
        for row in alias_rows {
            let alias = row.text("alias").unwrap_or("");
            let target = row.int("entity_id").unwrap_or(0);
            let Some(found) = by_name.get(&alias.trim().to_lowercase()) else {
                continue;
            };
            if found.id == target || !proper_shaped(alias) || !plausible(alias) {
                continue;
            }
            let Some(into) = rows.iter().find(|row| row.id == target) else {
                continue;
            };
            if family(&found.kind) != family(&into.kind) || !plausible(&into.name) {
                continue;
            }
            candidates.insert(
                (found.id, into.id),
                Merge {
                    from: found.id,
                    into: into.id,
                    from_name: found.name.clone(),
                    into_name: into.name.clone(),
                    kind_mismatch: !found.kind.trim().eq_ignore_ascii_case(into.kind.trim()),
                },
            );
        }
    }

    // One row cannot fold into two, and a row that is folding away cannot receive one:
    // the plan is a forest, walked in id order so it is deterministic.
    let mut folding: HashSet<i64> = HashSet::new();
    let mut merges: Vec<Merge> = Vec::new();
    for merge in candidates.into_values() {
        if folding.contains(&merge.from) || folding.contains(&merge.into) {
            continue;
        }
        folding.insert(merge.from);
        merges.push(merge);
    }
    merges
}

/// `id → name` for the entity rows, so a text can be judged against the name it is
/// supposed to be.
fn names_of(rows: &[Row]) -> HashMap<i64, String> {
    rows.iter()
        .map(|row| {
            (
                row.int("id").unwrap_or(0),
                row.text("name").unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// The titles and bodies of the articles that mention any of `ids`, one row each.
fn texts_sql(ids: &[i64]) -> String {
    let list: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    format!(
        "SELECT x.entity_id id, ar.title title, ar.content content FROM article_entities x \
         JOIN articles ar ON ar.id = x.article_id WHERE x.entity_id IN ({})",
        list.join(",")
    )
}

/// Drop the merges whose folded-away name the archive's own prose writes lowercase:
/// `Three`, `Commerce`, `Secretary`, `City` and `South` are words a sentence lowercases;
/// a name is not, however often the archive repeats it. This is the measurement the
/// ingest takes when it admits an entity (see `atlas-ingest`'s `admit`), taken here from
/// the text the archive already keeps — no list of words to drop.
fn without_prose(texts: &[Row], names: &HashMap<i64, String>, mut merges: Vec<Merge>) -> Vec<Merge> {
    let mut counted: HashMap<i64, (usize, usize)> = HashMap::new();
    for row in texts {
        let id = row.int("id").unwrap_or(0);
        let Some(name) = names.get(&id) else {
            continue;
        };
        let text = format!(
            "{} {}",
            row.text("title").unwrap_or(""),
            row.text("content").unwrap_or("")
        );
        let seen = counted.entry(id).or_insert((0, 0));
        seen.0 += text.matches(name.as_str()).count();
        seen.1 += text.matches(&name.to_lowercase()).count();
    }
    merges.retain(|merge| match counted.get(&merge.from) {
        // no text to judge it by: the shape rules already spoke
        None => true,
        Some((exact, lower)) => lower <= exact,
    });
    merges
}

fn mentions(links: &HashMap<i64, i64>, id: i64) -> i64 {
    links.get(&id).copied().unwrap_or(0)
}

/// Articles per entity: the evidence a merge is allowed to rest on.
fn article_counts(rows: &[Row]) -> HashMap<i64, i64> {
    rows.iter()
        .map(|row| (row.int("entity_id").unwrap_or(0), row.int("n").unwrap_or(0)))
        .collect()
}

fn shared_articles(rows: &[Row]) -> HashSet<(i64, i64)> {
    rows.iter()
        .map(|row| (row.int("a").unwrap_or(0), row.int("b").unwrap_or(0)))
        .collect()
}

const ENTITIES_SQL: &str = "SELECT id,name,type,count FROM entities";
const ALIAS_SQL: &str = "SELECT alias, entity_id FROM entity_alias";
const LINKS_SQL: &str = "SELECT entity_id, count(*) n FROM article_entities GROUP BY entity_id";
const SHARED_SQL: &str = "SELECT x.entity_id a, y.entity_id b FROM article_entities x \
     JOIN article_entities y ON y.article_id = x.article_id WHERE x.entity_id < y.entity_id";

/// The merges this archive would receive. Read-only, deterministic, and a fixed point:
/// applying them leaves nothing to plan.
pub fn plan(store: &Store) -> Result<Vec<Merge>, StoreError> {
    let entities = store.query(ENTITIES_SQL, &[])?;
    let shared = shared_articles(&store.query(SHARED_SQL, &[])?);
    let aliases = store.query(ALIAS_SQL, &[])?;
    let links = article_counts(&store.query(LINKS_SQL, &[])?);
    let merges = plan_from(&entities, &aliases, &shared, &links);
    let ids: Vec<i64> = merges.iter().map(|merge| merge.from).collect();
    if ids.is_empty() {
        return Ok(merges);
    }
    let texts = store.query(&texts_sql(&ids), &[])?;
    Ok(without_prose(&texts, &names_of(&entities), merges))
}

/// Fold `merges` into the archive, inside the caller's transaction: links and aliases
/// repointed, duplicates dropped, mentions and the seen window folded into the
/// survivor, the folded rows deleted.
fn apply(conn: &Connection, merges: &[Merge]) -> Result<usize, StoreError> {
    let mut move_links = conn
        .prepare("UPDATE OR IGNORE article_entities SET entity_id=? WHERE entity_id=?")
        .map_err(StoreError::from)?;
    let mut drop_links = conn
        .prepare("DELETE FROM article_entities WHERE entity_id=?")
        .map_err(StoreError::from)?;
    let mut keep_alias = conn
        .prepare("INSERT OR IGNORE INTO entity_alias(alias,entity_id) VALUES(?,?)")
        .map_err(StoreError::from)?;
    let mut move_aliases = conn
        .prepare("UPDATE OR IGNORE entity_alias SET entity_id=? WHERE entity_id=?")
        .map_err(StoreError::from)?;
    let mut drop_aliases = conn
        .prepare("DELETE FROM entity_alias WHERE entity_id=?")
        .map_err(StoreError::from)?;
    // Mentions add up, and the window the entity was seen in covers both rows: a merge
    // that kept only the survivor's dates would age the identity out of the graph.
    let mut fold = conn
        .prepare(
            "UPDATE entities SET count=coalesce(count,0)+coalesce((SELECT count FROM entities \
                     WHERE id=?2),0), \
                 first_seen=min(coalesce(first_seen,''), \
                     coalesce((SELECT first_seen FROM entities WHERE id=?2),'')), \
                 last_seen=max(coalesce(last_seen,''), \
                     coalesce((SELECT last_seen FROM entities WHERE id=?2),'')) \
             WHERE id=?1",
        )
        .map_err(StoreError::from)?;
    let mut drop_entity = conn
        .prepare("DELETE FROM entities WHERE id=?")
        .map_err(StoreError::from)?;
    let mut done = 0usize;
    for merge in merges {
        run(&mut move_links, &[int(merge.into), int(merge.from)])?;
        run(&mut drop_links, &[int(merge.from)])?;
        run(&mut keep_alias, &[text(&merge.from_name), int(merge.into)])?;
        run(&mut move_aliases, &[int(merge.into), int(merge.from)])?;
        run(&mut drop_aliases, &[int(merge.from)])?;
        run(&mut fold, &[int(merge.into), int(merge.from)])?;
        run(&mut drop_entity, &[int(merge.from)])?;
        done += 1;
    }
    Ok(done)
}

/// The weights the evidence gives once `merges` are folded in, and how many stored edges
/// disagree with it — the dry run's answer, computed without writing.
fn projected(
    store: &Store,
    merges: &[Merge],
    window: &str,
) -> Result<(usize, usize, usize, usize), StoreError> {
    let map: HashMap<i64, i64> = merges.iter().map(|merge| (merge.from, merge.into)).collect();
    let mentions = store.query(
        "SELECT ae.article_id a, ae.entity_id e, en.type t FROM article_entities ae \
         JOIN entities en ON en.id=ae.entity_id JOIN articles ar ON ar.id=ae.article_id \
         WHERE ar.relevant=1 AND ar.published>=?",
        &[text(window)],
    )?;
    let projected = weights(&mentions, &map);
    let stored = store.query("SELECT a,b,weight FROM edges", &[])?;
    let mut changed = 0usize;
    let mut dropped = 0usize;
    for row in &stored {
        let key = (row.int("a").unwrap_or(0), row.int("b").unwrap_or(0));
        // The same definition the rebuild counts with: a stored row the evidence rewrites
        // — reweighted or dropped — is one repair.
        match projected.get(&key) {
            None => {
                dropped += 1;
                changed += 1;
            }
            Some(weight) if *weight != row.int("weight").unwrap_or(0) => changed += 1,
            Some(_) => {}
        }
    }
    let held: HashSet<(i64, i64)> = stored
        .iter()
        .map(|row| (row.int("a").unwrap_or(0), row.int("b").unwrap_or(0)))
        .collect();
    let added = projected.keys().filter(|key| !held.contains(key)).count();
    Ok((projected.len(), changed, added, dropped))
}

/// Weight = the number of articles that mention both ends, with the merge map applied.
/// The same shape [`crate::graphdb`] writes, so the dry run cannot disagree with the
/// pass it is predicting.
fn weights(mentions: &[Row], map: &HashMap<i64, i64>) -> BTreeMap<(i64, i64), i64> {
    let mut per: BTreeMap<i64, Vec<(i64, String)>> = BTreeMap::new();
    for row in mentions {
        let raw = row.int("e").unwrap_or(0);
        let entity = *map.get(&raw).unwrap_or(&raw);
        let kind = row.text("t").unwrap_or("").to_string();
        per.entry(row.int("a").unwrap_or(0))
            .or_default()
            .push((entity, kind));
    }
    let mut pairs: BTreeMap<(i64, i64), i64> = BTreeMap::new();
    for entities in per.values_mut() {
        entities.sort_by_key(|(id, _)| *id);
        entities.dedup_by_key(|(id, _)| *id);
        // The same per-article cap the rebuild applies: without it the projection counts
        // pairs the rebuilt table will never hold, and the dry run stops agreeing with
        // the pass it is meant to describe (13 914 edges projected, 6 120 written).
        let refs: Vec<EntRef> = entities
            .iter()
            .map(|(id, kind)| EntRef {
                id: *id as u32,
                name: String::new(),
                kind: kind.clone(),
                count: 0.0,
            })
            .collect();
        let keep: Vec<i64> = keep_ids(&refs).into_iter().map(|id| id as i64).collect();
        for i in 0..keep.len() {
            for j in (i + 1)..keep.len() {
                *pairs.entry((keep[i], keep[j])).or_insert(0) += 1;
            }
        }
    }
    pairs
}

fn counted(store: &Store, sql: &str) -> Result<usize, StoreError> {
    Ok(store
        .query(sql, &[])?
        .first()
        .and_then(|row| row.int("n"))
        .unwrap_or(0) as usize)
}

/// Plan, and — unless `dry_run` — merge and rebuild the weights in one transaction.
pub fn normalise(store: &Store, days: i64, dry_run: bool) -> Result<Report, StoreError> {
    let window = window_ago(days);
    let mut report = Report {
        dry_run,
        ..Report::default()
    };
    report.entities_before = counted(store, "SELECT count(*) n FROM entities")?;
    report.edges_before = counted(store, "SELECT count(*) n FROM edges")?;
    if dry_run {
        report.merges = plan(store)?;
        let (after, changed, added, dropped) = projected(store, &report.merges, &window)?;
        report.edges_after = after;
        report.weights_changed = changed;
        report.edges_added = added;
        report.edges_dropped = dropped;
        report.entities_after = report.entities_before - report.merges.len();
        report.kind_mismatches = report.merges.iter().filter(|m| m.kind_mismatch).count();
        return Ok(report);
    }
    let (merges, merged, rebuilt) = store.in_transaction(|tx| {
        let rows = query(tx, ENTITIES_SQL, &[])?;
        let shared = shared_articles(&query(tx, SHARED_SQL, &[])?);
        let aliases = query(tx, ALIAS_SQL, &[])?;
        let links = article_counts(&query(tx, LINKS_SQL, &[])?);
        let planned = plan_from(&rows, &aliases, &shared, &links);
        let ids: Vec<i64> = planned.iter().map(|merge| merge.from).collect();
        let texts = if ids.is_empty() {
            Vec::new()
        } else {
            query(tx, &texts_sql(&ids), &[])?
        };
        let merges = without_prose(&texts, &names_of(&rows), planned);
        let merged = apply(tx, &merges)?;
        let rebuilt = rebuild_edges(tx, &window)?;
        Ok((merges, merged, rebuilt))
    })?;
    report.entities_after = report.entities_before - merged;
    report.edges_after = rebuilt.edges;
    report.weights_changed = rebuilt.reweighed;
    report.labelled = rebuilt.labelled;
    report.kind_mismatches = merges.iter().filter(|m| m.kind_mismatch).count();
    report.merges = merges;
    Ok(report)
}

/// `normalise` — `{"db":…, "dry_run": true, "days": 30}`.
///
/// Absent `dry_run` means dry: a pass that writes has to be asked for, and the report
/// says which one ran.
pub fn cmd_normalise(input: &Json) -> Result<String, String> {
    let store = open(input).map_err(fail)?;
    let days = opt_i64(input, "days", EDGE_DAYS);
    let dry_run = match input.get("dry_run") {
        None | Some(Json::Null) => true,
        Some(Json::Bool(value)) => *value,
        Some(_) => return Err("'dry_run' must be true or false".to_string()),
    };
    let started = std::time::Instant::now();
    let mut report = normalise(&store, days, dry_run).map_err(fail)?;
    report.ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(report.json())
}
