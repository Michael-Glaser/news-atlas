//! The `atlas-guard` binary: the library plus argv/stdin dispatch.
//!
//! All logic lives in `lib.rs`, so `atlas-net` and `atlas-web` decide the same
//! way this binary does instead of re-implementing a security rule.

#![forbid(unsafe_code)]

fn main() {
    atlas_guard::main_entry();
}
