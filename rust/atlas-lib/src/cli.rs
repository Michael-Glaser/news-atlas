//! The stdin/stdout contract every ATLAS binary follows.
//!
//! ```text
//! atlas-<crate> <subcommand>   <<< '{"...": ...}'
//! ```
//!
//! Success prints one JSON object on stdout and exits 0. Failure prints
//! `{"error": "..."}` on stdout and exits 1 — never a panic, never a stack
//! trace, never partial output — so the caller can always parse what comes back
//! and fall back to the Python implementation when it cannot.

use std::io::{self, IsTerminal, Read, Write};

use crate::json::Json;

/// Read all of stdin — unless stdin is a terminal, which means nobody is piping
/// anything into this process.
///
/// Measured before the fix: `bash rust/build.sh` on a machine where the script ran
/// `atlas-config audit` inside `$( )` sat for **15 minutes** with no output, because
/// `atlas-config` blocks on `read_to_string` of a terminal that never reaches EOF.
/// A person typing `atlas-config audit` and waiting for an answer got the same
/// hang — the subcommand had already decided what to do, it was only the input
/// that was missing. An empty string is the honest answer here: it is what the
/// caller receives today when nothing is piped in, and every handler already
/// treats it as "no input, use defaults".
pub fn read_stdin() -> Result<String, String> {
    if io::stdin().is_terminal() {
        return Ok(String::new());
    }
    let mut buf = String::new();
    match io::stdin().read_to_string(&mut buf) {
        Ok(_) => Ok(buf),
        Err(e) => Err(format!("cannot read stdin: {}", e)),
    }
}

/// The subcommand in `argv[1]`, or a clean error if it is missing.
pub fn subcommand(usage: &str) -> Result<String, String> {
    match std::env::args().nth(1) {
        Some(c) if !c.trim().is_empty() => Ok(c),
        _ => Err(format!("missing subcommand (expected: {})", usage)),
    }
}

/// Parse stdin and insist on a JSON object.
pub fn input_object(raw: &str) -> Result<Json, String> {
    let root = crate::json::parse(raw)?;
    match root {
        Json::Obj(_) => Ok(root),
        _ => Err("input must be a JSON object".to_string()),
    }
}

/// Print `text` plus a newline. Errors are ignored: a closed stdout means the
/// caller went away, which is not a failure we can report anywhere.
pub fn emit(text: &str) {
    let mut out = io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// Print `{"error": msg}` and exit 1.
pub fn fail(msg: &str) -> ! {
    let mut out = String::from("{\"error\":");
    crate::json::escape_str(&mut out, msg);
    out.push('}');
    emit(&out);
    std::process::exit(1);
}

/// Run `body`, turning its `Result` into the wire format.
///
/// ```ignore
/// fn main() {
///     atlas_lib::cli::serve(&|cmd, input| match cmd { ... });
/// }
/// ```
pub fn serve<F>(body: F)
where
    F: FnOnce(&str, &str) -> Result<String, String>,
{
    let cmd = match subcommand("a subcommand name") {
        Ok(c) => c,
        Err(e) => fail(&e),
    };
    let raw = match read_stdin() {
        Ok(s) => s,
        Err(e) => fail(&e),
    };
    match body(&cmd, &raw) {
        Ok(text) => emit(&text),
        Err(e) => fail(&e),
    }
}

/// Convenience: `o.get_str("k")` for required strings.
pub fn need_str(obj: &Json, key: &str) -> Result<String, String> {
    match obj.get(key).and_then(|v| v.as_str()) {
        Some(s) => Ok(s.to_string()),
        None => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn need_str_returns_empty_for_missing() {
        let obj = crate::json::parse(r#"{"a": 1}"#).unwrap();
        assert_eq!(need_str(&obj, "b").unwrap(), "");
    }

    #[test]
    fn input_object_rejects_arrays() {
        assert!(input_object("[1,2]").is_err());
        assert!(input_object(r#"{"a": 1}"#).is_ok());
    }
}
