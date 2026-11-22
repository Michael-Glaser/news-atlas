//! The two JSON fragments every answer is built from.
//!
//! Decides: how a list of strings is written (`, "key": ["a", "b"]`, escaped),
//! and how a `reason` is written — the matched entries joined with ", " and cut
//! at 300 characters, like the Python column.
//!
//! Does not decide: which entries are in the list, or what the answer means. Each
//! subcommand owns the shape of its own reply; only these two pieces are shared,
//! so they exist exactly once.

use atlas_lib::json;

use crate::chars;
use crate::score::REASON_MAX;

/// `,"key":["a","b"]`
pub(crate) fn write_string_list(out: &mut String, key: &str, values: &[String]) {
    out.push_str(",\"");
    out.push_str(key);
    out.push_str("\":[");
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json::escape_str(out, value);
    }
    out.push(']');
}

/// `", ".join(matched)[:300]`, escaped.
pub(crate) fn write_reason(out: &mut String, matched: &[String]) {
    let mut joined = String::new();
    for (i, part) in matched.iter().enumerate() {
        if i > 0 {
            joined.push_str(", ");
        }
        joined.push_str(part);
    }
    json::escape_str(out, chars::take_chars(&joined, REASON_MAX));
}
