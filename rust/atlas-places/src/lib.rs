//! atlas-places — where did a story actually happen?
//!
//! A faithful, std-only port of `server/places.py`. Title, summary and body are
//! scanned against a curated gazetteer of cities, regions and countries; a hit is
//! weighted by the zone it appears in, names that are also people or plain words
//! need supporting context, a city always beats its own country, and a city name
//! two countries share is decided by the rest of the text or by nothing at all.
//! When the text says nothing, the outlet's home country is the last resort
//! (`prec` is then `"outlet"`, never the primary answer).
//!
//! Wire format (see docs/RUST-CRATE-CONTRACT.md):
//!
//! ```text
//! atlas-places locate|explain|alternatives|outlet   <<< '{"title": …}'
//! ```
//!
//! * `locate`       — the dict `places.locate()` returns, same keys in the same
//!                    order: iso, lat, lon, place, prec, conf, alternatives.
//!                    `iso` is empty when the place it names has no ISO code
//!                    (Somaliland, Northern Cyprus): the name and the coordinates
//!                    are real, the code would be invented.
//! * `explain`      — `places.explain()`: the chosen result, the full scoring
//!                    table (each candidate with its per-zone mention counts and
//!                    its fate) and how much text was looked at.
//! * `alternatives` — the runner-ups of `explain()`: the same up-to-three
//!                    `alternatives` payload `locate` returns, plus every
//!                    non-chosen row of the scoring table.
//! * `outlet`       — `outlet_country(url, publisher)` on its own.
//!
//! Nothing here touches the filesystem, the network, the clock or the
//! environment: stdin in, stdout out, one JSON object each way. The gazetteer is
//! compiled in, the search index is built once per process on first use.
//!
//! ## The modules, and what each one decides
//!
//! * [`data`]   — the compiled-in gazetteer: cities, aliases, outlets, country
//!   records. Tables only, no logic.
//! * [`phrase`] — how a surface form becomes a lookup key ("Washington, D.C."
//!   -> `washington d c`).
//! * [`index`]  — which surface forms exist and which place each one means,
//!   built once per process.
//! * [`scan`]   — what one zone says: word runs, names, guards, cue windows,
//!   localized names, and what a mention is worth before ranking.
//! * [`score`]  — how the zones combine: weights, caps, the threshold, the
//!   city-beats-country promotion, the ordering and the confidence.
//! * [`namesake`] — which of two same-named cities in different countries a text
//!   means, and what to say when it does not say.
//! * [`labels`] — how a country is spelled in an answer.
//! * [`outlet`] — which country an outlet's host or name implies, and that the
//!   most specific name wins.
//! * [`report`] — the exact bytes of every answer.
//!
//! ## What an answer never says
//!
//! * A place the text does not support. Nothing is located below the threshold,
//!   and the outlet's own country is only ever reported with `prec` `"outlet"`,
//!   which every caller treats as the last resort it is.
//! * A country at a precision it has not got. A name two countries share
//!   ("Valencia" in Spain and in Venezuela) is decided by the rest of the text;
//!   when the text does not decide and the outlet cannot either, the answer names
//!   no place and `explain` says why. Table order is never the tie-break.
//! * A code that is not a code. Natural Earth writes `-99` where a place has no
//!   ISO code (Northern Cyprus, Somaliland); those places keep their name and
//!   coordinates and are answered with an empty `iso`.
//!
//! ## Where this cannot be byte-identical to the Python
//!
//! Ported behaviour first. None of the differences below changes an answer the
//! shipped tables could produce, but each is a place where the port decides
//! something the Python did not decide at all:
//!
//! * The country table (`data/countries.json`, 180 records; two of them carry
//!   Natural Earth's `-99` because no ISO code exists for those places) is
//!   compiled in. `places.py` reads it at run time and silently falls back to an
//!   empty table when the file is broken; that failure mode does not exist here.
//! * The `-99` placeholder is neither a surface form nor an answer code. The port
//!   keys it down to the token `99`, so before this was fixed a story about prices
//!   was located in Northern Cyprus, at confidence 1.
//! * `NAMESAKES` is new: the second city of a name two countries use. The ported
//!   table held one row per name, and the row that came first answered.
//! * Token runs: Python's `\w` is exactly the L*/N*/`_` categories, Rust's
//!   `char::is_alphanumeric` also accepts `Other_Alphabetic` combining marks. No
//!   surface in the gazetteer contains such a mark, so the split is the same.
//! * `str.lower()` never produces a word-final sigma, `str::to_lowercase` does.
//!   No surface contains one.
//! * Numbers are printed with `json::fmt_score` (`6`, not `6.0`) as the crate
//!   contract requires: the same JSON number, one digit shorter.

#![forbid(unsafe_code)]

pub(crate) mod data;
pub(crate) mod index;
pub(crate) mod labels;
pub(crate) mod namesake;
pub mod outlet;
pub(crate) mod phrase;
pub(crate) mod report;
pub(crate) mod scan;
pub(crate) mod score;
#[cfg(test)]
mod tests;

use atlas_lib::json::{self, Json};

use crate::outlet::outlet_country;
use crate::report::{build, push_alternatives, push_row};
use crate::score::{rows_order, score_all, BODY_CHARS};

/// One article, as the bridge sends it. A missing (or non-string) field is the
/// empty string, which is what the Python defaults mean.
pub struct Article {
    pub title: String,
    pub summary: String,
    pub text: String,
    pub url: String,
    pub publisher: String,
}

impl Article {
    pub fn from(root: &Json) -> Article {
        let s = |key: &str| root.get(key).and_then(Json::as_str).unwrap_or("").to_string();
        Article {
            title: s("title"),
            summary: s("summary"),
            text: s("text"),
            url: s("url"),
            publisher: s("publisher"),
        }
    }
}

/// Dispatch one subcommand. `raw` is the whole of stdin.
pub fn run(cmd: &str, raw: &str) -> Result<String, String> {
    let input = atlas_lib::cli::input_object(raw)?;
    match cmd {
        "locate" => locate(&input),
        "explain" => explain(&input),
        "alternatives" => alternatives(&input),
        "outlet" => outlet(&input),
        other => Err(format!("unknown subcommand: {}", other)),
    }
}

/// The place, or nothing, with the runner-ups.
pub fn locate(root: &Json) -> Result<String, String> {
    let a = Article::from(root);
    let (ranked, cands) = score_all(&a.title, &a.summary, &a.text, &a.url, &a.publisher);
    Ok(build(&ranked, &cands, &a.url, &a.publisher))
}

/// The place plus the full scoring table and how much text was looked at.
pub fn explain(root: &Json) -> Result<String, String> {
    let a = Article::from(root);
    let (ranked, cands) = score_all(&a.title, &a.summary, &a.text, &a.url, &a.publisher);
    let chosen = ranked.first().copied();

    let mut out = String::from("{\"result\":");
    out.push_str(&build(&ranked, &cands, &a.url, &a.publisher));
    out.push_str(",\"rows\":[");
    for (i, idx) in rows_order(&cands).iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_row(&mut out, &cands[*idx], Some(*idx) == chosen);
    }
    out.push_str("],\"scanned\":{\"title\":");
    out.push_str(&a.title.chars().count().to_string());
    out.push_str(",\"summary\":");
    out.push_str(&a.summary.chars().count().to_string());
    out.push_str(",\"body\":");
    out.push_str(&a.text.chars().count().min(BODY_CHARS).to_string());
    out.push_str("}}");
    Ok(out)
}

/// `places.alternatives()` does not exist in the Python module, so this is the
/// runner-up list of `explain()`: the same `alternatives` array `locate` returns
/// plus every row of the scoring table that did not win.
pub fn alternatives(root: &Json) -> Result<String, String> {
    let a = Article::from(root);
    let (ranked, cands) = score_all(&a.title, &a.summary, &a.text, &a.url, &a.publisher);
    let chosen = ranked.first().copied();

    let mut out = String::from("{\"alternatives\":[");
    push_alternatives(&mut out, &ranked, &cands);
    out.push_str("],\"runners\":[");
    let mut first = true;
    for idx in rows_order(&cands) {
        if Some(idx) == chosen {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        push_row(&mut out, &cands[idx], false);
    }
    out.push_str("]}");
    Ok(out)
}

/// `outlet_country(url, publisher)` on its own, with the host it was read from.
pub fn outlet(root: &Json) -> Result<String, String> {
    let url = root.get("url").and_then(Json::as_str).unwrap_or("");
    let publisher = root.get("publisher").and_then(Json::as_str).unwrap_or("");
    let mut out = String::from("{\"iso\":");
    json::escape_str(&mut out, outlet_country(url, publisher));
    out.push_str(",\"host\":");
    json::escape_str(&mut out, &crate::outlet::host_of(url));
    out.push('}');
    Ok(out)
}
