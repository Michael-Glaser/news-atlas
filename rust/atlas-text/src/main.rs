//! The `atlas-text` binary: argv, stdin and stdout, and nothing else.
//!
//! Every string decision lives in the library (`src/lib.rs` and the five
//! operation modules beside it), so a caller reaches the same code without
//! running the binary.

fn main() {
    atlas_text::main_entry();
}
