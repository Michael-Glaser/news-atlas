//! A city name that more than one country uses.
//!
//! Decides nothing itself: every case here asserts the answer a caller sees for a
//! name the gazetteer spells once and the world spells in two countries — the text
//! decides, the outlet breaks a tie, and with neither the answer names no place.

use super::*;

#[test]
fn a_shared_city_name_follows_the_country_the_text_names() {
    // Before this was decided by the text, the gazetteer's row order answered:
    // "Valencia, Venezuela" was Valencia in Spain, at conf 0.58, with Venezuela
    // listed as a runner-up — the text saying one country and the answer naming
    // another.
    for (text, iso, place) in [
        ("Valencia, Venezuela: protests in the city centre", "VEN", "Valencia"),
        ("Tripoli, Lebanon: a raid in the north", "LBN", "Tripoli"),
        ("Perth, Scotland: a by-election", "GBR", "Perth"),
        ("Córdoba, Spain: a heatwave", "ESP", "Córdoba"),
    ] {
        let got = locate(text, "", "", "", "");
        assert_eq!(
            (text_of(&got, "iso"), text_of(&got, "place")),
            (iso, place),
            "{} named {} — got {:?}",
            text,
            iso,
            answered(&got)
        );
    }
}

#[test]
fn a_shared_city_name_is_decided_by_the_rest_of_the_text() {
    // No dateline, no qualified name: the country named in the summary decides.
    let got = locate(
        "Valencia floods again",
        "Venezuela's government sent aid to the region.",
        "",
        "",
        "",
    );
    assert_eq!(text_of(&got, "iso"), "VEN");
    assert_eq!(text_of(&got, "place"), "Valencia");
    // A country named in the summary is the same evidence as one in a dateline.
    let got = locate(
        "Manchester reopened",
        "Britain cleared the debris.",
        "",
        "",
        "",
    );
    assert_eq!(text_of(&got, "iso"), "GBR");
    assert_eq!(text_of(&got, "place"), "Manchester");
}

#[test]
fn one_adjective_does_not_decide_between_two_namesakes() {
    // "British" is a weak hint (0.25), not a place: it may not pick a country for
    // a name that two countries use — the same rule the scorer applies to any
    // country adjective on its own.
    let got = locate("Manchester reopened", "The British city cleared the debris.", "", "", "");
    assert_eq!(text_of(&got, "iso"), "", "{:?}", answered(&got));
}

#[test]
fn a_shared_city_name_with_no_context_claims_no_country() {
    // Birmingham is in England and in Alabama, the text names nothing else, and
    // the outlet is unknown: the honest answer is "no place", not the first row
    // of the gazetteer.
    let got = locate("Birmingham shooting downtown", "", "", "", "");
    assert_eq!(text_of(&got, "iso"), "", "a guess pinned a country: {:?}", answered(&got));
    assert_eq!(text_of(&got, "place"), "");
    let raw = run("explain", &payload("Birmingham shooting downtown", "", "", "", "")).unwrap();
    assert!(
        raw.contains("does not say which"),
        "explain must say why it cannot decide: {}",
        raw
    );
    assert!(raw.contains("USA") && raw.contains("GBR"), "both readings are in the table");
}

#[test]
fn the_outlet_breaks_a_tie_between_same_named_cities() {
    // The outlet is the weakest signal left, and it is capped: a city placed by
    // its publisher's country may not read like a city the text named.
    let uk = locate(
        "Birmingham shooting downtown",
        "",
        "",
        "https://www.theguardian.com/uk/x",
        "The Guardian",
    );
    assert_eq!((text_of(&uk, "iso"), text_of(&uk, "place")), ("GBR", "Birmingham"));
    assert!(num_of(&uk, "conf") <= 0.5, "outlet-decided place claims {} conf", num_of(&uk, "conf"));
    let us = locate("Birmingham shooting downtown", "", "", "", "CNN");
    assert_eq!((text_of(&us, "iso"), text_of(&us, "place")), ("USA", "Birmingham"));
    // An outlet whose country is neither namesake still decides nothing: the
    // answer falls back to the outlet country, flagged as such, and never claims
    // one of the two namesake cities.
    let other = locate(
        "Birmingham shooting downtown",
        "",
        "",
        "https://www.aljazeera.com/x",
        "Al Jazeera",
    );
    assert_eq!(text_of(&other, "prec"), "outlet");
    assert_eq!(text_of(&other, "iso"), "QAT");
    assert_ne!(text_of(&other, "place"), "Birmingham", "a city was picked with no evidence");
}

#[test]
fn a_qualified_namesake_is_answered_exactly() {
    // "Birmingham, Alabama" is its own surface: no tie-break is involved.
    for (text, iso, place) in [
        ("Birmingham, Alabama: a shooting downtown", "USA", "Birmingham"),
        ("Manchester, New Hampshire: a primary", "USA", "Manchester"),
        ("Valencia, Venezuela: protests", "VEN", "Valencia"),
        ("Tripoli, Lebanon: a raid", "LBN", "Tripoli"),
        ("Alexandria, Virginia: a court ruling", "USA", "Alexandria"),
        ("Naples, Florida: a hurricane warning", "USA", "Naples"),
        ("Kingston, Ontario: a power cut", "CAN", "Kingston"),
        ("Vancouver, Washington: a fire", "USA", "Vancouver"),
        ("Perth, Scotland: a by-election", "GBR", "Perth"),
        ("Córdoba, Spain: a heatwave", "ESP", "Córdoba"),
    ] {
        let got = locate(text, "", "", "", "");
        assert_eq!(
            (text_of(&got, "iso"), text_of(&got, "place")),
            (iso, place),
            "{} — got {:?}",
            text,
            answered(&got)
        );
    }
}

#[test]
fn every_namesake_is_a_shared_name_the_index_linked() {
    // A namesake row is only useful as a collision: it must repeat a name the
    // gazetteer already has for another country, and the index must have linked
    // the pair (otherwise the second city is unreachable and the row is dead).
    let idx = index();
    for &(name, iso, lat, lon, aliases) in data::NAMESAKES {
        assert!(
            data::CITIES.iter().any(|city| city.0 == name && city.1 != iso),
            "{name} has no namesake in another country"
        );
        assert!(
            !data::CITIES.iter().any(|city| city.0 == name && city.1 == iso),
            "{name} ({iso}) is already in the gazetteer"
        );
        assert!(lat.abs() <= 90.0 && lon.abs() <= 180.0, "{name} has no coordinates");
        let key = crate::phrase::phrase_key(name, name.chars().count() < 5);
        let primary = idx
            .ci
            .get(&key)
            .or_else(|| idx.cs.get(&key))
            .copied()
            .unwrap_or_else(|| panic!("{name} has no surface"));
        let group = idx.entries[primary as usize]
            .group
            .unwrap_or_else(|| panic!("{name} is not linked to its namesake"));
        let members: Vec<&str> = idx.groups[group as usize]
            .iter()
            .map(|&e| idx.entries[e as usize].iso)
            .collect();
        assert!(
            members.contains(&iso) && members.len() >= 2,
            "{name}: the group is {:?}, which does not hold {iso}",
            members
        );
        // The qualified form answers for itself and only itself.
        let alias = aliases.split('|').next().unwrap_or("");
        if !alias.is_empty() {
            let key = crate::phrase::phrase_key(alias, false);
            let entry = idx.ci.get(&key).copied().expect("the qualified alias is a surface");
            assert_eq!(idx.entries[entry as usize].iso, iso, "{} answered for another country", alias);
            assert!(idx.entries[entry as usize].group.is_none(), "{} is not ambiguous", alias);
        }
    }
}

#[test]
fn a_namesake_never_moves_a_country_anchor() {
    // The anchor is the capital a country-level hit is pinned on. A second city
    // arriving in `NAMESAKES` must not take that role.
    let idx = index();
    for (iso, city, lat) in [
        ("USA", "Washington", 38.9),
        ("GBR", "London", 51.5),
        ("ESP", "Madrid", 40.42),
        ("VEN", "Caracas", 10.49),
        ("CAN", "Ottawa", 45.42),
    ] {
        let (alat, _) = *idx.anchors.get(iso).unwrap_or_else(|| panic!("{iso} has no anchor"));
        assert!(
            (alat - lat).abs() < 1.0,
            "{iso} is anchored on {alat}, not on {city} ({lat})"
        );
    }
}
