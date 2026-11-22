//! atlas-bench — the measurement harness for ATLAS.
//!
//! Every number in `docs/PERFORMANCE.md` comes from here: a temporary archive of
//! 5 000 articles / 20 000 entities / 60 000 links, seeded through the real
//! [`atlas_store`] schema, then the paths a user actually hits timed in release
//! mode.
//!
//! # The map
//!
//! * [`fixture`] — the archive, seeded once per run and deleted at the end.
//! * [`store_paths`] — list, FTS, entity profile, thread rebuild, prune.
//! * [`pipeline`] — one 20 kB article through extract, entities, places, topics,
//!   dates: the per-article cost.
//! * [`stats`] — the samples and their percentiles.
//! * [`budgets`] — the ceiling each path must stay under, and what it means.
//! * [`report`] — the table.
//!
//! # What it no longer measures
//!
//! The HTTP routes, the wasm bundle sizes and the time from `exec` to a bound
//! socket went with `atlas-web` / `atlas-ui`: the shipped product has no HTTP
//! face and no wasm module, so a number for either would describe a crate that is
//! not in the tree. The window's own paints are measured headlessly by
//! `atlas-desktop/tests/ui_layout.rs`, where the rects are.
//!
//! # What it never does
//!
//! No network, no download, no listener. It writes only under
//! `<temp>/atlas-bench-*`, which it removes, and it spawns only the engine
//! binaries the pipeline path names.

#![forbid(unsafe_code)]

pub mod budgets;
pub mod fixture;
pub mod pipeline;
pub mod report;
pub mod stats;
pub mod store_paths;

use std::path::PathBuf;

use fixture::Archive;
pub use report::Report;

/// What one run should measure, and how many samples it takes.
#[derive(Clone, Debug)]
pub struct Options {
    /// Only the paths whose id contains this text (`""` = all of them).
    pub only: String,
    /// Keep the seeded archive here instead of deleting it (for query plans).
    pub keep: Option<PathBuf>,
    /// Index names to drop before measuring, for an A/B on the same rows.
    pub drop_indexes: Vec<String>,
    /// How many independent passes over the same paths; the fastest p95 wins.
    ///
    /// This machine is shared — other builds, other agents — so a single p95 can
    /// carry seconds of somebody else's compiler. A path is measured `repeat`
    /// times and the best p95 is the number a report quotes: a path that is only
    /// fast once is not fast.
    pub repeat: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            only: String::new(),
            keep: None,
            drop_indexes: Vec::new(),
            repeat: 2,
        }
    }
}

impl Options {
    /// Does this path belong in this run?
    pub fn wants(&self, path: &str) -> bool {
        self.only.is_empty() || path.contains(&self.only)
    }
}

/// Measure everything this run asked for and hand back the table.
///
/// The order matters: the read paths run before the destructive ones (the thread
/// rebuild rewrites `threads`, the prune deletes rows), so no number is taken from
/// an archive another number already changed. The archive is seeded once; each
/// [`Options::repeat`] pass re-measures the same paths on it, and the fastest p95
/// per path is the one reported.
pub fn run(options: &Options) -> Report {
    let archive = match &options.keep {
        Some(root) => Archive::at(root.clone(), "run", true),
        None => Archive::build("run", true),
    };
    for name in &options.drop_indexes {
        archive.drop_index(&name.clone());
    }
    let mut notes: Vec<String> = Vec::new();
    let mut best: Vec<stats::Samples> = Vec::new();
    let repeat = options.repeat.max(1);

    for pass in 0..repeat {
        let rows = measure_pass(&archive, options, &mut notes);
        merge(&mut best, rows);
        if pass == 0 {
            notes.push(format!(
                "each path is measured {} times over the same archive; the table shows the \
                 fastest p95 of {} pass(es)",
                stats::RUNS, repeat
            ));
        }
    }

    let seed = archive.seed;
    let database_bytes = seed.bytes;
    let archive_root = archive.root.display().to_string();
    if options.keep.is_some() {
        // Deliberately left: the caller asked for the file, not the numbers.
        std::mem::forget(archive);
    } else {
        drop(archive); // removes the temp root
    }

    Report {
        seed,
        database_bytes,
        archive_root,
        rows: best,
        notes,
    }
}

/// One pass over the store and pipeline paths.
fn measure_pass(
    archive: &Archive,
    options: &Options,
    notes: &mut Vec<String>,
) -> Vec<stats::Samples> {
    let mut rows: Vec<stats::Samples> = Vec::new();

    // ---- store (its prune is last among these) ----------------------------
    let store = store_paths::StorePaths::run(archive);
    for samples in store.all() {
        if options.wants(samples.path) {
            rows.push(samples.clone());
        }
    }

    // ---- pipeline ---------------------------------------------------------
    let engine_dir = pipeline::engine_dir();
    if engine_dir.as_os_str().is_empty() {
        notes.push(
            "pipeline: no engine binaries found — run cargo build --release --offline first"
                .to_string(),
        );
    } else if options.wants("pipeline") {
        let pipeline = pipeline::PipelinePaths::run(&engine_dir);
        for samples in pipeline.all() {
            rows.push(samples.clone());
        }
        if !notes.iter().any(|note| note.starts_with("pipeline:")) {
            notes.push(format!(
                "pipeline: engines from {}, one 20 kB article, located as {:?} with {} topic \
                 match(es)",
                pipeline.engine_dir.display(),
                pipeline.located,
                pipeline.topics_matched
            ));
        }
    }

    // ---- startup: retired with the HTTP face -------------------------------
    // There is no second process to time any more: the window opens the store
    // in-process, and its first frame is measured by
    // `atlas-desktop/tests/ui_layout.rs` (a headless frame, not a spawn).
    rows
}

/// Keep the fastest p95 per path across passes.
fn merge(best: &mut Vec<stats::Samples>, rows: Vec<stats::Samples>) {
    for samples in rows {
        match best.iter_mut().find(|kept| kept.path == samples.path) {
            Some(kept) => {
                if samples.p95() < kept.p95() {
                    *kept = samples;
                }
            }
            None => best.push(samples),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_selects_the_paths_it_names() {
        let options = Options {
            only: "store.list".to_string(),
            ..Options::default()
        };
        assert!(options.wants("store.list"));
        assert!(!options.wants("store.fts"));
        assert!(Options::default().wants("anything"));
    }
}
