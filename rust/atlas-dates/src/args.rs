//! The arguments of a request, and the clock when a caller names no "now".
//!
//! Decides: how `ts` arrives (a JSON string or a JSON number), how an integer
//! argument is coerced (`age_s`, `max_age_s`, `skew_s`, with a message per
//! refusal), and what `now` means — a number in seconds, milliseconds,
//! microseconds or nanoseconds; a parseable timestamp; the word "now"; or, when
//! absent, the system clock.
//!
//! Does not decide: any date semantics. This is the only module that reads the
//! environment, and it reads exactly one thing: the wall clock.

use atlas_lib::json::{self, Json};

use crate::parse::parse_stamp;

/// Unix time now. The only thing this crate reads besides stdin.
pub fn clock() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

pub(crate) fn num_i64(v: &Json, key: &str) -> Result<i64, String> {
    match v {
        Json::Num(n) => {
            if !n.is_finite() {
                return Err(format!("\"{}\" must be a finite number", key));
            }
            Ok(n.trunc() as i64)
        }
        Json::Str(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("\"{}\" must be an integer", key)),
        _ => Err(format!("\"{}\" must be a number", key)),
    }
}

/// `ts` / `now` as text: a JSON number is a unix stamp, a string is parsed.
///
/// A key that is absent *or* `null` is "no date was written here" — an empty
/// value is not a value. The refusal says what to pass, because the reader of
/// the message is usually a log line or the local model, and "missing field"
/// alone does not tell either of them what to do next.
pub(crate) fn text_arg(input: &Json, key: &str) -> Result<String, String> {
    match input.get(key) {
        Some(Json::Str(s)) => Ok(s.clone()),
        Some(Json::Num(n)) => {
            if !n.is_finite() {
                return Err(format!("\"{}\" must be a finite number", key));
            }
            Ok(json::num_str(*n))
        }
        Some(Json::Null) | None => Err(format!(
            "missing \"{}\": pass the timestamp to read, as an ISO-8601 string (\"2026-11-21T09:00:00Z\") or a unix epoch number, or leave the key out and the item counts as undated",
            key
        )),
        Some(_) => Err(format!(
            "\"{}\" must be a string or a number, not a JSON array or object",
            key
        )),
    }
}

pub(crate) fn now_arg(input: &Json) -> Result<i64, String> {
    match input.get("now") {
        None | Some(Json::Null) => Ok(clock()),
        Some(Json::Num(n)) => {
            if !n.is_finite() {
                return Err("\"now\" must be a finite number".to_string());
            }
            let mag = n.abs();
            // ms/µs stamps are accepted too: callers pass what they have.
            let secs = if mag >= 1e14 {
                n / 1e6
            } else if mag >= 1e11 {
                n / 1e3
            } else {
                *n
            };
            Ok(secs.floor() as i64)
        }
        Some(Json::Str(s)) => {
            let t = s.trim();
            if t.is_empty() {
                return Err("\"now\" is empty".to_string());
            }
            if t.eq_ignore_ascii_case("now") {
                return Ok(clock());
            }
            parse_stamp(t, "", clock())
                .map_err(|why| format!("bad \"now\": {}", why))
                .map(|s| s.epoch)
        }
        Some(_) => Err("\"now\" must be a number or a string".to_string()),
    }
}
