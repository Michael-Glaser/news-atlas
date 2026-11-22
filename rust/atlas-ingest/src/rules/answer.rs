//! The answer envelope: one shape for a child process and for a library call.
//!
//! Decides: the payload ceiling every engine question is held to, that an
//! engine's `{"error": …}` becomes a [`RulesError`] carrying the engine's own
//! words, and that an answer which is not a JSON object is reported as exactly
//! that (with the exit status, when there was a process).
//!
//! Does not decide: which transport answers the question ([`super::NativeRules`]
//! chooses), nor what any field of an answer means (the trait methods read the
//! fields). This module exists so those two things cannot disagree per
//! transport: a spawned engine and an in-process engine reach the caller through
//! the same `{"…": …}` text and the same parse.

use serde_json::{json, Value};

use crate::rules::RulesError;

/// The payload ceiling `server/textcore.py` enforced, unchanged: an engine reads
/// one object from stdin, and 4 MiB is far past the largest page this app reads
/// (`atlas_net::DEFAULT_MAX_BYTES`). It is checked for *both* transports so the
/// two cannot disagree about which questions are legal.
pub(crate) const MAX_PAYLOAD: usize = 4 << 20;

/// What one engine said: the bytes, and whether it left a success status.
///
/// A child process that refuses prints `{"error": …}` and exits 1 — the status
/// tells this apart from `{"fresh": …}` printed by a process that was killed
/// before it could finish. An in-process answer has no status of its own, so it
/// reports `true` unless the library said `Err`, in which case the error
/// envelope below is built and reported `false` — the same two facts the binary
/// would have produced.
pub(crate) struct Spoken {
    pub text: String,
    pub ok: bool,
}

impl Spoken {
    /// The envelope `atlas_lib::cli::fail` prints: `{"error": …}` and exit 1.
    ///
    /// Written here, once, so an in-process engine's refusal reaches the caller
    /// **character for character** as the same `{"error": …}` a process would
    /// have printed — the parity test compares the two.
    pub fn refused(message: &str) -> Spoken {
        Spoken {
            text: json!({ "error": message }).to_string(),
            ok: false,
        }
    }
}

/// Ask nothing: refuse a payload past the ceiling, before any transport runs.
pub(crate) fn check_payload(engine: &str, op: &str, text: &str) -> Result<(), RulesError> {
    if text.len() > MAX_PAYLOAD {
        return Err(RulesError::new(format!(
            "{} {}: payload is {} bytes, the contract caps it at {}",
            engine,
            op,
            text.len(),
            MAX_PAYLOAD
        )));
    }
    Ok(())
}

/// Read one answer: a JSON object, or the reason it is not one.
///
/// The engine's own `error` field is the message the model and the user read, so
/// it is preferred over a generic failure. `{"error": …}` from a child process is
/// the engine saying *why* it refused (a bad timestamp, an unknown subcommand);
/// throwing that away and reporting "could not answer" would leave the caller
/// with nothing to fix.
pub(crate) fn parse(engine: &str, op: &str, spoken: &Spoken) -> Result<Value, RulesError> {
    let value: Value = match serde_json::from_str(&spoken.text) {
        Ok(value) => value,
        Err(_) if spoken.ok => {
            return Err(RulesError::new(format!(
                "{} {} answered something that is not JSON",
                engine, op
            )))
        }
        // It exited non-zero and printed no object: not a refusal we can quote.
        Err(_) => {
            return Err(RulesError::new(format!(
                "{} {} could not answer (it printed nothing we can read and did not exit cleanly)",
                engine, op
            )))
        }
    };
    let Value::Object(map) = value else {
        return Err(RulesError::new(format!(
            "{} {} answered something that is not a JSON object",
            engine, op
        )));
    };
    if let Some(error) = map.get("error").and_then(Value::as_str) {
        return Err(RulesError::new(format!("{} {}: {}", engine, op, error)));
    }
    Ok(Value::Object(map))
}
