//! One error type for the whole carry-over.
//!
//! Decisions: what a failure may say (a reason, a table name, a count — never a
//! title, a summary, a body or a prompt), and that every layer converts into this
//! type instead of growing its own.
//!
//! It does not: log, retry, or decide an exit code — [`crate::cli`] does.

use std::fmt;

/// A carry-over failure. The message is a reason and stays short enough to print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrateError {
    message: String,
}

impl MigrateError {
    pub fn new(message: impl Into<String>) -> MigrateError {
        let text = message.into();
        MigrateError {
            message: crate::redact::reason(&text),
        }
    }

    /// The reason, already scrubbed of control characters.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MigrateError {}

impl From<rusqlite::Error> for MigrateError {
    fn from(error: rusqlite::Error) -> MigrateError {
        // `Display` of a rusqlite error names the statement's first words when it
        // has them; a statement here is schema, never a value.
        MigrateError::new(format!("sqlite: {}", error))
    }
}

impl From<atlas_store::StoreError> for MigrateError {
    fn from(error: atlas_store::StoreError) -> MigrateError {
        MigrateError::new(error.message())
    }
}

impl From<serde_json::Error> for MigrateError {
    fn from(_: serde_json::Error) -> MigrateError {
        MigrateError::new("a stored value is not valid JSON")
    }
}

/// The crate's result.
pub type Result<T> = std::result::Result<T, MigrateError>;
