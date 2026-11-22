//! `token` — is this tag / place / topic / search value usable as a value?
//!
//! Decides the per-kind length cap, the character allowlist (so a search term
//! cannot carry SQL wildcards or an invisible override), the lowercasing and
//! space collapsing, and the verdict `ok` / `value` / `reason` the caller acts
//! on. A rejected value is a verdict, not an error: only an unknown kind is bad
//! input.
//!
//! It does not decide where a value came from, whether it is true, or what is
//! already stored under it — `atlas-store` asks, and trusts this answer.

use crate::json::{self, jbool, jobj, jstr, Json};
use crate::field;

pub struct Checked {
    pub ok: bool,
    pub value: String,
    pub reason: String,
}

fn max_len(kind: &str) -> Option<usize> {
    match kind {
        "tag" => Some(40),
        "place" => Some(80),
        "topic" => Some(80),
        "search" => Some(200),
        _ => None,
    }
}

fn allowed(ch: char) -> bool {
    ch.is_alphanumeric() || ch == ' ' || ch == '-' || ch == '\'' || ch == '&' || ch == '.'
}

/// Normalise a value for one kind and say whether it may be used. `Err` is
/// reserved for a caller mistake (unknown kind); a rejected value is a
/// verdict with `ok:false` and a reason.
pub fn check(value: &str, kind: &str) -> Result<Checked, String> {
    let max = match max_len(kind) {
        Some(m) => m,
        None => {
            return Err(format!(
                "unknown kind \"{}\" (expected tag | place | topic | search)",
                kind
            ))
        }
    };

    let mut lowered = String::with_capacity(value.len());
    let mut control = false;
    let mut invisible = false;
    let mut wildcard = false;
    for ch in value.chars() {
        if crate::cleantext::is_invisible(ch) {
            invisible = true;
            continue;
        }
        if ch.is_control() {
            control = true;
            continue;
        }
        if ch == '%' || ch == '_' {
            wildcard = true;
            continue;
        }
        if ch.is_whitespace() {
            lowered.push(' ');
            continue;
        }
        for lower in ch.to_lowercase() {
            lowered.push(lower);
        }
    }

    // Collapse spaces and trim the ends, so the checks below see the value
    // that would actually be stored.
    let mut norm = String::with_capacity(lowered.len());
    let mut pending = false;
    for ch in lowered.chars() {
        if ch == ' ' {
            if !norm.is_empty() {
                pending = true;
            }
        } else {
            if pending {
                norm.push(' ');
                pending = false;
            }
            norm.push(ch);
        }
    }

    let mut ok = false;
    let mut reason = String::new();
    if control {
        reason = "contains control characters".to_string();
    } else if invisible {
        reason = "contains invisible (zero-width, bidi or hidden tag) characters".to_string();
    } else if wildcard {
        reason = "contains SQL wildcard characters (% or _)".to_string();
    } else if norm.is_empty() {
        reason = "empty value".to_string();
    } else if norm.chars().count() > max {
        reason = format!("longer than {} characters", max);
    } else if !norm.chars().all(allowed) {
        reason = "contains a character that is not allowed (letters, digits, space, - ' & . only)"
            .to_string();
    } else {
        let starts_ok = match norm.chars().next() {
            Some(ch) => ch.is_alphanumeric(),
            None => false,
        };
        let ends_ok = match norm.chars().last() {
            Some(ch) => ch.is_alphanumeric(),
            None => false,
        };
        if !starts_ok {
            reason = "starts with punctuation".to_string();
        } else if !ends_ok {
            reason = "ends with punctuation".to_string();
        } else {
            ok = true;
        }
    }

    Ok(Checked {
        ok,
        value: norm,
        reason,
    })
}

pub fn run(root: &Json) -> Result<String, String> {
    // `value` is the documented field. `text` is accepted too: the caller that
    // routes a request through this subcommand names the payload field of every
    // operation "text", and a silent field mismatch would leave that path
    // switched off without a word.
    let value = match field::get(root, "value").or_else(|| field::get(root, "text")) {
        Some(v) => v,
        None => return Err("missing \"value\" string".to_string()),
    };
    let kind = field::required(root, "kind")?;
    let checked = check(value, kind)?;
    Ok(json::to_string(&jobj(vec![
        ("ok", jbool(checked.ok)),
        ("value", jstr(&checked.value)),
        ("reason", jstr(&checked.reason)),
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::json_of;

    #[test]
    fn token_lowercases_and_collapses_spaces() {
        let c = check("  NVIDIA   Corp ", "tag").expect("known kind");
        assert!(c.ok, "{}", c.reason);
        assert_eq!(c.value, "nvidia corp");
    }

    #[test]
    fn token_length_caps_per_kind() {
        assert!(check(&"a".repeat(40), "tag").expect("kind").ok);
        let too_long = check(&"a".repeat(41), "tag").expect("kind");
        assert!(!too_long.ok);
        assert!(too_long.reason.contains("40"), "{}", too_long.reason);
        assert!(check(&"a".repeat(80), "place").expect("kind").ok);
        assert!(!check(&"a".repeat(81), "place").expect("kind").ok);
        assert!(check(&"a".repeat(80), "topic").expect("kind").ok);
        assert!(!check(&"a".repeat(81), "topic").expect("kind").ok);
        assert!(check(&"a".repeat(200), "search").expect("kind").ok);
        assert!(!check(&"a".repeat(201), "search").expect("kind").ok);
    }

    #[test]
    fn token_rejects_sql_wildcards() {
        for (value, kind) in [("50% off", "search"), ("a_b", "tag"), ("%", "search")] {
            let c = check(value, kind).expect("kind");
            assert!(!c.ok, "{} should be refused", value);
            assert!(
                c.reason.contains("wildcard") || c.reason.contains("empty"),
                "{} gave {:?}",
                value,
                c.reason
            );
        }
    }

    #[test]
    fn token_rejects_control_and_invisible_characters() {
        let control = check("a\u{0}b", "tag").expect("kind");
        assert!(!control.ok);
        assert!(control.reason.contains("control"), "{}", control.reason);
        let invisible = check("a\u{200b}b", "tag").expect("kind");
        assert!(!invisible.ok);
        assert!(invisible.reason.contains("invisible"), "{}", invisible.reason);
        // A tag character is invisible ASCII: the value reaches a database and a
        // model prompt, so it is invisible here too, and the value that would be
        // stored no longer carries it.
        let smuggled = check("nvidia\u{e0041}corp", "tag").expect("kind");
        assert!(!smuggled.ok, "a tag character must not pass as a tag");
        assert!(
            smuggled.reason.contains("invisible"),
            "{}",
            smuggled.reason
        );
        assert_eq!(smuggled.value, "nvidiacorp");
    }

    #[test]
    fn token_rejects_leading_and_trailing_punctuation() {
        for (value, needle) in [
            ("-nvidia", "starts"),
            (".tag", "starts"),
            ("nvidia-", "ends"),
            ("tag.", "ends"),
            ("'quoted'", "starts"),
        ] {
            let c = check(value, "tag").expect("kind");
            assert!(!c.ok, "{} should be refused", value);
            assert!(
                c.reason.contains(needle),
                "{} gave {:?}",
                value,
                c.reason
            );
        }
    }

    #[test]
    fn token_allows_ampersand_apostrophe_dot_and_hyphen() {
        for value in ["at&t's inc", "sri-lanka", "u.s. markets", "st. mary's"] {
            let c = check(value, "place").expect("kind");
            assert!(c.ok, "{} should be allowed: {:?}", value, c.reason);
            assert_eq!(c.value, value);
        }
    }

    #[test]
    fn token_rejects_characters_outside_the_allowlist() {
        for value in ["nvidia:chips", "ai (chips)", "a/b", "x;drop table"] {
            let c = check(value, "tag").expect("kind");
            assert!(!c.ok, "{} should be refused", value);
            assert!(
                c.reason.contains("not allowed") || c.reason.contains("control"),
                "{} gave {:?}",
                value,
                c.reason
            );
        }
    }

    #[test]
    fn token_rejects_an_empty_value() {
        let c = check("   ", "tag").expect("kind");
        assert!(!c.ok);
        assert_eq!(c.reason, "empty value");
        assert_eq!(c.value, "");
    }

    #[test]
    fn token_returns_the_normalised_value_even_when_rejected() {
        let c = check("NVIDIA!!!", "tag").expect("kind");
        assert!(!c.ok);
        assert_eq!(c.value, "nvidia!!!");
        let again = check("NVIDIA!!!", "tag").expect("kind");
        assert!(again.reason.contains("not allowed"), "{}", again.reason);
    }

    #[test]
    fn token_unknown_kind_is_bad_input() {
        assert!(check("x", "slug").is_err());
        assert!(run(&json_of("{\"value\":\"x\",\"kind\":\"slug\"}")).is_err());
        assert!(run(&json_of("{\"value\":\"x\"}")).is_err(), "missing kind");
    }

    #[test]
    fn token_run_returns_ok_value_and_reason() {
        let out = run(&json_of("{\"value\":\" Nvidia \",\"kind\":\"tag\"}")).expect("ok");
        assert_eq!(out, "{\"ok\":true,\"value\":\"nvidia\",\"reason\":\"\"}");
        // The request adapter sends the field as "text"; both must work.
        let as_text = run(&json_of("{\"text\":\" Nvidia \",\"kind\":\"tag\"}")).expect("ok");
        assert_eq!(as_text, out);
        assert!(run(&json_of("{\"kind\":\"tag\"}")).is_err(), "no value at all");
    }
}
