//! The licence gate: what a reader accepts before anything is downloaded.
//!
//! The window has its own licence screen (`atlas-desktop/src/views/license.rs`) and it is
//! the right place for the full text. But the download happens *before* the window ever
//! opens — the app never fetches anything at run time, so the 6.5 GB arrive from a program
//! the person started, and "I accept" has to come before that, not after it. So this crate
//! asks the same question, in the same words it can fit in a terminal, and records the
//! answer:
//!
//! * **the same document** — [`VERSION`] is the version string the window's screen uses, and
//!   a test reads that file and fails if the two ever drift. Consent to a document is
//!   consent to *that* document, so a new sentence there must be a new version here.
//! * **the same three points** — this code is Apache-2.0; the crates, the vendored
//!   llama.cpp tree and the weights keep their own terms; and there is no warranty, because
//!   a model can be wrong.
//! * **one file** — `data/license.json`, written atomically, naming the version, the moment
//!   and who recorded it. That is the handoff: the window can read it instead of asking a
//!   second time, and until it does, it asks in full on its own screen — which is not a bug,
//!   it is the same question with the whole text in front of the reader.
//!
//! Nothing here downloads anything: it asks, it records, and it says what happens next.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The document version, identical to `atlas-desktop`'s licence screen.
///
/// `atlas-desktop/src/views/license.rs::VERSION` is the authority; [`tests`] reads that file
/// and compares, so a change there without a change here is a failing test rather than two
/// consents to two different documents.
pub const VERSION: &str = "apache-2.0+notice-2";

/// Where the record lives, relative to the root — the contract with the window.
pub const RELATIVE_PATH: &str = "data/license.json";

/// What was accepted, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub version: String,
    /// Seconds since the Unix epoch. No date formatting here: the window has the fonts.
    pub accepted_at: u64,
    /// Which command recorded it, so a record written by a script is distinguishable from
    /// one written by a person answering a prompt.
    pub by: String,
}

/// Whether this machine has answered the gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Answered, for this exact document version.
    Accepted(Record),
    /// Not answered — or answered for a *different* version, which is not an answer to
    /// this one.
    Missing,
}

/// The record's path for a root.
pub fn path(root: &Path) -> PathBuf {
    root.join("data").join("license.json")
}

/// Read the record. A missing, unreadable or older-version file is [`State::Missing`]:
/// anything else would let an old acceptance answer for a new document.
pub fn state(root: &Path) -> State {
    let Ok(text) = fs::read_to_string(path(root)) else {
        return State::Missing;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return State::Missing;
    };
    let version = value.get("version").and_then(serde_json::Value::as_str).unwrap_or_default();
    if version != VERSION {
        return State::Missing;
    }
    State::Accepted(Record {
        version: version.to_string(),
        accepted_at: value.get("accepted_at").and_then(serde_json::Value::as_u64).unwrap_or(0),
        by: value
            .get("by")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// Record the acceptance, atomically: `license.json.new` beside it, then a rename.
pub fn accept(root: &Path, by: &str) -> Result<Record, String> {
    let target = path(root);
    let record = Record {
        version: VERSION.to_string(),
        accepted_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0),
        by: by.to_string(),
    };
    let text = format!(
        "{{\"version\":{version},\"accepted_at\":{at},\"by\":{by}}}\n",
        version = quote(&record.version),
        at = record.accepted_at,
        by = quote(&record.by),
    );
    let Some(dir) = target.parent() else {
        return Err(format!("{} has no directory", target.display()));
    };
    fs::create_dir_all(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let staged = staged(&target);
    if let Err(error) = fs::write(&staged, text) {
        let _ = fs::remove_file(&staged);
        return Err(format!("cannot write {}: {error}", staged.display()));
    }
    if let Err(error) = fs::rename(&staged, &target) {
        let _ = fs::remove_file(&staged);
        return Err(format!("cannot replace {}: {error}", target.display()));
    }
    Ok(record)
}

/// `license.json` → `license.json.new`, in the same directory so the rename stays on one
/// volume and a reader never sees half a record.
fn staged(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|name| name.to_os_string()).unwrap_or_default();
    name.push(".new");
    path.with_file_name(name)
}

/// JSON string quoting for the two text fields. Deliberately minimal: the values are a
/// constant and a command name, and a full serializer for two strings is a dependency we
/// do not need.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// The version string the window's own licence screen uses, read from its source when that
/// file is in this checkout.
///
/// A library module does not print (`atlas-guard`'s audit keeps it that way: a print is
/// unredacted, unbounded and cannot be turned off), so the answer to "can the pin be
/// checked" is a value: `Ok(version)` when the screen's source was read, and `Err(reason)`
/// — naming the file — when it is not here. A test can then tell "nothing to compare" from
/// "the two documents disagree", which is the difference that matters.
pub fn window_version() -> Result<String, String> {
    let screen = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("atlas-desktop")
        .join("src")
        .join("views")
        .join("license.rs");
    let text = fs::read_to_string(&screen)
        .map_err(|_| format!("{} is not in this checkout", screen.display()))?;
    let marker = "pub const VERSION: &str =";
    text.lines()
        .find_map(|line| line.trim().strip_prefix(marker))
        .map(|rest| rest.trim().trim_end_matches(';').trim().trim_matches('"').to_string())
        .ok_or_else(|| format!("{} names no VERSION", screen.display()))
}

/// What the person is shown before the question. Plain English, no jargon, and it says
/// what this program is about to do to their machine as well as what the licence covers.
pub fn summary() -> Vec<String> {
    vec![
        "ATLAS is free software under the Apache License 2.0: this program's own code."
            .to_string(),
        "The model file it downloads, and the copy of llama.cpp it builds, keep their own \
         terms. Nothing here re-licenses anybody else's work."
            .to_string(),
        "There is no warranty. A model can be wrong, and the app says so before it shows a \
         verdict."
            .to_string(),
        format!(
            "The full text is in LICENSE beside this file; the app shows it in full the first \
             time it opens. Document version: {VERSION}."
        ),
    ]
}

/// Is this answer a yes? Only "yes" and "y" (any case) are; everything else — including
/// silence — is a no, because a download must never be the default.
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "yes" | "y")
}

/// The sentence a person gets when they have not accepted yet.
///
/// It names the command, the file and what will happen, so "nothing was downloaded" is
/// never a dead end.
pub fn not_accepted_line() -> String {
    format!(
        "the licence has not been accepted yet, so nothing was downloaded. Run \
         `atlas-setup --install` and answer the question, or pass `--accept-license` to accept \
         {VERSION} when you start it from a script (the record goes to {RELATIVE_PATH})"
    )
}

/// The line that asks the question, after [`summary`] and the "what this will download"
/// line the caller adds.
pub fn question() -> String {
    "Type yes and press return to accept, or anything else to stop without downloading."
        .to_string()
}

/// Judge an answer, and record it if it is a yes.
///
/// `answer` is the one line the person typed; `None` means the stream ended (a script, a
/// closed terminal), which is a refusal with the sentence above — never a hang, never a
/// panic, and never an acceptance nobody gave. If the machine already answered for this
/// document version, the record is returned and the answer is not looked at.
pub fn decide(
    root: &Path,
    by: &str,
    answer: Option<&str>,
    say: &mut dyn FnMut(&str),
) -> Result<Record, String> {
    if let State::Accepted(record) = state(root) {
        return Ok(record);
    }
    let Some(answer) = answer else {
        return Err(not_accepted_line());
    };
    if !is_yes(answer) {
        return Err(not_accepted_line());
    }
    let record = accept(root, by)?;
    say(&format!(
        "licence: accepted ({}) and recorded in {RELATIVE_PATH}",
        record.version
    ));
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-license-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch root");
        dir
    }

    /// The record is the contract: a version, a moment, and who recorded it — and it is
    /// replaced, never edited in place, so a reader never sees half of it.
    #[test]
    fn the_record_names_the_version_the_moment_and_the_caller() {
        let root = scratch("record");
        let record = accept(&root, "atlas-setup --accept-license").expect("a record");
        assert_eq!(record.version, VERSION);
        assert!(record.accepted_at > 0, "{record:?}");

        let text = fs::read_to_string(path(&root)).expect("the file");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(value["version"], VERSION);
        assert_eq!(value["by"], "atlas-setup --accept-license");
        assert!(value["accepted_at"].as_u64().unwrap_or(0) > 0, "{text}");
        assert!(!staged(&path(&root)).exists(), "no staged file is left behind: {text}");
        let _ = fs::remove_dir_all(&root);
    }

    /// A version that moved on is not an acceptance of the new document. This is the whole
    /// reason the version is in the file.
    #[test]
    fn an_older_acceptance_does_not_answer_for_a_newer_document() {
        let root = scratch("version");
        fs::create_dir_all(root.join("data")).expect("data");
        fs::write(
            path(&root),
            "{\"version\":\"apache-2.0+notice-0\",\"accepted_at\":1,\"by\":\"old\"}\n",
        )
        .expect("an old record");
        assert_eq!(state(&root), State::Missing);

        accept(&root, "atlas-setup").expect("a fresh record");
        assert!(matches!(state(&root), State::Accepted(_)));
        let _ = fs::remove_dir_all(&root);
    }

    /// A broken or unreadable record is "not answered", not a crash: the question can
    /// always be asked again.
    #[test]
    fn an_unreadable_record_is_not_an_acceptance() {
        let root = scratch("broken");
        fs::create_dir_all(root.join("data")).expect("data");
        for text in ["", "not json", "[]", "{\"version\":3}"] {
            fs::write(path(&root), text).expect("a broken record");
            assert_eq!(state(&root), State::Missing, "{text:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// Silence and "no" both refuse, with the sentence that says what to run instead — and
    /// nothing is written.
    #[test]
    fn silence_and_no_refuse_without_recording_anything() {
        for answer in [None, Some("no"), Some(""), Some("n"), Some("later")] {
            let root = scratch("refuse");
            let mut lines: Vec<String> = Vec::new();
            let error = decide(&root, "atlas-setup --install", answer, &mut |line| lines.push(line.to_string()))
                .expect_err("a refusal");
            assert!(error.contains("not been accepted"), "{error}");
            assert!(error.contains("atlas-setup --install"), "{error}");
            assert!(error.contains(RELATIVE_PATH), "{error}");
            assert!(!path(&root).exists(), "nothing was recorded for {answer:?}");
            assert!(lines.is_empty(), "a refusal records nothing, not even a line: {lines:?}");
            let _ = fs::remove_dir_all(&root);
        }
    }

    /// "yes" accepts, records, and a second run does not ask again.
    #[test]
    fn yes_accepts_once_and_the_next_run_is_silent() {
        let root = scratch("accept");
        let mut first: Vec<String> = Vec::new();
        let record = decide(&root, "atlas-setup --install", Some("Yes"), &mut |line| first.push(line.to_string()))
            .expect("an acceptance");
        assert_eq!(record.version, VERSION);
        assert!(first.iter().any(|line| line.contains("accepted")), "{first:?}");

        let mut second: Vec<String> = Vec::new();
        let again = decide(&root, "atlas-setup --install", None, &mut |line| second.push(line.to_string()))
            .expect("the record already answers");
        assert_eq!(again.version, VERSION);
        assert!(second.is_empty(), "a second run asks nothing: {second:?}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The summary is what a first run reads: the three points the window's screen makes,
    /// in the same order, plus where the whole text is.
    #[test]
    fn the_summary_makes_the_three_points_plainly() {
        let lines = summary().join(" ");
        assert!(lines.contains("Apache License 2.0"), "{lines}");
        assert!(lines.contains("keep their own terms"), "{lines}");
        assert!(lines.contains("no warranty"), "{lines}");
        assert!(lines.contains("LICENSE"), "{lines}");
        assert!(lines.contains(VERSION), "{lines}");
        for jargon in ["stdin", "argv", "http", "json", "sha256", "binary"] {
            assert!(!lines.to_lowercase().contains(jargon), "{jargon:?} in {lines}");
        }
    }

    /// The anti-drift pin: this crate's version is the window's version. Two consent
    /// strings that disagree means a person accepted one document and was shown another.
    ///
    /// Two outcomes, and they are different on purpose: a screen that is *not in this
    /// checkout* is "nothing to compare", while a screen whose constant cannot be read is a
    /// broken pin and fails here. Neither outcome prints — [`window_version`] returns the
    /// reason instead.
    #[test]
    fn the_version_is_the_window_s_own_version() {
        match window_version() {
            Ok(found) => assert_eq!(
                found, VERSION,
                "the window's licence version and this crate's must agree"
            ),
            Err(reason) => assert!(
                reason.contains("not in this checkout"),
                "the screen is here but its version cannot be read: {reason}"
            ),
        }
    }

    /// A quote in a field cannot break the file, and a control character cannot make it
    /// invalid JSON.
    #[test]
    fn the_record_stays_valid_json_for_awkward_text() {
        let root = scratch("quoting");
        accept(&root, "a \"quoted\" \\ name\twith a tab").expect("a record");
        let text = fs::read_to_string(path(&root)).expect("the file");
        let value: serde_json::Value = serde_json::from_str(&text).expect("still valid JSON");
        assert_eq!(value["by"], "a \"quoted\" \\ name\twith a tab");
        let _ = fs::remove_dir_all(&root);
    }
}
