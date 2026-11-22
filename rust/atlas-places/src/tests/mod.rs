//! The crate's tests: one module per question the answer has to get right.
//!
//! Every case exercises the whole pipeline — `run()` through the scan, the score
//! and the report — because the answer a caller sees is what is asserted. The
//! shared helpers (`locate`, `payload`, `Want`) live here; the cases live next to
//! the behaviour they pin:
//!
//! * [`ambiguous_names`] — Georgia the state, Jordan the country, Washington the
//!   government.
//! * [`evidence`] — the threshold, the zone weights, the outlet as last resort,
//!   the scripts matched as substrings.
//! * [`namesake`] — a city name two countries use: the text decides, or says so.
//! * [`geodata`] — the codes an answer may carry, the vendored map, the outlet
//!   table, the sorted tables every lookup binary-searches.
//! * [`wire`] — the bytes of every answer, and the inputs that must not break them.

use atlas_lib::json::{self, Json};

use crate::data;
use crate::index::index;
use crate::run;
use crate::score::{round2, BODY_CHARS};

#[derive(Default)]
struct Want {
    iso: Option<&'static str>,
    prec: Option<&'static str>,
    place: Option<&'static str>,
    alt_iso: Option<&'static str>,
    conf: Option<f64>,
    max_conf: Option<f64>,
}

fn payload(title: &str, summary: &str, text: &str, url: &str, publisher: &str) -> String {
    let mut out = String::from("{\"title\":");
    json::escape_str(&mut out, title);
    out.push_str(",\"summary\":");
    json::escape_str(&mut out, summary);
    out.push_str(",\"text\":");
    json::escape_str(&mut out, text);
    out.push_str(",\"url\":");
    json::escape_str(&mut out, url);
    out.push_str(",\"publisher\":");
    json::escape_str(&mut out, publisher);
    out.push('}');
    out
}

fn call(cmd: &str, title: &str, summary: &str, text: &str, url: &str, publisher: &str) -> Json {
    let raw = run(cmd, &payload(title, summary, text, url, publisher))
        .unwrap_or_else(|e| panic!("{} failed: {}", cmd, e));
    json::parse(&raw).unwrap_or_else(|e| panic!("{} produced invalid JSON: {} ({})", cmd, e, raw))
}

fn locate(title: &str, summary: &str, text: &str, url: &str, publisher: &str) -> Json {
    call("locate", title, summary, text, url, publisher)
}

fn text_of<'a>(v: &'a Json, key: &str) -> &'a str {
    v.get(key).and_then(Json::as_str).unwrap_or("")
}

fn num_of(v: &Json, key: &str) -> f64 {
    v.get(key).and_then(Json::as_f64).unwrap_or(f64::NAN)
}

fn alts(v: &Json) -> &Vec<Json> {
    v.get("alternatives").and_then(Json::as_arr).expect("alternatives array")
}

/// The `case()` helper of tools/geo_check.py.
fn check(name: &str, title: &str, summary: &str, text: &str, url: &str, publisher: &str, want: Want) {
    let got = locate(title, summary, text, url, publisher);
    let mut problems: Vec<String> = Vec::new();
    if let Some(iso) = want.iso {
        if text_of(&got, "iso") != iso {
            problems.push(format!("iso={} want {}", text_of(&got, "iso"), iso));
        }
    }
    if let Some(prec) = want.prec {
        if text_of(&got, "prec") != prec {
            problems.push(format!("prec={} want {}", text_of(&got, "prec"), prec));
        }
    }
    if let Some(place) = want.place {
        if !text_of(&got, "place").to_lowercase().contains(&place.to_lowercase()) {
            problems.push(format!("place={} want {}", text_of(&got, "place"), place));
        }
    }
    if let Some(alt_iso) = want.alt_iso {
        let have: Vec<&str> = alts(&got).iter().map(|a| text_of(a, "iso")).collect();
        if !have.contains(&alt_iso) {
            problems.push(format!("alternatives={:?} want {}", have, alt_iso));
        }
    }
    if let Some(conf) = want.conf {
        if num_of(&got, "conf") != conf {
            problems.push(format!("conf={} want {}", num_of(&got, "conf"), conf));
        }
    }
    if let Some(max_conf) = want.max_conf {
        if num_of(&got, "conf") > max_conf {
            problems.push(format!("conf={} over {}", num_of(&got, "conf"), max_conf));
        }
    }
    assert!(
        problems.is_empty(),
        "{}: {} — got iso={} place={} prec={} conf={} alternatives={}",
        name,
        problems.join("; "),
        text_of(&got, "iso"),
        text_of(&got, "place"),
        text_of(&got, "prec"),
        num_of(&got, "conf"),
        json::to_string(&Json::Arr(alts(&got).clone())),
    );
}

fn case(name: &str, title: &str, summary: &str, want: Want) {
    check(name, title, summary, "", "", "", want);
}

/// The place an answer names, as `(iso, place, prec)`, for a failure message.
fn answered(v: &Json) -> (String, String, String) {
    (
        text_of(v, "iso").to_string(),
        text_of(v, "place").to_string(),
        text_of(v, "prec").to_string(),
    )
}

mod ambiguous_names;
mod evidence;
mod geodata;
mod namesake;
mod wire;
