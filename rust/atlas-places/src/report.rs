//! The wire format: the exact bytes each subcommand writes to stdout.
//!
//! Decides: key order, number formatting (`6`, not `6.0`), the two-decimal
//! rounding of scores, the six-decimal coordinates, the escaped strings, the
//! chosen place's label and coordinates (including the country anchor), the
//! outlet fallback and the empty answer.
//!
//! Does not decide: which candidate won or what it scored — it is handed a
//! ranked list and reports it. Building the JSON twice for the same input is
//! therefore impossible by construction.

use atlas_lib::json;

use crate::index::{index, Kind};
use crate::labels::{iso_name, short_country};
use crate::outlet::outlet_country;
use crate::score::{conf_of, round2, Cand};


fn push_num(out: &mut String, v: f64) {
    out.push_str(&json::fmt_score(v));
}

fn push_coord(out: &mut String, v: f64) {
    out.push_str(&format!("{:.6}", v));
}

fn push_kind(out: &mut String, kind: Kind) {
    json::escape_str(out, kind.name());
}

pub(crate) fn push_row(out: &mut String, c: &Cand, chosen: bool) {
    out.push_str("{\"place\":");
    json::escape_str(
        out,
        if c.kind == Kind::City {
            c.place
        } else {
            short_country(c.place)
        },
    );
    out.push_str(",\"iso\":");
    json::escape_str(out, c.iso);
    out.push_str(",\"kind\":");
    push_kind(out, c.kind);
    out.push_str(",\"title\":");
    out.push_str(&c.n[0].to_string());
    out.push_str(",\"summary\":");
    out.push_str(&c.n[1].to_string());
    out.push_str(",\"body\":");
    out.push_str(&c.n[2].to_string());
    out.push_str(",\"score\":");
    push_num(out, round2(c.score));
    out.push_str(",\"rank\":");
    push_num(out, round2(c.rank));
    out.push_str(",\"chosen\":");
    out.push_str(if chosen { "true" } else { "false" });
    out.push_str(",\"note\":");
    json::escape_str(
        out,
        if !c.note.is_empty() {
            c.note
        } else if chosen {
            "chosen"
        } else {
            "runner-up"
        },
    );
    out.push('}');
}

pub(crate) fn push_alternatives(out: &mut String, ranked: &[usize], cands: &[Cand]) {
    if ranked.is_empty() {
        return;
    }
    let best = &cands[ranked[0]];
    let mut written = 0usize;
    for &k in &ranked[1..] {
        let c = &cands[k];
        if (c.iso, c.place) == (best.iso, best.place) {
            continue;
        }
        if written > 0 {
            out.push(',');
        }
        written += 1;
        out.push_str("{\"iso\":");
        json::escape_str(out, c.iso);
        out.push_str(",\"place\":");
        json::escape_str(
            out,
            if c.kind == Kind::City {
                c.place
            } else {
                short_country(c.place)
            },
        );
        out.push_str(",\"prec\":");
        push_kind(out, c.kind);
        out.push_str(",\"score\":");
        push_num(out, round2(c.score));
        out.push_str(",\"conf\":");
        push_num(out, conf_of(c, best.score));
        out.push('}');
        if written == 3 {
            break;
        }
    }
}

/// `_build`: the public result dict, so `locate()` and `explain()` agree.
pub(crate) fn build(ranked: &[usize], cands: &[Cand], url: &str, publisher: &str) -> String {
    let mut out = String::new();
    if let Some(&top) = ranked.first() {
        let best = &cands[top];
        let rival = ranked[1..]
            .iter()
            .map(|&k| &cands[k])
            .find(|c| {
                !(best.kind == Kind::City && c.kind == Kind::Country && c.iso == best.iso)
            })
            .map(|c| c.score)
            .unwrap_or(0.0);
        let mut place = best.place;
        let mut lat = best.lat;
        let mut lon = best.lon;
        if best.kind == Kind::Country {
            place = short_country(place);
            if let Some(&(alat, alon)) = index().anchors.get(best.iso) {
                lat = alat;
                lon = alon;
            }
        }
        let mut conf = conf_of(best, rival);
        if best.pw == 0.0 {
            conf = conf.min(0.5); // nothing but adjectives backs this place
        }
        // A namesake decided by the outlet's country is a weaker claim than a text
        // that names the country: `crate::namesake` set the ceiling.
        conf = conf.min(best.conf_cap);
        out.push_str("{\"iso\":");
        json::escape_str(&mut out, best.iso);
        out.push_str(",\"lat\":");
        push_coord(&mut out, lat);
        out.push_str(",\"lon\":");
        push_coord(&mut out, lon);
        out.push_str(",\"place\":");
        json::escape_str(&mut out, place);
        out.push_str(",\"prec\":");
        push_kind(&mut out, best.kind);
        out.push_str(",\"conf\":");
        push_num(&mut out, conf);
        out.push_str(",\"alternatives\":[");
        push_alternatives(&mut out, ranked, cands);
        out.push_str("]}");
        return out;
    }

    // Last resort: the outlet's own country, flagged so the map can show it
    // differently. Never the primary location.
    let iso = outlet_country(url, publisher);
    if !iso.is_empty() {
        let idx = index();
        let rec = idx.countries.get(iso);
        let mut lat = 0.0;
        let mut lon = 0.0;
        if let Some(&(alat, alon)) = idx.anchors.get(iso) {
            lat = alat;
            lon = alon;
        } else if let Some(rec) = rec {
            lat = rec.lat;
            lon = rec.lon;
        }
        out.push_str("{\"iso\":");
        json::escape_str(&mut out, iso);
        out.push_str(",\"lat\":");
        push_coord(&mut out, lat);
        out.push_str(",\"lon\":");
        push_coord(&mut out, lon);
        out.push_str(",\"place\":");
        json::escape_str(
            &mut out,
            match rec {
                Some(rec) => short_country(rec.name),
                None => iso_name(iso),
            },
        );
        out.push_str(",\"prec\":\"outlet\",\"conf\":0.2,\"alternatives\":[]}");
        return out;
    }
    out.push_str(
        "{\"iso\":\"\",\"lat\":0.000000,\"lon\":0.000000,\"place\":\"\",\"prec\":\"\",\"conf\":0,\"alternatives\":[]}",
    );
    out
}
