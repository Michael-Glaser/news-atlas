//! The `atlas-core` binary: argv, stdin and stdout, and nothing else.
//!
//! Every decision lives in the library (`src/lib.rs` and the modules beside it),
//! so a caller reaches the same code without running the binary.

fn main() {
    atlas_core::main_entry();
}
