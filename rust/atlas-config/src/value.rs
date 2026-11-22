//! A JSON value as the text a setting carries.
//!
//! One decision: how the JSON that came from `data/config.json` or the
//! environment becomes the string a [`crate::Spec`] can be coerced from — `null`
//! is empty, a number keeps its digits, a structure renders as compact JSON
//! (Python's `str()` used `"[1, 2]"`; for a setting the difference is never
//! meaningful and this is the only place the port renders a value differently).
//!
//! It does not: coerce a type, range-check anything, or know which key it is
//! serving. That is [`crate::spec`]'s job.

use serde_json::Value;


/// A JSON value as the text a setting would carry.
///
/// `null` is empty, a number keeps its digits, and a structure is rendered as
/// compact JSON — Python used `str()` there (`"[1, 2]"` vs `"[1,2]"`), which for
/// a setting is never a meaningful difference and is the only place this port
/// renders a value differently.
pub(crate) fn value_as_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(other) => other.to_string(),
    }
}

/// Non-empty trimmed string, or `default`.
pub(crate) fn text_or(raw: Option<&Value>, default: &str) -> String {
    let text = value_as_text(raw);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}
