//! `atlas-migrate` — argv, stdout, the exit code, and nothing else.
//!
//! ```text
//! atlas-migrate verify                    # read-only report, non-zero on findings
//! atlas-migrate normalize --apply         # repair what the report proved
//! atlas-migrate carry --apply --limit 50  # 50 articles through the model, resumable
//! ```
//!
//! Every decision lives in the library: [`atlas_migrate::cli`] parses, runs and
//! reports; [`atlas_migrate::verify`], [`atlas_migrate::normalize`] and
//! [`atlas_migrate::carry`] decide what the answer is.

#![forbid(unsafe_code)]

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(atlas_migrate::cli::main(argv));
}
