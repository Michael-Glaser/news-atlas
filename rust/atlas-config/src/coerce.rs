//! One number setting coerced into the range its SPEC row allows.
//!
//! Decisions: `null`, a boolean and a structure are not numbers (a boolean is
//! not a port), a numeric string is accepted after trimming, a non-finite value
//! is refused, and everything else is clamped — never rejected — into
//! `[low, high]`, rounded half-to-even for an integer row and to three places
//! for a fractional one.
//!
//! It also answers *what the clamp did* ([`Clamp`]), because clamping a number
//! is still changing the user's number: `data/config.json` said `80`, the app
//! runs on `1024`, and a person who is never told cannot explain either. The
//! note that says it is assembled by [`crate::Config::build`] from
//! [`crate::spec::range_text`]; this module only carries the fact.
//!
//! It does not: decide which rows exist or what they mean ([`crate::spec`]),
//! remember where a value came from ([`crate::problem::Origin`]), or format a
//! message.

use serde_json::Value;

use crate::spec::{Kind, Spec};

/// How the SPEC's range moved a value. `None` means it was already usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clamp {
    None,
    Low,
    High,
}

/// A value that is usable, and whether the range had to move it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coerced {
    pub value: f64,
    pub clamp: Clamp,
}

/// `round()` in Python is half-to-even; a port must not drift by one because a
/// value landed exactly on `.5`.
fn round_half_even(value: f64) -> f64 {
    let floor = value.floor();
    let diff = value - floor;
    if diff > 0.5 {
        floor + 1.0
    } else if diff < 0.5 {
        floor
    } else if floor % 2.0 == 0.0 {
        floor
    } else {
        floor + 1.0
    }
}

/// `round(value, places)`. Scaling by 10^places is exact for the values this
/// table accepts (halves and short decimals); a value whose decimal expansion
/// sits exactly on a tie can differ from Python in the last place, which no
/// setting in this table can express.
fn round_to(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    round_half_even(value * scale) / scale
}

fn parse_float(text: &str) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok()
}

/// One numeric setting coerced into its range, or `None` when the value is not a
/// number at all.
///
/// Faithful to `config._number`: `null` and booleans are unusable, a numeric
/// string is accepted after trimming, a non-finite value is refused, and
/// everything else is clamped — never rejected — into `[low, high]`.
pub fn coerce(spec: &Spec, raw: Option<&Value>) -> Option<Coerced> {
    let value = match raw? {
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => return None,
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => parse_float(text.trim())?,
    };
    if !value.is_finite() {
        return None;
    }
    let clamp = if value < spec.low {
        Clamp::Low
    } else if value > spec.high {
        Clamp::High
    } else {
        Clamp::None
    };
    let clamped = value.max(spec.low).min(spec.high);
    Some(Coerced {
        value: match spec.kind {
            Kind::Int => round_half_even(clamped),
            Kind::Float => round_to(clamped, 3),
        },
        clamp,
    })
}

/// [`coerce`], keeping only the value — for a caller that does not report a
/// clamp (the round-trip in [`crate::stored`], a test).
pub fn coerce_number(spec: &Spec, raw: Option<&Value>) -> Option<f64> {
    coerce(spec, raw).map(|coerced| coerced.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::spec_of;

    #[test]
    fn a_value_inside_the_range_is_not_clamped() {
        let port = spec_of("app_port").expect("app_port");
        let inside = coerce(port, Some(&Value::from(8787)));
        assert_eq!(inside, Some(Coerced { value: 8787.0, clamp: Clamp::None }));
        // Exactly on a bound is inside, because the range is inclusive.
        assert_eq!(coerce(port, Some(&Value::from(1024.0))).map(|c| c.clamp), Some(Clamp::None));
        assert_eq!(coerce(port, Some(&Value::from(65535.0))).map(|c| c.clamp), Some(Clamp::None));
    }

    #[test]
    fn a_value_outside_the_range_says_which_side_it_came_from() {
        let port = spec_of("app_port").expect("app_port");
        let low = coerce(port, Some(&Value::from(80))).expect("usable after clamping");
        assert_eq!((low.value, low.clamp), (1024.0, Clamp::Low));
        let high = coerce(port, Some(&Value::from(99999))).expect("usable after clamping");
        assert_eq!((high.value, high.clamp), (65535.0, Clamp::High));
        // The same through a numeric string, which the environment always sends.
        let text = coerce(port, Some(&Value::String(" 80 ".into()))).expect("usable");
        assert_eq!((text.value, text.clamp), (1024.0, Clamp::Low));
    }

    #[test]
    fn a_fractional_row_reports_its_own_bounds() {
        let interval = spec_of("collect_interval_min").expect("interval");
        let low = coerce(interval, Some(&Value::from(0.0))).expect("usable");
        assert_eq!((low.value, low.clamp), (0.5, Clamp::Low));
        let high = coerce(interval, Some(&Value::from(100000.0))).expect("usable");
        assert_eq!((high.value, high.clamp), (1440.0, Clamp::High));
    }
}
