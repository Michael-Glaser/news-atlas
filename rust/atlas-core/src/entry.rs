//! `entry` — the process boundary: argv, stdin, stdout, exit code.
//!
//! Decides how one request arrives (the subcommand from argv, stdin read whole),
//! which module answers it, and how a request error leaves the process:
//! `{"error": ...}` on stdout and exit status 1. `main_entry` is the single call
//! `src/main.rs` makes.
//!
//! It decides nothing about text, HTML or scoring: every rule lives in the
//! module the subcommand names, and this file only routes to it.

use std::io::{self, Read, Write};

pub fn main_entry() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = match args.get(1) {
        Some(c) => c.as_str(),
        None => fail("missing subcommand (expected: extract | entities | similar)"),
    };
    let input = match read_stdin() {
        Ok(s) => s,
        Err(e) => fail(&e),
    };
    let output = match run(cmd, &input) {
        Ok(s) => s,
        Err(e) => fail(&e),
    };
    let mut stdout = io::stdout();
    let _ = stdout.write_all(output.as_bytes());
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}

/// Dispatch one subcommand. Returns the JSON text to print on stdout.
///
/// This is the whole program `main_entry` runs below, and it is public because it
/// is public *in the same sense*: another crate in this workspace asks
/// `atlas-core` a question in its own process by calling this function, with the
/// bytes it would have written to a child's stdin (`atlas-ingest`'s
/// `rules::inproc` does, for `extract` and `entities`). `Ok` is the line the
/// binary prints, `Err` is the message the binary would have escaped into
/// `{"error": …}` — one envelope, reached two ways, never copied.
pub fn run(cmd: &str, input: &str) -> Result<String, String> {
    let root = crate::json::parse(input)?;
    match &root {
        crate::json::Json::Obj(_) => {}
        _ => return Err("input must be a JSON object".to_string()),
    }
    match cmd {
        "extract" => crate::extract::run(&root),
        "entities" => crate::entities::run(&root),
        "similar" => crate::similar::run(&root),
        other => Err(format!("unknown subcommand: {}", other)),
    }
}

fn read_stdin() -> Result<String, String> {
    let mut buf = String::new();
    match io::stdin().read_to_string(&mut buf) {
        Ok(_) => Ok(buf),
        Err(e) => Err(format!("cannot read stdin: {}", e)),
    }
}

/// Print `{"error": msg}` on stdout and exit with status 1.
fn fail(msg: &str) -> ! {
    let mut out = String::from("{\"error\":");
    crate::json::escape_str(&mut out, msg);
    out.push_str("}\n");
    let mut stdout = io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
    std::process::exit(1);
}

// ---------------------------------------------------------------------------
// json — a minimal parser / writer, just enough for this tool
