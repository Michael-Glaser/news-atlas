//! The `atlas-ai` binary: argv, stdin, stdout — and nothing else.
//!
//! Four questions `server/ai.py` answered, with argv instead of a Python call:
//! `health`, `chat`, `verdict`, `analyze` (plus `specs` and `version`). Every answer
//! is one JSON object on stdout so `server/aicore.py` can read it without guessing,
//! and nothing is ever logged.
//!
//! Exit 0 when the question was answered (including "the model is not running",
//! which is a fact, not a failure); 1 when it could not be answered; 2 for bad
//! usage.
//!
//! Which question answers what, and with which exit code, is a decision, so it
//! lives in the library: [`atlas_ai::wire`].

#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    atlas_ai::wire::serve()
}
