//! The freshness window: is this timestamp current, stale, or from the future?
//!
//! Decides: the 72-hour default, the 300-second tolerance for a clock that runs
//! a little fast ([`DEFAULT_SKEW_S`] is also the display rule's only tolerance),
//! and the three-way verdict with its note — `fresh`, `stale`, `skew`.
//!
//! An unparseable timestamp is answered as `{"fresh": false, "reason":
//! "unparsed"}` with the reader's reason attached, never as "current": a garbage
//! feed date is not breaking news. Its `age_s`/`age_h` are `null` rather than
//! `0`, because a stamp that could not be read has no age — `0` reads as "just
//! published" to anything that forgets to look at `fresh` first.
//!
//! A timestamp more than the skew ahead of `now` is `skew`, not `fresh`: a
//! negative age is smaller than any window, so the arithmetic alone would let an
//! article dated 2027 through every gate there is.
//!
//! Does not decide: how a timestamp is read (`crate::parse`), or how an age is
//! displayed (`crate::format`). The window is the only gate; everything else
//! about freshness lives here.

use atlas_lib::json::{self, Json};

use crate::args::{num_i64, now_arg, text_arg};
use crate::iso::iso_utc;
use crate::parse::parse_stamp;

/// 72 h of current reporting: the default `max_age_s`.
pub const DEFAULT_MAX_AGE_S: i64 = 72 * 3600;
/// A clock a few minutes ahead is normal; more is not. The display rule
/// (`crate::format`) uses the same number, so the label and the gate agree about
/// what "now" means.
pub const DEFAULT_SKEW_S: i64 = 300;

pub fn cmd_window(input: &Json) -> Result<String, String> {
    let ts = text_arg(input, "ts")?;
    let now = now_arg(input)?;
    let max_age = match input.get("max_age_s") {
        Some(Json::Null) | None => DEFAULT_MAX_AGE_S,
        Some(v) => num_i64(v, "max_age_s")?,
    };
    if max_age < 0 {
        return Err("\"max_age_s\" must be >= 0 (seconds of look-back, e.g. 259200)".to_string());
    }
    let skew = match input.get("skew_s") {
        Some(Json::Null) | None => DEFAULT_SKEW_S,
        Some(v) => num_i64(v, "skew_s")?,
    };
    if skew < 0 {
        return Err("\"skew_s\" must be >= 0 (seconds of tolerance for a fast clock)".to_string());
    }

    let mut out = String::with_capacity(192);
    match parse_stamp(&ts, "", now) {
        Err(why) => {
            // A stamp that could not be read has no age. Answering `0` here is
            // what made an unreadable feed date look like breaking news.
            out.push_str(
                "{\"fresh\":false,\"age_s\":null,\"age_h\":null,\"reason\":\"unparsed\",\"note\":",
            );
            json::escape_str(&mut out, &why);
            out.push_str(",\"iso\":\"\",\"now\":");
            out.push_str(&now.to_string());
            push_window_tail(&mut out, max_age, skew);
        }
        Ok(st) => {
            let age = now - st.epoch;
            let (fresh, reason, note) = if age < 0 && -age > skew {
                (
                    false,
                    "skew",
                    format!(
                        "timestamp is {} s in the future of now: a future-dated item is not current reporting",
                        -age
                    ),
                )
            } else if age > max_age {
                (
                    false,
                    "stale",
                    format!("{} s older than the {} s window", age, max_age),
                )
            } else if age < 0 {
                (
                    true,
                    "fresh",
                    format!("{} s ahead, inside the {} s skew", -age, skew),
                )
            } else {
                (true, "fresh", String::new())
            };
            out.push_str("{\"fresh\":");
            out.push_str(if fresh { "true" } else { "false" });
            out.push_str(",\"age_s\":");
            out.push_str(&age.to_string());
            out.push_str(",\"age_h\":");
            out.push_str(&json::fmt_score(age as f64 / 3600.0));
            out.push_str(",\"reason\":");
            json::escape_str(&mut out, reason);
            out.push_str(",\"note\":");
            json::escape_str(&mut out, &note);
            out.push_str(",\"iso\":");
            json::escape_str(&mut out, &iso_utc(st.epoch));
            out.push_str(",\"now\":");
            out.push_str(&now.to_string());
            push_window_tail(&mut out, max_age, skew);
        }
    }
    Ok(out)
}

fn push_window_tail(out: &mut String, max_age: i64, skew: i64) {
    out.push_str(",\"max_age_s\":");
    out.push_str(&max_age.to_string());
    out.push_str(",\"skew_s\":");
    out.push_str(&skew.to_string());
    out.push('}');
}
