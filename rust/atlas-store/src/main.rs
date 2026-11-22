//! The `atlas-store` binary: argv, stdin, stdout — and nothing else.
//!
//! Same wire contract as every other ATLAS helper: one JSON object on stdin, one
//! JSON object on stdout, exit 0 on success. A failure prints `{"error": …}` and
//! exits 1 — never a panic, never a stack trace, never a value echoed back.
//!
//! ```text
//! echo '{"db":"data/atlas.db","sql":"SELECT COUNT(*) c FROM articles"}' \
//!   | atlas-store query
//! {"rows":[{"c":1435}],"count":1}
//! ```
//!
//! Ops: `init`, `query`, `one`, `exec`, `many`, `meta_get`, `meta_set`, `search`,
//! `prune`. Which op answers what is a decision, so it lives in the library:
//! [`atlas_store::wire`], with the field validation next to it in
//! [`atlas_store::fields`].

#![forbid(unsafe_code)]

fn main() {
    atlas_store::wire::serve();
}
