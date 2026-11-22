//! Carrying the model's own answers across the whole archive.
//!
//! Decisions:
//!
//! * **The model decides, and only the model.** Every field here is re-derived
//!   through `atlas-ingest`'s own pass (`analyze_article`): entity names and types,
//!   relation labels, aliases, categories. The topic pass is `atlas-topics` through
//!   `atlas-ingest`'s `Rules`, because topic membership is the reader's topics'
//!   decision, not a guess.
//! * **A heuristic answer is reverted, never kept.** When the model does not answer,
//!   `atlas-ingest` falls back to a heuristic receipt; this crate snapshots the rows
//!   that pass would touch and puts them back, so an item the model did not classify
//!   stays exactly as the model left it — unclassified — and a later run retries it.
//!   That includes the label's provenance: the heuristic writes a `meta` row
//!   (`reason:<a>:<b>`) beside every edge it labels, and the row goes with the label
//!   ([`crate::retype::drop_orphan_reasons`]) — a row that says an edge was labelled for
//!   a reason while carrying no label is exactly the kind of leftover this pass may not
//!   leave behind.
//! * **An existing entity type is re-derived, not overwritten by a guess.** A stored
//!   type that no model has confirmed is cleared for the article being carried, the
//!   article's links are detached so `atlas-graph` sees the model's names as new (it
//!   never relabels an entity it has already typed), and every link, count and
//!   timestamp is restored afterwards — with the snapshot in `meta`, so an interrupted
//!   run is finished by the next one instead of guessed at.
//! * **Resumable and rate-limited.** Each article gets a receipt, `--limit` and
//!   `--max-minutes` bound a run, `--rate-ms` paces it, and a resume skips what has a
//!   receipt.
//!
//! It does not: fetch a page, search the web, write a tag (`tag_article` is the
//! agent's pen and the archive cannot tell a heuristic tag from a model tag — see
//! `tag.provenance`), invent a value for an item the model skipped, or run when the
//! model is not answering.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use atlas_ingest::{Config, Pipeline, TopicRow};
use atlas_store::{SqlValue, Store};

use crate::error::{MigrateError, Result};
use crate::ledger::{self, Record, Snapshot};
use crate::open;
use crate::queries;

/// How far one carry run goes.
#[derive(Clone, Debug)]
pub struct Options {
    /// Articles to classify in this run (`0` = no bound).
    pub limit: usize,
    /// Milliseconds between two model calls: the model is local and single-threaded,
    /// so this is what keeps the machine usable while the archive is carried.
    pub rate_ms: u64,
    /// Re-carry articles that already have a receipt.
    pub force: bool,
    /// Only the deterministic topic pass; no model call, no engine for the model.
    pub topics_only: bool,
    /// Re-derive stored entity types (the detach/restore pass).
    pub retype: bool,
    /// Include hidden duplicates (`dupe=1`).
    pub include_dupes: bool,
    /// Give up after this many minutes (`None` = no bound).
    pub max_minutes: Option<u64>,
    /// Only these article ids.
    pub ids: Vec<i64>,
    /// Print what would happen, write nothing, call no model.
    pub dry_run: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            limit: 0,
            rate_ms: 250,
            force: false,
            topics_only: false,
            retype: true,
            include_dupes: false,
            max_minutes: None,
            ids: Vec::new(),
            dry_run: true,
        }
    }
}

/// What one carry run did.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Articles the run could choose from.
    pub candidates: usize,
    /// Articles the run looked at.
    pub considered: usize,
    /// Articles where the model answered.
    pub classified: usize,
    /// Articles where it did not, and the heuristic output was reverted.
    pub unclassified: usize,
    /// Articles skipped because their receipt is already there.
    pub already: usize,
    /// Articles whose pass failed outright (a store error).
    pub failed: usize,
    /// Topic assignments written or refreshed.
    pub topics: usize,
    /// Entity types replaced by the model's answer.
    pub retyped: usize,
    /// Entity types the model answered with the type they already carried, so a re-run
    /// does not mistake a confirmation for a guess.
    pub confirmed: usize,
    /// Entity types left unclassified because the model did not name the entity.
    pub untyped: usize,
    /// Writes a heuristic answered with, and the guard undid.
    pub reverted: usize,
    /// Why the run stopped early, when it did.
    pub stopped: String,
    /// The run could not reach the model at all, so it classified nothing. Structured
    /// because the exit code depends on it: sniffing `stopped` for a phrase would let a
    /// wording change turn "could not run" into "clean".
    pub refused: bool,
    /// Ids the caller asked for that name no article.
    pub missing_ids: Vec<i64>,
}

impl Outcome {
    pub fn model_was_used(&self) -> bool {
        self.classified > 0
    }
}

/// Carry the archive at `path`.
///
/// Opens the archive through `atlas-store` and wires `atlas-ingest`'s production
/// pipeline (the real engines, the local model over loopback). [`run_with`] is the
/// same run over a pipeline a test supplies.
pub fn run(path: &Path, root: &Path, options: &Options) -> Result<Outcome> {
    // A dry run reads the archive read-only: no engine, no model, no write access.
    if options.dry_run {
        let archive = crate::open::Archive::open_read_only(path)?;
        return dry_run(&archive, options);
    }
    let store = Arc::new(open::open_write(path)?);
    let config = Config::for_root(root);
    let pipeline = Pipeline::native(store, config)
        .map_err(|error| MigrateError::new(error.message()))?;
    run_with(&pipeline, options)
}

/// The carry-over over one pipeline: every decision below is `atlas-ingest`'s and
/// `atlas-store`'s, this function only spends them.
pub fn run_with(pipeline: &Pipeline, options: &Options) -> Result<Outcome> {
    let store = pipeline.store().clone();
    let mut outcome = Outcome::default();

    // A dry run over a real pipeline still writes nothing and asks no model.
    if options.dry_run {
        let archive = crate::open::Archive::open_read_only(store.path())?;
        return dry_run(&archive, options);
    }

    // An interrupted run is finished before anything new starts.
    let pending = ledger::pending(&store)?;
    for snapshot in &pending {
        crate::retype::restore(&store, snapshot)?;
        ledger::clear_snapshot(&store, snapshot.article)?;
    }

    let candidates = candidates(&store, options)?;
    outcome.candidates = candidates.len();
    // Ids the caller named that named no article: reported, not silently dropped.
    outcome.missing_ids = options
        .ids
        .iter()
        .copied()
        .filter(|id| !candidates.contains(id))
        .collect();

    let topics: Vec<TopicRow> = pipeline
        .active_topics()
        .map_err(|error| MigrateError::new(error.message()))?;

    // The model is asked before anything is touched: a run with no model must not
    // clear a link or a type.
    if !options.topics_only && !pipeline.model().online() {
        outcome.stopped = "the local model is not answering - nothing was classified".to_string();
        outcome.refused = true;
        return Ok(outcome);
    }

    let started = Instant::now();
    let budget = options.max_minutes.map(|minutes| Duration::from_secs(minutes * 60));
    let mut since_probe = 0usize;

    for id in candidates {
        if options.limit > 0 && outcome.classified + outcome.unclassified >= options.limit {
            outcome.stopped = format!("limit {} reached", options.limit);
            break;
        }
        if let Some(budget) = budget {
            if started.elapsed() >= budget {
                outcome.stopped = "time budget reached".to_string();
                break;
            }
        }
        if !options.force && ledger::done(&store, id) {
            outcome.already += 1;
            continue;
        }
        outcome.considered += 1;

        if !options.topics_only {
            // The model can go away mid-run (a user closing llama.cpp): stop instead of
            // writing heuristic values for the rest of the archive.
            since_probe += 1;
            if since_probe >= 20 {
                since_probe = 0;
                if !pipeline.model().online() {
                    outcome.stopped =
                        "the local model stopped answering - the run stopped".to_string();
                    break;
                }
            }
            match classify(&pipeline, id, options.retype, &mut outcome) {
                Ok(record) => {
                    if record.is_model() {
                        outcome.classified += 1;
                    } else {
                        outcome.unclassified += 1;
                    }
                    outcome.reverted += record.reverted;
                    if let Err(error) = ledger::record(&store, id, &record) {
                        outcome.failed += 1;
                        outcome.stopped = error.message().to_string();
                        break;
                    }
                }
                Err(error) => {
                    outcome.failed += 1;
                    outcome.stopped = error.message().to_string();
                    break;
                }
            }
            if options.rate_ms > 0 {
                std::thread::sleep(Duration::from_millis(options.rate_ms));
            }
        }

        // The topic pass is deterministic and always runs: an article's membership in
        // the reader's topics is not the model's question.
        match crate::topics::assign(&pipeline, &store, id, &topics) {
            Ok(written) => {
                outcome.topics += written;
                if written > 0 {
                    if let Some(mut record) = ledger::read(&store, id) {
                        record.topics = written;
                        let _ = ledger::record(&store, id, &record);
                    }
                }
            }
            Err(error) => {
                outcome.failed += 1;
                outcome.stopped = error.message().to_string();
                break;
            }
        }
    }
    Ok(outcome)
}

/// What a dry run reports: how many articles are left, and why none of them was touched.
///
/// The numbers are the numbers the real run would produce, bounds included: `--limit` caps
/// what a run may consider, and an id in `--ids` that names no article is dropped and
/// reported instead of being counted as work. A dry run that says "considered 1 405" for a
/// run that would consider 5 is a lie a script cannot tell from the truth.
pub fn dry_run(archive: &crate::open::Archive, options: &Options) -> Result<Outcome> {
    let (candidates, missing_ids) = if options.ids.is_empty() {
        // Only read the candidate list when the run will use it: a `--ids` run naming three
        // articles should not walk 1 405 rows to answer.
        let rows = if options.include_dupes {
            archive.query(queries::CARRY_CANDIDATES_ALL, &[])?
        } else {
            archive.query(queries::CARRY_CANDIDATES, &[])?
        };
        (
            rows.iter().filter_map(|row| row.int("id")).collect::<Vec<i64>>(),
            Vec::new(),
        )
    } else {
        existing(archive, &options.ids)?
    };
    let todo: Vec<i64> = candidates
        .iter()
        .copied()
        .filter(|id| options.force || !ledger::done_readonly(archive, *id))
        .collect();
    let bounded = if options.limit > 0 && todo.len() > options.limit {
        options.limit
    } else {
        todo.len()
    };
    let pending = archive.query(
        queries::META_KEYS_LIKE,
        &[SqlValue::Text(format!("{}%", ledger::SNAPSHOT_PREFIX))],
    )?;
    let mut stopped = "dry run: no model call, no write".to_string();
    if !pending.is_empty() {
        stopped = format!(
            "{} snapshot(s) from an interrupted run would be restored first",
            pending.len()
        );
    }
    if bounded < todo.len() {
        stopped.push_str(&format!(
            "; --limit {} would stop after {} of the {} articles left",
            options.limit,
            bounded,
            todo.len()
        ));
    }
    if !missing_ids.is_empty() {
        stopped.push_str(&format!(
            "; no article with id {}",
            missing_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<String>>()
                .join(",")
        ));
    }
    Ok(Outcome {
        candidates: candidates.len(),
        already: candidates.len() - todo.len(),
        considered: bounded,
        missing_ids,
        stopped,
        ..Outcome::default()
    })
}

/// Split the caller's ids into the ones the archive holds and the ones it does not.
fn existing(
    archive: &crate::open::Archive,
    ids: &[i64],
) -> Result<(Vec<i64>, Vec<i64>)> {
    let mut present: Vec<i64> = Vec::new();
    let mut absent: Vec<i64> = Vec::new();
    for id in ids {
        let found = archive
            .query_one(queries::ARTICLE_EXISTS, &[SqlValue::Integer(*id)])?
            .is_some();
        if found {
            present.push(*id);
        } else {
            absent.push(*id);
        }
    }
    Ok((present, absent))
}

/// The articles this run may work on.
///
/// An id the caller named that names no article is dropped here and reported in
/// [`Outcome::missing_ids`]: `analyze_article` on a row that is not there would write a
/// receipt for nothing, and a dry run that counted it would promise work it cannot do.
fn candidates(store: &Store, options: &Options) -> Result<Vec<i64>> {
    if !options.ids.is_empty() {
        let mut present: Vec<i64> = Vec::new();
        for id in &options.ids {
            if store
                .query_one(queries::ARTICLE_EXISTS, &[SqlValue::Integer(*id)])?
                .is_some()
            {
                present.push(*id);
            }
        }
        return Ok(present);
    }
    let sql = if options.include_dupes {
        queries::CARRY_CANDIDATES_ALL
    } else {
        queries::CARRY_CANDIDATES
    };
    let rows = store.query(sql, &[])?;
    Ok(rows.iter().filter_map(|row| row.int("id")).collect())
}

/// One model pass for one article, with the guard and the restore around it.
fn classify(pipeline: &Pipeline, article_id: i64, retype: bool, outcome: &mut Outcome) -> Result<Record> {
    let store = pipeline.store();
    let pre = crate::retype::PreState::capture(store, article_id)?;

    // 1. Make the model's answer the one `link_article` writes: detach the links and
    //    clear the types no model has confirmed. The snapshot is on disk first.
    if retype && !pre.links.is_empty() {
        let snapshot = Snapshot {
            article: article_id,
            at: atlas_store::now_iso(),
            links: pre.links.clone(),
        };
        ledger::save_snapshot(store, &snapshot)?;
        store.exec(queries::DETACH_ARTICLE_ENTITIES, &[SqlValue::Integer(article_id)])?;
        for link in &pre.links {
            if !ledger::entity_verified(store, link.id) {
                store.exec(queries::CLEAR_ENTITY_TYPE, &[SqlValue::Integer(link.id)])?;
            }
        }
    }

    let answer = pipeline.analyze_article(article_id, true);
    let model = answer
        .get("model")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // 2. Put the article back together: the links the model did not name are restored
    //    exactly, and the bookkeeping `link_article` increments is set back to what it
    //    was, so the carry-over leaves no drift in a count it did not mean to change.
    //    A type the model did not restate stays empty — the documented "unclassified"
    //    state — and loses the category that came from the old guess.
    if retype && !pre.links.is_empty() {
        let restored = crate::retype::restore(
            pipeline.store(),
            &Snapshot {
                article: article_id,
                at: String::new(),
                links: pre.links.clone(),
            },
        )?;
        outcome.retyped += restored.replaced;
        outcome.confirmed += restored.confirmed;
        outcome.untyped += restored.unclassified;
        ledger::clear_snapshot(store, article_id)?;
    }

    let mut record = Record {
        at: atlas_store::now_iso(),
        source: if model { "model".to_string() } else { "none".to_string() },
        entities: answer.get("entities").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize,
        categories: answer.get("categories").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize,
        topics: 0,
        reverted: 0,
    };

    // 3. No model answer means the receipt was a heuristic: undo exactly what it
    //    wrote, and drop the receipt so the next run asks again.
    if !model {
        record.reverted = pre.revert(store, article_id)?;
        let _ = store.exec(
            queries::DELETE_META_KEY,
            &[SqlValue::Text(format!("enrich:{}", article_id))],
        );
    }
    Ok(record)
}
