//! `entry` — the process boundary: argv, stdin, stdout, exit code.
//!
//! Decides how one request arrives (the subcommand from argv, stdin read up to
//! `MAX_STDIN`), which module answers it, and how a request error leaves the
//! process: `{"error": ...}` on stdout and exit status 1. `main_entry` is the
//! single call `src/main.rs` makes.
//!
//! It decides no security verdict. Every rule lives in the module the
//! subcommand names; this file only routes to it and prints the result.

use std::io::{self, Read, Write};

/// Hard cap on the whole stdin payload, JSON escaping included.
const MAX_STDIN: usize = 4 * 1024 * 1024;

pub fn main_entry() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = match args.get(1) {
        Some(c) => c.as_str(),
        None => fail(
            "missing subcommand (expected: url | clean_text | sanitize_html | token | redact | address)",
        ),
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
pub(crate) fn run(cmd: &str, input: &str) -> Result<String, String> {
    let root = crate::json::parse(input)?;
    if !matches!(root, crate::json::Json::Obj(_)) {
        return Err("input must be a JSON object".to_string());
    }
    match cmd {
        "url" => crate::urlsafe::run(&root),
        "clean_text" => crate::cleantext::run(&root),
        "sanitize_html" => crate::sanitize::run(&root),
        "token" => crate::token::run(&root),
        "redact" => crate::redact::run(&root),
        "address" => crate::address::run(&root),
        other => Err(format!("unknown subcommand: {}", other)),
    }
}

/// Read stdin, refusing anything past `MAX_STDIN` instead of growing a buffer
/// without a bound.
fn read_stdin() -> Result<String, String> {
    let stdin = io::stdin();
    let mut handle = stdin.lock().take((MAX_STDIN + 1) as u64);
    let mut buf = String::new();
    if handle.read_to_string(&mut buf).is_err() {
        return Err("cannot read stdin as utf-8".to_string());
    }
    if buf.len() > MAX_STDIN {
        return Err(format!("stdin larger than {} bytes", MAX_STDIN));
    }
    Ok(buf)
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

