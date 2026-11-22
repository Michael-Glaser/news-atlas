//! The display age: the one relative label the pipeline produces.
//!
//! Decides: `now`, `5 min`, `4 h`, `2 d`, and — for a stamp dated *ahead* of the
//! clock — `in 4 h`. The label is bare, because the template supplies the "·"
//! separators. `format` also accepts a timestamp plus "now" instead of an age,
//! and answers the age it computed.
//!
//! This is the second place where a future-dated item can be presented as current
//! news, so it borrows the window's 300-second skew ([`DEFAULT_SKEW_S`]): inside
//! that, a stamp is "now" (a clock that runs a little fast is normal); beyond it,
//! the label says the item is early (`in 4 h`) instead of claiming it just
//! happened. The old rule floored every age at "now", so an article dated 2027 and
//! one published a minute ago printed the same two characters.
//!
//! Does not decide: whether anything is fresh — that is [`crate::window`] and only
//! [`crate::window`]. Display is not a gate. The wasm UI writes its own label from
//! the stored string (`atlas-ui/src/fmt.rs`, same thresholds, same numbers), and
//! that copy is the one thing this rule has to stay consistent with.

use atlas_lib::json::{self, Json};

use crate::args::{now_arg, num_i64, text_arg};
use crate::parse::parse_stamp;
use crate::window::DEFAULT_SKEW_S;

/// The magnitude rule: `now`, `5 min`, `4 h`, `2 d`. The sign is
/// [`rel_text`]'s business — an age is a distance here, so `-7200` and `7200`
/// both answer `("h", 2)`. `saturating_abs` because `i64::MIN` has no positive
/// counterpart.
pub fn rel_unit(age_s: i64) -> (&'static str, i64) {
    let age = age_s.saturating_abs();
    if age < 60 {
        ("now", 0)
    } else if age < 3600 {
        ("min", age / 60)
    } else if age < 86_400 {
        ("h", age / 3600)
    } else {
        ("d", age / 86_400)
    }
}

/// The label, with its direction: `now`, `5 min`, `4 h`, `2 d`, `in 4 h`.
pub fn rel_text(age_s: i64) -> String {
    let (unit, n) = rel_unit(age_s);
    let body = match unit {
        "now" => "now".to_string(),
        "min" => format!("{} min", n),
        "h" => format!("{} h", n),
        _ => format!("{} d", n),
    };
    if is_ahead(age_s) {
        format!("in {}", body)
    } else {
        body
    }
}

/// Is this stamp ahead of the clock by more than the skew the freshness window
/// allows? `age_s` is `now - ts`, so only a negative age can be.
fn is_ahead(age_s: i64) -> bool {
    age_s < -DEFAULT_SKEW_S
}

pub fn cmd_format(input: &Json) -> Result<String, String> {
    let age = match input.get("age_s") {
        Some(Json::Null) | None => {
            let ts = text_arg(input, "ts")?;
            let now = now_arg(input)?;
            let st = parse_stamp(&ts, "", now).map_err(|why| format!("bad \"ts\": {}", why))?;
            now - st.epoch
        }
        Some(v) => num_i64(v, "age_s")?,
    };
    let (unit, n) = rel_unit(age);
    let mut out = String::with_capacity(72);
    out.push_str("{\"text\":");
    json::escape_str(&mut out, &rel_text(age));
    out.push_str(",\"unit\":");
    json::escape_str(&mut out, unit);
    out.push_str(",\"n\":");
    out.push_str(&n.to_string());
    out.push_str(",\"age_s\":");
    out.push_str(&age.to_string());
    out.push_str(",\"future\":");
    out.push_str(if is_ahead(age) { "true" } else { "false" });
    out.push('}');
    Ok(out)
}
