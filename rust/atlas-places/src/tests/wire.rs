//! The bytes of every answer, and the inputs that must not break them.
//!
//! Key order, number formatting, escaping, the empty answer, and the boundary
//! cases — a 10 kB token, a 20 kB multi-byte body, a payload with no keys at all.

use super::*;

// --- boundary cases ----------------------------------------------------

#[test]
fn empty_article_locates_nothing() {
    let got = locate("", "", "", "", "");
    assert_eq!(text_of(&got, "iso"), "");
    assert_eq!(text_of(&got, "place"), "");
    assert_eq!(text_of(&got, "prec"), "");
    assert_eq!(num_of(&got, "conf"), 0.0);
    assert!(alts(&got).is_empty());
    // Same answer for a payload that omits every key.
    let raw = run("locate", "{}").unwrap();
    let got = json::parse(&raw).unwrap();
    assert_eq!(text_of(&got, "iso"), "");
}

#[test]
fn city_inside_a_longer_word_is_not_a_hit() {
    // "Riga" inside "rigatoni", "Oslo" inside "Oslo's" is fine but "Los" is
    // not a city: only whole alphanumeric runs match.
    let got = locate("Rigatoni and tagliatelle on the menu", "Chefs agreed.", "", "", "");
    assert_eq!(text_of(&got, "iso"), "", "Rigatoni is not Riga");
    let got = locate("Oslo and Riga trade notes", "Both capitals agreed.", "", "", "");
    assert_eq!(text_of(&got, "iso"), "NOR");
    assert_eq!(text_of(&got, "place"), "Oslo");
}

#[test]
fn two_cities_of_the_same_name_stay_apart() {
    // San Jose (California) and San José (Costa Rica) are different places.
    let got = locate(
        "San Jose and San José both grow",
        "The two mayors spoke.",
        "",
        "",
        "",
    );
    assert_eq!(text_of(&got, "iso"), "USA");
    assert_eq!(text_of(&got, "place"), "San Jose");
    let have: Vec<(&str, &str)> = alts(&got)
        .iter()
        .map(|a| (text_of(a, "iso"), text_of(a, "place")))
        .collect();
    assert!(
        have.contains(&("CRI", "San José")),
        "the Costa Rican namesake is a runner-up, got {:?}",
        have
    );
}

#[test]
fn avant_garde_punctuation_survives() {
    // Quotes, newlines, emoji, em dashes and a stray "<" must not break the
    // JSON — and the place is still found.
    let got = locate(
        "\"Quotes\" 🚀 and a — dash: Washington?\n<not a tag>",
        "A tab\there\r\nand Киев too.",
        "",
        "",
        "",
    );
    assert_eq!(text_of(&got, "iso"), "USA");
    let raw = run(
        "locate",
        &payload("\"Quotes\" 🚀", "line\nbreak\ttab", "", "", ""),
    )
    .unwrap();
    assert!(json::parse(&raw).is_ok(), "output is valid JSON: {}", raw);
}

#[test]
fn long_single_line_without_whitespace() {
    // One 10 kB token: no boundaries anywhere, still no panic, no hit.
    let blob = "A".repeat(10_000);
    let got = locate(&blob, "", &blob, "", "");
    assert_eq!(text_of(&got, "iso"), "");
    let got = call("explain", &blob, "", &blob, "", "");
    let scanned = got.get("scanned").expect("scanned");
    assert_eq!(scanned.get("title").and_then(Json::as_usize), Some(10_000));
    assert_eq!(scanned.get("body").and_then(Json::as_usize), Some(10_000));
}

#[test]
fn twenty_kilobyte_body_is_scanned_with_char_indices() {
    // A multi-byte body: the cue window must clip on character boundaries.
    let body = "北京方面表示将支持增长。".repeat(2_000);
    let got = call("explain", "", "", &body, "", "");
    let scanned = got.get("scanned").expect("scanned");
    assert_eq!(scanned.get("body").and_then(Json::as_usize), Some(BODY_CHARS));
    assert_eq!(text_of(&locate("", "", &body, "", ""), "iso"), "CHN");
}

#[test]
fn cyrillic_inflections_and_stems() {
    // Москва, Москвы and Москве all match the stored stem "москв".
    let got = locate("Москве и Москвы: кремль молчит", "В Санкт-Петербурге тихо.", "", "", "");
    assert_eq!(text_of(&got, "iso"), "RUS");
    assert_eq!(text_of(&got, "place"), "Moscow");
}

#[test]
fn cjk_and_hangul_headlines() {
    let got = locate("東京の株式市場は急落", "日本銀行は金利を据え置いた。", "", "", "");
    assert_eq!(text_of(&got, "iso"), "JPN");
    assert_eq!(text_of(&got, "place"), "Tokyo");
    let got = locate("서울의 봄", "한국 정부가 발표했다.", "", "", "");
    assert_eq!(text_of(&got, "iso"), "KOR");
    assert_eq!(text_of(&got, "place"), "Seoul");
}

#[test]
fn arabic_needs_a_word_boundary() {
    // "مصر" (Egypt) must not fire inside "مصرف" (a bank).
    let got = locate("مصرف الرافدين يعلن أرباحا", "", "", "", "");
    assert_eq!(text_of(&got, "iso"), "", "a bank is not Egypt");
    let got = locate("مصر تفتح معبر رفح", "", "", "", "");
    assert_eq!(text_of(&got, "iso"), "EGY");
}

#[test]
fn pre_guards_and_guards_hold() {
    // "South America" is a continent, "New Mexico" is a US state.
    let got = locate("South America and New Mexico", "Boston hosted the talks.", "", "", "");
    assert_eq!(text_of(&got, "iso"), "USA");
    assert_eq!(text_of(&got, "place"), "Boston");
    // "Wall Street Journal" is an outlet, not the street.
    let got = locate("Wall Street Journal layoffs", "The paper confirmed them.", "", "", "");
    assert_eq!(text_of(&got, "iso"), "");
}

#[test]
fn multi_word_names_bridge_name_punctuation() {
    let got = locate("Tel-Aviv rally draws thousands", "Protesters gathered.", "", "", "");
    assert_eq!(text_of(&got, "place"), "Tel Aviv");
    let got = locate("St. Louis arch reopens", "Crews finished the repairs.", "", "", "");
    assert_eq!(text_of(&got, "place"), "St. Louis");
    let got = locate("N'Djamena talks resume", "Chad's government met rebels.", "", "", "");
    assert_eq!(text_of(&got, "place"), "N'Djamena");
}

#[test]
fn title_hits_outweigh_body_repetition() {
    // One title mention beats any amount of body repetition: the per-zone
    // weight is capped at 4 for the body, so 200 mentions still score 4,
    // against 6 for the single title hit.
    let body = "Chicago. ".repeat(200);
    let got = locate("Boston braces for snow", "", &body, "", "");
    assert_eq!(text_of(&got, "place"), "Boston");
    assert_eq!(text_of(&got, "prec"), "city");
    let table = call("explain", "Boston braces for snow", "", &body, "", "");
    let rows = table.get("rows").and_then(Json::as_arr).expect("rows");
    let chicago = rows
        .iter()
        .find(|r| text_of(r, "place") == "Chicago")
        .expect("Chicago row");
    assert_eq!(
        chicago.get("body").and_then(Json::as_usize),
        Some(200),
        "every body mention is counted"
    );
    assert_eq!(num_of(chicago, "score"), 4.0, "but the zone weight is capped at 4");
    assert_eq!(num_of(chicago, "rank"), 5.2);
}

// --- wire format -------------------------------------------------------

#[test]
fn outlet_subcommand_reads_urls_and_publishers() {
    let get = |url: &str, publisher: &str| -> String {
        let mut p = String::from("{\"url\":");
        json::escape_str(&mut p, url);
        p.push_str(",\"publisher\":");
        json::escape_str(&mut p, publisher);
        p.push('}');
        let got = json::parse(&run("outlet", &p).unwrap()).unwrap();
        text_of(&got, "iso").to_string()
    };
    assert_eq!(get("https://www.nytimes.com/2024/x.html", ""), "USA");
    assert_eq!(get("https://edition.cnn.com/x", ""), "USA");
    assert_eq!(get("http://news.bbc.co.uk/x", ""), "GBR");
    assert_eq!(get("https://user:pw@www.theguardian.com/x", ""), "GBR");
    assert_eq!(get("//www.reuters.com/x", ""), "GBR");
    assert_eq!(get("www.nytimes.com/x", ""), "", "no scheme, no host");
    assert_eq!(get("", "The New York Times"), "USA");
    assert_eq!(get("", "Bloomberg News"), "USA");
    assert_eq!(get("", "Some Local Paper"), "");
    assert_eq!(get("", "  Reuters  "), "GBR");
    assert_eq!(get("", ""), "");
}

#[test]
fn missing_and_bad_input() {
    assert!(run("locate", "[]").is_err(), "an array is not an object");
    assert!(run("locate", "not json").is_err());
    assert!(run("nope", "{}").is_err(), "unknown subcommand");
    // Missing keys are the Python defaults, not an error.
    assert!(run("locate", "{}").is_ok());
    assert!(run("explain", "{}").is_ok());
    assert!(run("alternatives", "{}").is_ok());
    assert!(run("outlet", "{}").is_ok());
    // A non-string field is treated as empty rather than trusted.
    let got = json::parse(&run("locate", "{\"title\": 42, \"summary\": null}").unwrap()).unwrap();
    assert_eq!(text_of(&got, "iso"), "");
}

#[test]
fn rounding_matches_python() {
    // round(x, 2) in Python: correctly rounded decimal, ties to even.
    let cases = [
        (0.125, 0.12),
        (0.375, 0.38),
        (2.675, 2.67),
        (1.005, 1.0),
        (0.615, 0.61),
        (0.115, 0.12),
        (0.005, 0.01),
        (0.505, 0.51),
        (0.635, 0.64),
        (0.045, 0.04),
        (1.235, 1.24),
        (0.0, 0.0),
    ];
    for (input, want) in cases {
        assert_eq!(round2(input), want, "round2({})", input);
    }
}

#[test]
fn alternatives_subcommand_lists_runner_ups() {
    let got = call(
        "alternatives",
        "Gaza ceasefire holds in Rafah",
        "Aid trucks entered Khan Younis. Talks in Cairo continued.",
        "Cairo hosts the talks again.",
        "",
        "",
    );
    let list = got.get("alternatives").and_then(Json::as_arr).expect("alternatives");
    assert_eq!(list.len(), 3);
    let runners = got.get("runners").and_then(Json::as_arr).expect("runners");
    assert!(runners.iter().all(|r| r.get("chosen") == Some(&Json::Bool(false))));
    assert!(!runners.is_empty());
}

#[test]
fn json_is_escaped_everywhere() {
    // Quotes, a newline and a stray tag in the input must never reach the
    // output as raw bytes: one object, one line, parseable.
    let raw = run(
        "locate",
        &payload("Washington \"DC\" <script>\nline two\ttab", "", "", "", ""),
    )
    .unwrap();
    assert!(json::parse(&raw).is_ok(), "output is valid JSON: {}", raw);
    assert!(!raw.contains('\n') && !raw.contains('\t'), "one line, no control bytes");
    let got = json::parse(&raw).unwrap();
    assert_eq!(text_of(&got, "place"), "Washington");
    // The same through explain(), whose rows come from the gazetteer.
    let raw = run("explain", &payload("\"🚀\" Washington", "", "", "", "")).unwrap();
    assert!(json::parse(&raw).is_ok());
    assert!(!raw.contains('\n'));
}

#[test]
fn index_has_the_python_shape() {
    // geo_check.py: 509 cities, 1222 surface forms, 66 non-Latin forms. The
    // longest surface is a five-token country name ("Democratic Republic of
    // the Congo"), so the lookahead never has to try more than five tokens.
    //
    // The port adds `NAMESAKES`, and each of those rows adds exactly one surface
    // (its qualified alias, "Valencia, Venezuela"): its bare name is spelled the
    // same as the gazetteer row on purpose, so it collides instead of adding a key.
    // Spelling a namesake differently would make this count too high — the test
    // fails rather than the link silently missing.
    let idx = index();
    assert_eq!(data::CITIES.len(), 509);
    assert_eq!(idx.ci.len() + idx.cs.len(), 1222 + data::NAMESAKES.len());
    assert_eq!(idx.nonlatin.len(), 66);
    assert_eq!(idx.maxrun, 5);
    assert!(idx.first.contains("new") && idx.first.contains("south"));
}
