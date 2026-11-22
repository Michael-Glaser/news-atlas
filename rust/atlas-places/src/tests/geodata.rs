//! The geodata, the codes an answer may carry, and the outlet table.
//!
//! Asserts the things a place answer cannot do: invent a code, treat a number as a
//! place, or read a table out of order. The Natural Earth check reads the vendored
//! file itself, so it fails when the file and the tables stop agreeing.

use super::*;

#[test]
fn a_number_is_not_a_place() {
    // The country table carries Natural Earth's `-99` ("no code"). It was once a
    // surface form, keyed down to the token `99`: a story about prices answered
    // "Northern Cyprus" at conf 1.
    for text in [
        "Inflation hit 99 percent in June",
        "The 1999 budget was approved by the cabinet",
        "99 bottles of beer",
    ] {
        let got = locate(text, "", "", "", "");
        assert_eq!(text_of(&got, "iso"), "", "{} — got {:?}", text, answered(&got));
        assert_eq!(text_of(&got, "place"), "", "{} produced a place", text);
    }
}

#[test]
fn every_answer_code_is_a_real_iso_code() {
    // Three capital letters or nothing: the map's name table is keyed by ISO code,
    // so a placeholder in `iso` is a country the app cannot name.
    let idx = index();
    for code in idx.iso.keys() {
        assert!(
            crate::index::is_iso_code(code),
            "{} is not an ISO 3166-1 alpha-3 code",
            code
        );
    }
    for &(name, iso, _, _, _) in data::COUNTRIES {
        if crate::index::is_iso_code(iso) {
            continue;
        }
        // A place with no code keeps its name and its own entry, and the answer
        // says so by leaving `iso` empty.
        let got = locate(&format!("{name} held a vote"), "", "", "", "");
        assert_eq!(text_of(&got, "place"), name, "{} lost its name", name);
        assert_eq!(text_of(&got, "iso"), "", "{} answered with {}", name, text_of(&got, "iso"));
    }
}

#[test]
fn a_place_with_no_iso_code_says_which_place_it_is() {
    let got = locate("Somaliland holds an election", "", "", "", "");
    assert_eq!(text_of(&got, "place"), "Somaliland");
    assert_eq!(text_of(&got, "iso"), "");
    assert!(num_of(&got, "lat") > 9.0 && num_of(&got, "lat") < 12.0, "the pin is not on Somaliland");
    assert!(num_of(&got, "lon") > 45.0 && num_of(&got, "lon") < 48.0);
    let got = locate("Northern Cyprus talks resume", "", "", "", "");
    assert_eq!(text_of(&got, "place"), "Northern Cyprus");
    assert_eq!(text_of(&got, "iso"), "");
    assert!(num_of(&got, "lat") > 34.0 && num_of(&got, "lat") < 36.0);
}

/// The ISO codes the vendored Natural Earth file publishes: `ISO_A3`, and
/// `ISO_A3_EH` — which is where Natural Earth keeps the real code for the eight
/// features it writes `-99` for (France and Norway among them).
fn vendored_iso_codes() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("public")
        .join("vendor")
        .join("ne_50m_countries.geojson");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    let mut codes: Vec<String> = Vec::new();
    for field in ["\"ISO_A3\":\"", "\"ISO_A3_EH\":\""] {
        let mut from = 0usize;
        while let Some(rel) = text[from..].find(field) {
            let at = from + rel + field.len();
            from = at;
            let rest = &text[at..];
            if rest.len() >= 4 && rest.as_bytes()[3] == b'"' {
                let code = &rest[..3];
                if code.bytes().all(|b| b.is_ascii_uppercase()) && !codes.iter().any(|c| c == code) {
                    codes.push(code.to_string());
                }
            }
        }
    }
    assert!(codes.len() > 150, "the file was not read: {} codes", codes.len());
    codes
}

#[test]
fn every_iso_code_answers_to_a_country_the_vendored_map_publishes() {
    // The map is drawn from `public/vendor/ne_50m_countries.geojson`; an answer
    // carrying a code that file does not publish is a country the map cannot
    // place. Measured against the file itself, not against a list in this test.
    //
    // Two codes are deliberately absent, and neither is invented here:
    // * `GUF` French Guiana — Natural Earth's admin-0 draws it as part of France,
    //   so there is no feature to key on; its own coordinates are right, and it is
    //   what `data/countries.json` calls the place.
    // * `CS-KM` Kosovo — Natural Earth publishes no code at all (`-99`), and ISO
    //   3166 has none; `data/countries.json`, which the server reads for every
    //   country label, and the stored archive both carry `CS-KM`, so changing it
    //   here alone would leave the map with no name for the row.
    let absent_on_purpose: &[&str] = &["GUF", "CS-KM"];
    let published = vendored_iso_codes();
    let idx = index();

    let mut codes: Vec<&str> = Vec::new();
    for &(_, iso, _, _, _) in data::COUNTRIES {
        codes.push(iso);
    }
    for &(_, iso, _, _, _) in data::CITIES {
        codes.push(iso);
    }
    for &(_, iso, _, _, _) in data::NAMESAKES {
        codes.push(iso);
    }
    for &(_, iso) in data::ADJECTIVES {
        codes.push(iso);
    }
    for &(_, iso) in data::COUNTRY_ALIASES {
        codes.push(iso);
    }
    for &(_, iso, _cues) in data::GOV_METONYMS {
        codes.push(iso);
    }
    for &(_, _cues, _alt_cues, alt) in data::AMBIGUOUS {
        if let Some(iso) = alt {
            codes.push(iso);
        }
    }
    for &(_, kind, target) in data::NON_LATIN {
        if kind == "country" {
            codes.push(target);
        }
    }
    for &(_, iso) in data::OUTLET_COUNTRY {
        codes.push(iso);
    }
    for &(_, iso) in data::OUTLET_NAME_COUNTRY {
        codes.push(iso);
    }
    // Every code that survived into the index is answerable too.
    codes.extend(idx.iso.keys().copied());

    let mut missing: Vec<&str> = Vec::new();
    for code in codes {
        // A place with no code (`-99`, the geodata placeholder) is answered with an
        // empty `iso`, so it is not a code that has to be published.
        if !crate::index::is_iso_code(code) {
            continue;
        }
        if absent_on_purpose.contains(&code) || published.iter().any(|p| p == code) {
            continue;
        }
        if !missing.contains(&code) {
            missing.push(code);
        }
    }
    assert!(
        missing.is_empty(),
        "answering with a code the vendored map does not publish: {:?}",
        missing
    );
    assert!(
        missing.is_empty(),
        "answering with a code the vendored map does not publish: {:?}",
        missing
    );
}

#[test]
fn the_tables_read_by_binary_search_are_sorted() {
    // Every lookup in `crate::labels` and `crate::index` is a binary search over a
    // table promised to be sorted. One row out of place makes that lookup silently
    // miss — no error, just a country with no name.
    fn sorted(table: &str, keys: Vec<&str>) {
        assert!(!keys.is_empty(), "{table} is empty");
        for pair in keys.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "{}: {:?} is listed before {:?}",
                table,
                pair[0],
                pair[1]
            );
            assert!(
                pair[0] != pair[1],
                "{}: {:?} is listed twice",
                table,
                pair[0]
            );
        }
    }
    sorted("SHORT_NAMES", data::SHORT_NAMES.iter().map(|r| r.0).collect());
    sorted("ISO_NAMES", data::ISO_NAMES.iter().map(|r| r.0).collect());
    sorted("CAPITAL_CITY", data::CAPITAL_CITY.iter().map(|r| r.0).collect());
    sorted("ADJECTIVES", data::ADJECTIVES.iter().map(|r| r.0).collect());
    sorted("AMBIGUOUS", data::AMBIGUOUS.iter().map(|r| r.0).collect());
    sorted("GOV_METONYMS", data::GOV_METONYMS.iter().map(|r| r.0).collect());
    sorted("GUARDS", data::GUARDS.iter().map(|r| r.0).collect());
    sorted("PRE_GUARDS", data::PRE_GUARDS.iter().map(|r| r.0).collect());

    // And the searches find what the tables hold.
    assert_eq!(crate::labels::iso_name("CS-KM"), "Kosovo");
    assert_eq!(crate::labels::iso_name("HKG"), "Hong Kong");
    assert_eq!(crate::labels::iso_name("ZZZ"), "ZZZ", "an unknown code is itself");
    assert_eq!(crate::labels::short_country("United States of America"), "United States");
    assert_eq!(crate::labels::short_country("Kosovo"), "Kosovo", "no short name, no invention");
    assert_eq!(
        crate::labels::lookup_pair(data::CAPITAL_CITY, "GBR"),
        Some("London")
    );
}

// --- the outlet: the most specific name wins -----------------------------

#[test]
fn the_most_specific_outlet_name_wins() {
    // "The Times of India" contains "the times": taking the first table row that
    // occurs answered GBR for an Indian paper. The longest name that occurs as
    // whole words is the one meant.
    for (publisher, iso) in [
        ("The Times of India", "IND"),
        ("The Hindu BusinessLine", "IND"),
        ("The Times of India (Mumbai)", "IND"),
        ("The Times", "GBR"),
        ("BBC News", "GBR"),
        ("BBC", "GBR"),
        ("CNBC International", "USA"),
        ("Der Spiegel", "DEU"),
        ("FAZ.NET", "DEU"),
        ("NPR", "USA"),
        ("Some Local Paper", ""),
        ("news.lavx.hu", ""),
    ] {
        assert_eq!(
            crate::outlet::outlet_country("", publisher),
            iso,
            "publisher {:?}",
            publisher
        );
    }
}

#[test]
fn a_text_with_no_place_yields_no_place_not_a_country() {
    // The two ways an article can end up with a country: a place the text names,
    // or the outlet as a flagged last resort. Nothing in between.
    let none = locate("Chipmaker beats earnings estimates", "Revenue rose.", "", "", "");
    assert_eq!(text_of(&none, "iso"), "");
    assert_eq!(text_of(&none, "place"), "");
    assert_eq!(text_of(&none, "prec"), "");
    assert_eq!(num_of(&none, "conf"), 0.0);

    let outlet = locate(
        "Chipmaker beats earnings estimates",
        "Revenue rose.",
        "",
        "https://www.reuters.com/x",
        "Reuters",
    );
    assert_eq!(text_of(&outlet, "prec"), "outlet");
    assert_eq!(text_of(&outlet, "iso"), "GBR");
    assert!(num_of(&outlet, "conf") <= 0.2, "the last resort is flagged, not confident");

    // A place the text does name is never reported as the outlet's country.
    let named = locate(
        "Oslo lifts its ban",
        "The Norwegian government said so.",
        "",
        "https://www.reuters.com/x",
        "Reuters",
    );
    assert_eq!(text_of(&named, "iso"), "NOR");
    assert!(text_of(&named, "prec") != "outlet");
}

#[test]
fn a_shared_name_costs_a_small_bounded_amount() {
    // A shared name registers one candidate per namesake for every mention, so the
    // cost has to be measured, not assumed. Two bodies of the same shape and size:
    // one naming shared names, one naming cities only one country has. Measured on
    // this machine: 0.43 ms against 0.15 ms for 16.9 kB, i.e. about 0.3 us per
    // extra candidate — one more candidate per namesake, never a second pass over
    // the text.
    let shared = "Valencia, Tripoli, Perth and Kingston all voted, officials said. ".repeat(260);
    let unique = "Lisbon, Nairobi, Bonn and Quito all voted, officials said. ".repeat(260);
    locate("warm up", "", &shared, "", "");
    let time = |body: &str| -> f64 {
        let mut best = f64::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            for _ in 0..5 {
                locate("A vote was held", "Officials spoke.", body, "", "");
            }
            best = best.min(start.elapsed().as_secs_f64() * 1000.0 / 5.0);
        }
        best
    };
    let (with_shared, without) = (time(&shared), time(&unique));
    println!(
        "{:.0} chars: {:.2} ms with shared names, {:.2} ms without",
        shared.chars().count(),
        with_shared,
        without
    );
    let budget = if cfg!(debug_assertions) { 150.0 } else { 5.0 };
    assert!(with_shared < budget, "{:.2} ms/call over the {:.0} ms budget", with_shared, budget);
    // The factor is a constant, not a curve: a bounded amount over the same body
    // with no shared names, so a future second pass over the text would show up.
    assert!(
        with_shared < without * 8.0 + 1.0,
        "{:.2} ms with shared names against {:.2} ms without — that is not a constant",
        with_shared,
        without
    );
}
