//! Five engines, in this process, through the very function their binaries call.
//!
//! Every engine crate here exposes `pub fn run(cmd: &str, raw: &str) ->
//! Result<String, String>` — the whole of its `main`: read `argv[1]`, read stdin,
//! answer one JSON object, refuse with a message. `atlas-places`, `atlas-topics`
//! and `atlas-dates` are built on `atlas_lib::cli::serve(crate::run)`;
//! `atlas-text` and `atlas-core` build the same envelope in their own private
//! module and call it from `main`. Either way `run` *is* the program, so calling
//! it here asks the **same function with the same bytes** and gets the same JSON
//! object back: there is one code path, and the process was only ever the
//! transport.
//!
//! The two crates that used to be asked as children got there by exporting the
//! envelope they already had — `atlas-text` moved its `cli` body into
//! `atlas-text/src/envelope.rs` and made the function public as `atlas_text::run`;
//! `atlas-core` made `atlas-core/src/entry.rs`'s `run` public — and the caps those
//! envelopes apply (`atlas_text::MAX_INPUT`, `atlas_text::MAX_WORDS`,
//! `atlas_text::MAX_FIELD`) are read from the crate that owns them. No arm below
//! restates an envelope, a cap or a refusal format.
//!
//! Decides: which engine name maps to which library, and that a library's `Err`
//! becomes the same `{"error": …}` envelope the binary would have printed
//! ([`Spoken::refused`], one implementation for all five).
//!
//! Does not decide: what a subcommand means (the engine's own modules), or
//! whether an in-process answer is acceptable (the parity test in
//! [`super::tests`] compares the two answers, byte for byte, for the same
//! payload — including a refusal).

use crate::rules::answer::Spoken;
use crate::rules::RulesError;

/// Which engines answer in-process. One list, so a caller and the transport
/// choice cannot disagree.
pub const ENGINES: &[&str] = &[
    "atlas-text",
    "atlas-core",
    "atlas-places",
    "atlas-topics",
    "atlas-dates",
];

/// Is this engine answered without a process?
pub fn answers_here(engine: &str) -> bool {
    ENGINES.contains(&engine)
}

/// Ask one of the five engines in this process.
///
/// `op` and `raw` are exactly what would be written to the child's argv and
/// stdin, so the answer is exactly what it would have printed.
pub(crate) fn ask(engine: &str, op: &str, raw: &str) -> Result<Spoken, RulesError> {
    let answer = match engine {
        "atlas-text" => atlas_text::run(op, raw),
        "atlas-core" => atlas_core::run(op, raw),
        "atlas-places" => atlas_places::run(op, raw),
        "atlas-topics" => atlas_topics::run(op, raw),
        "atlas-dates" => atlas_dates::run(op, raw),
        other => {
            return Err(RulesError::new(format!(
                "{} is asked in-process but has no library entry point (in-process engines: {})",
                other,
                ENGINES.join(", ")
            )))
        }
    };
    Ok(match answer {
        Ok(text) => Spoken { text, ok: true },
        Err(message) => Spoken::refused(&message),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_engine_name_has_a_library_to_answer_it() {
        // A name in `ENGINES` with no arm in `ask` would be an engine the wiring
        // promises and the code cannot answer, so each one is asked a question it
        // refuses: `Err` from the arm means the arm exists.
        for engine in ENGINES {
            let spoken = ask(engine, "definitely-not-an-op", "{}")
                .unwrap_or_else(|error| panic!("{} has no arm in ask: {}", engine, error.message()));
            assert!(!spoken.ok, "{} must refuse an unknown op", engine);
            assert!(
                spoken.text.starts_with("{\"error\":\""),
                "{} refused in a shape that is not the binary's: {}",
                engine,
                spoken.text
            );
        }
        assert!(
            ask("atlas-nonesuch", "op", "{}").is_err(),
            "an engine with no library must say so, not answer"
        );
    }
}
