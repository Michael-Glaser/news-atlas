//! `field` — the request field contract: one place that says what a field is
//! and how big it may be.
//!
//! Decides how a field is read out of the request object (`get`), what a
//! missing or non-string field answers (`required`), the `MAX_FIELD` cap and the
//! message for exceeding it (`too_large`), and how a string is cut to a
//! character count without splitting a character (`char_prefix`).
//!
//! It does not know any subcommand and it does not validate content: a value
//! that passes here is merely well-formed. Whether a value is *allowed* is the
//! verdict of the module that asked for it.

/// Hard cap on one input field: 1 MiB of characters. Larger requests are
/// refused (or truncated, where the subcommand says so) instead of being
/// buffered, scanned and copied again in memory.
pub(crate) const MAX_FIELD: usize = 1_048_576;

pub(crate) fn get<'a>(root: &'a crate::json::Json, key: &str) -> Option<&'a str> {
    root.get(key).and_then(crate::json::Json::as_str)
}

/// A required string field. Missing or non-string is bad input, not a verdict.
pub(crate) fn required<'a>(root: &'a crate::json::Json, key: &str) -> Result<&'a str, String> {
    match get(root, key) {
        Some(value) => Ok(value),
        None => Err(format!("missing \"{}\" string", key)),
    }
}

pub(crate) fn too_large(key: &str, chars: usize) -> String {
    format!("\"{}\" is {} characters, over the {} cap", key, chars, MAX_FIELD)
}

/// The first `limit` characters of `text`, cut on a character boundary.
pub(crate) fn char_prefix(text: &str, limit: usize) -> &str {
    let mut seen = 0usize;
    for (i, _ch) in text.char_indices() {
        if seen == limit {
            return match text.get(..i) {
                Some(head) => head,
                None => text,
            };
        }
        seen += 1;
    }
    text
}
