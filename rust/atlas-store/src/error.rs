//! Every failure this crate reports, and the only place an error message is made
//! safe to log.
//!
//! The decision here is what a caller may be told: the message is scrubbed
//! through `atlas-guard` (a SQLite error can quote a bound value) and capped, so
//! no credential and no article text can reach a log through an error.
//!
//! It does not: decide whether an operation fails, retry it, or carry a SQL
//! statement. Callers build these; the store reports them.

use std::fmt;

/// Longest error message handed back to a caller, after scrubbing.
pub(crate) const MAX_ERROR_CHARS: usize = 300;

/// A failure with a reason a caller may log: no article text, no page content,
/// no credential. The message is scrubbed on the way in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    message: String,
}

impl StoreError {
    pub fn new(message: impl Into<String>) -> Self {
        StoreError {
            message: scrub(&message.into()),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        StoreError::new(err.to_string())
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(err: serde_json::Error) -> Self {
        StoreError::new(err.to_string())
    }
}

impl From<rusqlite::types::FromSqlError> for StoreError {
    fn from(err: rusqlite::types::FromSqlError) -> Self {
        StoreError::new(err.to_string())
    }
}

/// The result type every method here returns.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Strip credential-shaped material out of `text` and cap it.
///
/// The vocabulary is `atlas-guard`'s (`redact`), not this crate's: a SQLite
/// error message can quote a bound value, and an error message is the one place
/// a stored secret could otherwise escape into a log.
pub fn scrub(text: &str) -> String {
    let redacted = atlas_guard::redact::redact(text);
    let total = redacted.text.chars().count();
    let mut out: String = redacted.text.chars().take(MAX_ERROR_CHARS).collect();
    if total > MAX_ERROR_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;


#[test]
fn scrub_takes_secrets_out_of_error_messages() {
    assert!(!scrub("failed for sk-live-abcdefghijklmnop").contains("sk-live-abcdefghijklmnop"));
    let long = "x".repeat(1_000);
    assert!(scrub(&long).chars().count() <= MAX_ERROR_CHARS + 1);
}

}
