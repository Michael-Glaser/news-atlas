//! `atlas-bench` — argv in, the table out, and the exit code.
//!
//! ```text
//! atlas-bench                       every path, release build
//! atlas-bench --only store.list     one family of paths
//! atlas-bench --json                the same numbers as JSON
//! atlas-bench --repeat 3            three passes, the fastest p95 per path wins
//! ```
//!
//! Everything else is [`atlas_bench`].

use atlas_bench::{budgets, run, Options};

fn main() {
    let options = match parse(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => return,
        Err(message) => {
            eprintln!("atlas-bench: {}", message);
            std::process::exit(2);
        }
    };
    let json = std::env::args().any(|arg| arg == "--json");
    let report = run(&options);
    if json {
        match serde_json::to_string_pretty(&report.json()) {
            Ok(text) => println!("{}", text),
            Err(error) => {
                eprintln!("atlas-bench: the report will not serialise: {}", error);
                std::process::exit(1);
            }
        }
    } else {
        print!("{}", report.table());
    }
    let regressions = report.regressions();
    if !regressions.is_empty() {
        eprintln!();
        for (path, p95, budget) in &regressions {
            eprintln!(
                "REGRESSION {}: p95 {:.2} ms is more than {:.0}% over the {:.1} ms budget",
                path,
                p95,
                budgets::TOLERANCE * 100.0,
                budget
            );
        }
        std::process::exit(1);
    }
}

/// Parse argv. `Ok(None)` means help was asked for and printed.
fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => {
                options.only = args.next().ok_or("--only needs a path fragment")?;
            }
            "--rust-dir" => {
                // Retired with the wasm bundle: the only path that needed the
                // build output. Kept as a rejection, not a silent ignore, so an
                // old script fails loudly instead of measuring something else.
                return Err("--rust-dir was retired with the wasm UI".to_string());
            }
            "--no-startup" => {
                // Retired with the HTTP face: there is no spawned server left to
                // skip. An old command line must not look like it still worked.
                return Err("--no-startup was retired with the HTTP face".to_string());
            }
            "--keep" => {
                options.keep = Some(args.next().ok_or("--keep needs a directory")?.into());
            }
            "--drop-index" => {
                options
                    .drop_indexes
                    .push(args.next().ok_or("--drop-index needs an index name")?);
            }
            "--repeat" => {
                let text = args.next().ok_or("--repeat needs a number")?;
                options.repeat = text
                    .parse()
                    .map_err(|_| format!("--repeat {} is not a number", text))?;
            }
            "--json" => {}
            "--help" | "-h" => {
                println!(
                    "atlas-bench [--only <path fragment>] [--json]\n\
                     \n\
                     Measures the paths a user hits on a seeded archive: the store statements\n\
                     and the per-article pipeline. Exits 1 when a path is more than 20% over its\n\
                     budget (see atlas_bench::budgets)."
                );
                return Ok(None);
            }
            other => return Err(format!("unknown argument {}", other)),
        }
    }
    Ok(Some(options))
}
