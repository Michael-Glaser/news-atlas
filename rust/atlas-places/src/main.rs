//! The `atlas-places` binary: argv in, stdin in, stdout out.
//!
//! Nothing is decided here. `atlas_lib::cli` reads `argv[1]` and all of stdin,
//! hands both to [`atlas_places::run`], prints the one JSON object it gets back,
//! and turns an error into `{"error": …}` with exit code 1. Every decision —
//! the scan, the score, the bytes — lives in the library.

#![forbid(unsafe_code)]

fn main() {
    atlas_lib::cli::serve(atlas_places::run);
}
