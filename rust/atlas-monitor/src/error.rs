//! Every failure this crate reports, and the one place a message is made safe.
//!
//! A message is scrubbed through `atlas-store`'s `scrub` (which asks
//! `atlas-guard`) and capped: a SQLite error can quote a bound value, and a
//! monitor's query is user text. It never carries an article's text, a summary or
//! a prompt.
//!
//! It does not decide whether an operation fails — that is the statement's job —
//! and it never turns a failure into a value.

use std::fmt;

/// Longest message handed back to a caller, after scrubbing.
pub const MAX_ERROR_CHARS: usize = 300;

/// A failure with a reason a caller may log and show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Error {
        let text = scrub(&message.into());
        Error {
            message: if text.chars().count() > MAX_ERROR_CHARS {
                text.chars().take(MAX_ERROR_CHARS).collect()
            } else {
                text
            },
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<atlas_store::StoreError> for Error {
    fn from(error: atlas_store::StoreError) -> Error {
        Error::new(error.message())
    }
}

impl From<atlas_ingest::PipelineError> for Error {
    fn from(error: atlas_ingest::PipelineError) -> Error {
        Error::new(error.message())
    }
}

/// A reason a log row may hold: credential-shaped material redacted (that rule is
/// `atlas-store`'s, which asks `atlas-guard`), control characters folded to spaces,
/// and one line of at most [`MAX_ERROR_CHARS`].
pub fn scrub(text: &str) -> String {
    crate::verdict::clean_field(&atlas_store::scrub(text), MAX_ERROR_CHARS)
}

/// The result type every function in this crate returns.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_one_scrubbed_line() {
        let error = Error::new("boom\nwith\u{7}a break");
        assert!(!error.message().contains('\n'));
        assert!(!error.message().contains('\u{7}'));
    }

    #[test]
    fn a_long_message_is_capped_not_dropped() {
        let error = Error::new("x".repeat(2000));
        assert_eq!(error.message().chars().count(), MAX_ERROR_CHARS);
    }
}
