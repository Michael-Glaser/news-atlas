//! atlas-config — the native settings core.
//!
//! Every setting ATLAS has, the environment it reads, the file it stores them in,
//! the paths they point at and the rules that keep the machine safe. The library
//! does the work; this binary is the same code with argv/stdin.

#![forbid(unsafe_code)]

fn main() {
    atlas_config::main_entry();
}
