//! atlas-migrate — carrying an existing ATLAS archive over to the Rust build, and
//! proving it was carried.
//!
//! The archive predates this workspace: `data/atlas.db` holds the articles, entities,
//! links, threads, tags, alerts, monitors and topics a Python build wrote, and some of
//! what it holds is wrong — a full-text index that lagged behind an edit, a row that
//! points at an article the retention pass removed, two spellings of one newspaper, a
//! place nothing in the text supports, entity types a name-shape heuristic guessed.
//! This crate finds each of those, states it, and fixes exactly that.
//!
//! # Three passes, in this order
//!
//! | Pass | Answers | Writes |
//! | --- | --- | --- |
//! | [`verify`] | what does the archive hold, and what disagrees? | nothing — the connection is `SQLITE_OPEN_READ_ONLY` and `query_only` |
//! | [`normalize`] | which statements repair the disagreement? | nothing on `--dry-run` (the default); one transaction on `--apply` |
//! | [`carry`] | what did the model never actually classify? | the model's own answers, through `atlas-ingest`, one article at a time |
//!
//! # The map
//!
//! * [`open`] — the read-only archive, and the write store every fix goes through.
//! * [`queries`] — every statement this crate runs, named after the invariant it reads.
//! * [`sample`] — row counts and the sampled rows of every table.
//! * [`fts`] — is the full-text index complete, and does `ORDER BY rank` answer?
//! * [`refs`] — rows pointing at an article, entity, topic, category, thread or alias
//!   that is gone.
//! * [`invariants`] — the pairs `docs/DATA-MODEL.md` says never happen.
//! * [`publisher`] — one outlet stored under two spellings.
//! * [`place`] — a stored place the article's own text does not support.
//! * [`tags`] — a tag that cannot be a label, repeats another, or repeats the outlet.
//! * [`findings`] — what a disagreement is, and the statements that fix it.
//! * [`report`] — the verification report, as text and as JSON.
//! * [`normalize`] — plan, list, apply.
//! * [`carry`] / [`ledger`] — the model pass, its receipts and its crash-safe snapshots.
//! * [`retype`] — making an entity's type the model's answer, without losing a link.
//! * [`topics`] — the deterministic topic pass (`atlas-topics`, no model).
//! * [`engines`] — the rule engines, and what a check says when they are not built.
//! * [`redact`] — what a report may print: never article text.
//! * [`cli`] — argv, stdout, the exit code.
//!
//! # What this crate will not do
//!
//! * It does not decide what is news, what a name is, what a place is, or whether a tag
//!   is true. Those are the local model's and the rule engines' decisions; this crate
//!   asks them and records the answer.
//! * It does not delete an article, a source, an entity or a topic. The only rows it
//!   removes are references to rows that are already gone, tags that cannot stand as a
//!   label, and the values `--apply` was asked to replace.
//! * It never writes a value a heuristic guessed: when the model does not answer, the
//!   writes that pass attempted are rolled back and the item stays unclassified.
//! * It does not print article text, a prompt or a page body — a report names ids,
//!   counts, schema and reasons (see [`redact`]).
//! * It opens no socket, binds no port, downloads nothing, and never touches a process
//!   it did not start (`atlas-ingest`'s rule engines are the only child processes, one
//!   per question, with a timeout).

#![forbid(unsafe_code)]

pub mod carry;
pub mod cli;
pub mod engines;
pub mod error;
pub mod findings;
pub mod fts;
pub mod invariants;
pub mod ledger;
pub mod normalize;
pub mod open;
pub mod place;
pub mod publisher;
pub mod queries;
pub mod redact;
pub mod refs;
pub mod retype;
pub mod report;
pub mod sample;
pub mod tags;
pub mod topics;
pub mod verify;

#[cfg(test)]
pub mod fixture;
#[cfg(test)]
mod tests;

pub use carry::{Options as CarryOptions, Outcome as CarryOutcome};
pub use error::{MigrateError, Result};
pub use findings::{Batch, Finding, Kind, Severity};
pub use normalize::Plan;
pub use report::Report;
pub use verify::Limits;

/// What a run was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Read-only report.
    Verify,
    /// Repair from a report.
    Normalize,
    /// Re-derive the model's fields.
    Carry,
    /// Verify, normalize, verify, carry.
    All,
}

impl Command {
    pub fn name(self) -> &'static str {
        match self {
            Command::Verify => "verify",
            Command::Normalize => "normalize",
            Command::Carry => "carry",
            Command::All => "all",
        }
    }
}
