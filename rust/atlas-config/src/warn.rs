//! Hardening notes: collected once, deduplicated, capped, always redacted.
//!
//! A note is a sentence about a *setting* ("model path rejected: …"), never
//! page text, a prompt or an article. It still goes through
//! [`atlas_guard::redact`] before it is stored or printed: a hand-edited
//! `data/config.json` can put a credential in a path, and a log line is the last
//! place it may surface.

use std::io::Write;
use std::sync::{LazyLock, Mutex};

/// Upper bound on recorded notes, so a hostile config cannot grow the list.
pub const WARNING_CAP: usize = 40;

/// Longest single note; a path is not a page and must not be a paragraph.
pub const WARNING_MAX_CHARS: usize = 400;

static WARNINGS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Record one hardening note (deduplicated) and echo it to stderr once.
///
/// Returns the note as it was stored, which is what [`crate::Config::describe`]
/// reports under `security.warnings`.
pub fn warn(message: &str) -> String {
    let scrubbed = atlas_guard::redact::redact(message).text;
    let text: String = scrubbed.chars().take(WARNING_MAX_CHARS).collect();
    let mut guard = match WARNINGS.lock() {
        Ok(guard) => guard,
        // A poisoned lock must not turn a note into a panic on the boot path.
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.iter().any(|seen| seen == &text) || guard.len() >= WARNING_CAP {
        return text;
    }
    guard.push(text.clone());
    drop(guard);
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "ATLAS security: {}", text);
    let _ = stderr.flush();
    text
}

/// Every hardening note this process collected, oldest first.
pub fn warnings() -> Vec<String> {
    match WARNINGS.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;


#[test]
fn a_warning_is_redacted_and_capped() {
    let note = warn("handshake failed for https://example.test/page?token=abc123secret");
    assert!(note.contains("[redacted]"), "{}", note);
    assert!(!note.contains("abc123secret"));
    assert_eq!(
        warn(&"x".repeat(1000)).chars().count(),
        WARNING_MAX_CHARS
    );
    assert_eq!(WARNING_CAP, 40);
    // The same note twice is one note, and the answer is stable either way.
    let first = warn("atlas-config test note 4711");
    let second = warn("atlas-config test note 4711");
    assert_eq!(first, second);
}
}
