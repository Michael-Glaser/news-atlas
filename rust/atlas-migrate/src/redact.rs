//! What a cell may say in a report.
//!
//! Decisions: which columns are body text (and are therefore reported as a length,
//! never as content), how a URL is shortened so a tracking token does not travel
//! into a terminal or a log file, and how a value that came from a page or from the
//! model is capped and stripped of control characters.
//!
//! It does not: escape for JSON ([`atlas_store::value_to_json`] does), decide what
//! a finding is, or print anything.

use atlas_store::SqlValue;

/// Columns whose value is article text. Reported as a length, never as content —
/// `docs/SECURITY-RUST.md`'s "no article text in a log" applies to a report a user
/// redirects into a file just as much.
const BODY_COLUMNS: &[&str] = &[
    "content",
    "summary",
    "text",
    "body",
    "prompt",
    "analysis",
    "description",
];

/// Columns whose value came from a page or from the model: shown, but capped and
/// with control characters removed.
const CAPPED_COLUMNS: &[&str] = &[
    "title",
    "name",
    "tag",
    "matched",
    "why",
    "query",
    "keywords",
    "exclude",
    "publisher",
    "place",
    "author",
];

/// How much of a capped column is kept.
pub const CAP_CHARS: usize = 72;

/// How much of a URL is kept (scheme + host + a short path).
const URL_CHARS: usize = 64;

/// One value, ready for the report.
pub fn cell(column: &str, value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "NULL".to_string(),
        SqlValue::Integer(number) => number.to_string(),
        SqlValue::Real(number) => {
            if number.is_finite() {
                format!("{}", number)
            } else {
                "NULL".to_string()
            }
        }
        SqlValue::Blob(bytes) => format!("<{} bytes>", bytes.len()),
        SqlValue::Text(text) => {
            if BODY_COLUMNS.contains(&column) {
                let count = text.chars().count();
                if count == 0 {
                    "''".to_string()
                } else {
                    format!("<{} chars>", count)
                }
            } else if column == "url" || column == "rss" || column == "fetched_url" {
                url(text)
            } else if CAPPED_COLUMNS.contains(&column) || column == "v" {
                cap(text, CAP_CHARS)
            } else {
                cap(text, 120)
            }
        }
    }
}

/// `scheme://host/path` with the path cut short: the query string never travels.
pub fn url(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "''".to_string();
    }
    let (scheme, rest) = match trimmed.find("://") {
        Some(at) => (&trimmed[..at], &trimmed[at + 3..]),
        None => ("", trimmed),
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    let path = match path.find(['?', '#']) {
        Some(at) => &path[..at],
        None => path,
    };
    let joined = if scheme.is_empty() {
        format!("{}{}", authority, path)
    } else {
        format!("{}://{}{}", scheme, authority, path)
    };
    cap(&joined, URL_CHARS)
}

/// One line: control characters out, runs of space collapsed, capped.
pub fn cap(text: &str, limit: usize) -> String {
    let mut out = String::with_capacity(text.len().min(limit * 2));
    let mut last_space = false;
    for ch in text.chars() {
        if out.chars().count() >= limit {
            out.push('…');
            break;
        }
        if ch.is_control() {
            last_space = false;
            continue;
        }
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        last_space = false;
        out.push(ch);
    }
    out.trim_end().to_string()
}

/// A failure reason: one line, capped, no control characters.
pub fn reason(text: &str) -> String {
    let cleaned = cap(text, 200);
    if cleaned.is_empty() {
        "no reason given".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_text_is_a_length_and_never_the_text() {
        let body = cell("content", &SqlValue::Text("a secret body".to_string()));
        assert_eq!(body, "<13 chars>");
        assert_eq!(cell("summary", &SqlValue::Text(String::new())), "''");
        // A column that is not in the body list is shown (capped).
        assert_eq!(cell("publisher", &SqlValue::Text("Reuters".to_string())), "Reuters");
    }

    #[test]
    fn a_url_loses_its_query_string() {
        assert_eq!(
            url("https://example.com/a/b?token=deadbeef#top"),
            "https://example.com/a/b"
        );
        assert_eq!(url("https://example.com"), "https://example.com");
        assert_eq!(url(""), "''");
    }

    #[test]
    fn control_characters_and_newlines_never_reach_the_report() {
        let value = SqlValue::Text("line\none\u{7}  two".to_string());
        assert_eq!(cell("title", &value), "lineone two");
        assert_eq!(cap("a\n\n  b", 10), "a b");
    }

    #[test]
    fn capping_marks_where_it_stopped() {
        let long = "x".repeat(200);
        let capped = cap(&long, 5);
        assert_eq!(capped.chars().count(), 6);
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn numbers_read_as_numbers() {
        assert_eq!(cell("id", &SqlValue::Integer(7)), "7");
        assert_eq!(cell("conf", &SqlValue::Real(0.84)), "0.84");
        assert_eq!(cell("x", &SqlValue::Null), "NULL");
        assert_eq!(cell("x", &SqlValue::Blob(vec![1, 2, 3])), "<3 bytes>");
    }
}
