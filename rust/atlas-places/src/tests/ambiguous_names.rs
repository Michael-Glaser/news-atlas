//! Names that are also a place, and places that are also something else.
//!
//! Decides nothing: asserts the answers for Georgia the state and Georgia the
//! country, Jordan the person and Jordan the country, a capital that stands for
//! its government, and an outlet name that looks like a dateline.

use super::*;

// --- a US state that shares a country's name ---------------------------

#[test]
fn georgia_is_the_us_state() {
    case(
        "Georgia = the US state",
        "Georgia Senate race tightens as Atlanta turnout surges",
        "Georgia election officials said turnout was up.",
        Want {
            iso: Some("USA"),
            prec: Some("city"),
            place: Some("Atlanta"),
            ..Want::default()
        },
    );
}

#[test]
fn georgia_is_the_country() {
    case(
        "Georgia = the country",
        "Georgia signs security pact with the EU in Tbilisi",
        "The Caucasus country moved closer to Brussels.",
        Want {
            iso: Some("GEO"),
            place: Some("Tbilisi"),
            ..Want::default()
        },
    );
}

#[test]
fn georgia_country_party_politics() {
    case(
        "Georgia = the country, party politics",
        "Georgia's ruling party tightens grip",
        "Parliament approved the foreign agent law.",
        Want {
            iso: Some("GEO"),
            prec: Some("country"),
            place: Some("Georgia"),
            ..Want::default()
        },
    );
}

// --- a person who shares a country's name ------------------------------

#[test]
fn jordan_is_the_person() {
    case(
        "Jordan = the person",
        "Jordan scores 34 as the Bulls win again",
        "Michael Jordan watched from the stands.",
        Want {
            iso: Some(""),
            prec: Some(""),
            ..Want::default()
        },
    );
}

#[test]
fn jordan_is_the_country() {
    case(
        "Jordan = the country",
        "Jordan warns Israel over border incursion",
        "King Abdullah said Amman would respond.",
        Want {
            iso: Some("JOR"),
            ..Want::default()
        },
    );
}

#[test]
fn jordan_without_supporting_context() {
    case(
        "Jordan with no supporting context",
        "Jordan posts weaker exports",
        "Trade data disappointed analysts.",
        Want {
            iso: Some(""),
            prec: Some(""),
            ..Want::default()
        },
    );
}

// --- near-identical country names --------------------------------------

#[test]
fn niger_vs_nigeria() {
    case(
        "Niger vs Nigeria",
        "Nigeria recalls ambassador from Niger",
        "Abuja said Niamey's junta had crossed a line.",
        Want {
            iso: Some("NGA"),
            alt_iso: Some("NER"),
            ..Want::default()
        },
    );
}

#[test]
fn niger_on_its_own() {
    case(
        "Niger on its own",
        "Niger coup leaders name a new prime minister",
        "Niamey stayed calm after the announcement.",
        Want {
            iso: Some("NER"),
            ..Want::default()
        },
    );
}

// --- a city against its own country ------------------------------------

#[test]
fn seoul_outranks_south_korea() {
    case(
        "Seoul outranks South Korea",
        "South Korea president faces backlash in Seoul",
        "South Korea's parliament voted on the bill.",
        Want {
            iso: Some("KOR"),
            prec: Some("city"),
            place: Some("Seoul"),
            ..Want::default()
        },
    );
}

#[test]
fn rio_outranks_brazil() {
    case(
        "Rio outranks Brazil",
        "Rio de Janeiro and Brazil brace for carnival",
        "Brazil's government sent funds to Rio de Janeiro.",
        Want {
            iso: Some("BRA"),
            prec: Some("city"),
            place: Some("Rio"),
            ..Want::default()
        },
    );
}

// --- a capital that is also the government -----------------------------

#[test]
fn washington_is_the_government() {
    case(
        "Washington = the government",
        "Washington imposes new sanctions on Iran",
        "The administration said the measures target banks.",
        Want {
            iso: Some("USA"),
            prec: Some("country"),
            place: Some("United States"),
            ..Want::default()
        },
    );
}

#[test]
fn washington_is_the_dateline() {
    case(
        "Washington = the dateline",
        "Washington's cherry blossoms bloom early",
        "Gardeners along the Potomac expect peak bloom.",
        Want {
            iso: Some("USA"),
            prec: Some("city"),
            place: Some("Washington"),
            ..Want::default()
        },
    );
}

// --- outlets that look like places -------------------------------------

#[test]
fn washington_post_is_not_a_dateline() {
    case(
        "Washington Post is not a dateline",
        "Washington Post staff strike",
        "Union members walked out.",
        Want {
            iso: Some(""),
            prec: Some(""),
            ..Want::default()
        },
    );
}

#[test]
fn new_york_times_is_not_a_dateline() {
    case(
        "New York Times is not a dateline",
        "New York Times cuts staff",
        "The paper said buyouts were offered.",
        Want {
            iso: Some(""),
            prec: Some(""),
            ..Want::default()
        },
    );
}

#[test]
fn no_place_anywhere() {
    case(
        "no place anywhere in the article",
        "Chipmaker beats earnings estimates",
        "Revenue rose 12 percent on strong demand.",
        Want {
            iso: Some(""),
            prec: Some(""),
            conf: Some(0.0),
            ..Want::default()
        },
    );
}

// --- last resort: the outlet's own country -----------------------------

#[test]
fn outlet_fallback_by_url() {
    check(
        "outlet-only fallback by url",
        "Chipmaker beats earnings estimates",
        "Revenue rose 12 percent on strong demand.",
        "",
        "https://www.nytimes.com/2024/05/revenue.html",
        "",
        Want {
            iso: Some("USA"),
            prec: Some("outlet"),
            conf: Some(0.2),
            ..Want::default()
        },
    );
}

#[test]
fn outlet_fallback_by_publisher() {
    check(
        "outlet-only fallback by publisher",
        "Chipmaker beats earnings estimates",
        "Revenue rose 12 percent on strong demand.",
        "",
        "",
        "The Guardian",
        Want {
            iso: Some("GBR"),
            prec: Some("outlet"),
            conf: Some(0.2),
            ..Want::default()
        },
    );
}

// --- non-Latin scripts -------------------------------------------------

#[test]
fn cyrillic_text() {
    case(
        "Cyrillic text",
        "Москва и Киев обменялись обвинениями",
        "Стороны не договорились о встрече.",
        Want {
            iso: Some("RUS"),
            place: Some("Moscow"),
            ..Want::default()
        },
    );
}

#[test]
fn chinese_text() {
    case(
        "Chinese text",
        "中国央行下调利率",
        "北京方面表示将支持增长。",
        Want {
            iso: Some("CHN"),
            place: Some("Beijing"),
            ..Want::default()
        },
    );
}

#[test]
fn arabic_text() {
    case(
        "Arabic text",
        "مصر تفتح معبر رفح",
        "قالت القاهرة إن شاحنات المساعدات عبرت.",
        Want {
            iso: Some("EGY"),
            place: Some("Cairo"),
            ..Want::default()
        },
    );
}
