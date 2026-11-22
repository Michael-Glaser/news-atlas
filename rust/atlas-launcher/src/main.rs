//! `atlas` — read the arguments, print the answer, set the exit code. Nothing else.
//!
//! Every decision lives in the library next to its test: the command line in
//! [`atlas_launcher::args`], the boot in [`atlas_launcher::start`], the checks in
//! [`atlas_launcher::doctor`], and the process boundary in
//! [`atlas_launcher::entry::main_entry`].

#![forbid(unsafe_code)]

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(atlas_launcher::main_entry(&argv));
}
