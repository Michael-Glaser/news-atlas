//! atlas-graph — the JSON CLI of the relation layer.
//!
//! ```text
//! atlas-graph <subcommand>   <<< '{"db": "/path/atlas.db", …}'
//! ```
//!
//! One JSON object in on stdin, one JSON object out on stdout, exit 0; a failure
//! prints `{"error": …}` on stdout and exits 1. The engine, the database half and
//! the subcommand table live in the library (`atlas_graph`), so a test can call
//! either without a process.
//!
//! The pure subcommands (`edges`, `why`, `threads`, `neighbours`) never touch a
//! file. The ones that own their tables (`link_article`, `recompute`, `subgraph`,
//! `path`, `links_for`, `profile`, `top_entities`) open exactly one file — the
//! database named by `"db"` — and nothing else: no socket, no port, no thread, no
//! child process.

#![forbid(unsafe_code)]

fn main() {
    atlas_lib::cli::serve(&atlas_graph::run);
}
