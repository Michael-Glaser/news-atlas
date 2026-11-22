//! Everything that is not a city or a country record: the surfaces that point at
//! a city already in the gazetteer, and the words that change how a hit is read.
//!
//! Decides, table by table:
//!
//! * `REGION_ALIASES` — districts and streets that belong to one city, and
//!   whether the surface is case-sensitive ("the City" is London, not a city).
//! * `COUNTRY_ALIASES` — surfaces that name a country and not one of its cities.
//! * `ADJECTIVES` — country adjectives ("French officials"): a weak hint, never a
//!   location on their own.
//! * `AMBIGUOUS` — names that are also a person or a plain word ("Jordan"), with
//!   the cues that support this reading, the cues that support the other one,
//!   and what the other reading means.
//! * `GOV_METONYMS` — capitals used as their government ("Washington").
//! * `GUARDS` — a word *after* a hit that voids it ("Washington Post").
//! * `PRE_GUARDS` — a word *before* a hit that voids it ("New Mexico").
//! * `NON_LATIN` — localized names, matched as substrings of the lowered text.
//!
//! Does not decide: when a cue is close enough, or what a voided hit is worth.
//! That is `crate::scan`. Data only, no logic.

/// (surface, city, mode) — mode "cs" keeps the match case-sensitive.
pub static REGION_ALIASES: &[(&str, &str, &str)] = &[
    ("the City", "London", "cs"),
    ("The City", "London", "cs"),
    ("Whitehall", "London", "ci"),
    ("Canary Wharf", "London", "ci"),
];

/// Surfaces that name a country rather than one of its cities.
pub static COUNTRY_ALIASES: &[(&str, &str)] = &[
    ("Scotland", "GBR"),
    ("Wales", "GBR"),
    ("Northern Ireland", "GBR"),
    ("Turkiye", "TUR"),
    ("Washington state", "USA"),
    ("Washington State", "USA"),
];

/// Country adjectives, sorted by key. Alone they never pin a location.
pub static ADJECTIVES: &[(&str, &str)] = &[
    ("afghan", "AFG"),
    ("albanian", "ALB"),
    ("algerian", "DZA"),
    ("american", "USA"),
    ("argentine", "ARG"),
    ("australian", "AUS"),
    ("austrian", "AUT"),
    ("bangladeshi", "BGD"),
    ("belarusian", "BLR"),
    ("belgian", "BEL"),
    ("bolivian", "BOL"),
    ("brazilian", "BRA"),
    ("britisch", "GBR"),
    ("british", "GBR"),
    ("bulgarian", "BGR"),
    ("burmese", "MMR"),
    ("cambodian", "KHM"),
    ("canadian", "CAN"),
    ("chilean", "CHL"),
    ("chinese", "CHN"),
    ("colombian", "COL"),
    ("congolese", "COD"),
    ("croatian", "HRV"),
    ("cuban", "CUB"),
    ("czech", "CZE"),
    ("danish", "DNK"),
    ("dutch", "NLD"),
    ("ecuadorian", "ECU"),
    ("egyptian", "EGY"),
    ("emirati", "ARE"),
    ("english", "GBR"),
    ("ethiopian", "ETH"),
    ("filipino", "PHL"),
    ("finnish", "FIN"),
    ("französisch", "FRA"),
    ("french", "FRA"),
    ("german", "DEU"),
    ("ghanaian", "GHA"),
    ("greek", "GRC"),
    ("haitian", "HTI"),
    ("hungarian", "HUN"),
    ("icelandic", "ISL"),
    ("indian", "IND"),
    ("indonesian", "IDN"),
    ("iranian", "IRN"),
    ("iraqi", "IRQ"),
    ("irish", "IRL"),
    ("israeli", "ISR"),
    ("italian", "ITA"),
    ("japanese", "JPN"),
    ("jordanian", "JOR"),
    ("kenyan", "KEN"),
    ("korean", "KOR"),
    ("kuwaiti", "KWT"),
    ("lebanese", "LBN"),
    ("libyan", "LBY"),
    ("malaysian", "MYS"),
    ("mexican", "MEX"),
    ("moroccan", "MAR"),
    ("nepali", "NPL"),
    ("nigerian", "NGA"),
    ("norwegian", "NOR"),
    ("pakistani", "PAK"),
    ("palestinian", "PSE"),
    ("peruvian", "PER"),
    ("polish", "POL"),
    ("portuguese", "PRT"),
    ("qatari", "QAT"),
    ("romanian", "ROU"),
    ("russian", "RUS"),
    ("saudi", "SAU"),
    ("scottish", "GBR"),
    ("serbian", "SRB"),
    ("singaporean", "SGP"),
    ("slovak", "SVK"),
    ("slovenian", "SVN"),
    ("somali", "SOM"),
    ("spanish", "ESP"),
    ("sudanese", "SDN"),
    ("swedish", "SWE"),
    ("swiss", "CHE"),
    ("syrian", "SYR"),
    ("taiwanese", "TWN"),
    ("thai", "THA"),
    ("tunisian", "TUN"),
    ("turkish", "TUR"),
    ("ugandan", "UGA"),
    ("ukrainian", "UKR"),
    ("venezuelan", "VEN"),
    ("vietnamese", "VNM"),
    ("welsh", "GBR"),
    ("yemeni", "YEM"),
    ("zimbabwean", "ZWE"),
];

/// Ambiguous names, sorted by key: cues for this reading, cues for the
/// other reading, and the ISO code the other reading means (if any).
pub static AMBIGUOUS: &[(&str, &[&str], &[&str], Option<&str>)] = &[
    ("chad",
     &[
         "n'djamena", "ndjamena", "sahel", "junta", "lake chad", "sudan", "libya", "africa", "military",
         "government", "capital", "border", "treaty", "troops", "rebel", "president",
     ],
     &[],
     None),
    ("georgia",
     &[
         "tbilisi", "caucasus", "ossetia", "abkhazia", "georgian dream", "saakashvili", "ivanishvili",
         "black sea", "foreign agent", "parliament", "prime minister", "ruling party", "bidzina",
     ],
     &[
         "atlanta", "senate", "governor", "peach state", "u.s. state", "us state", "congress",
         "savannah", "augusta", "georgia tech",
     ],
     Some("USA")),
    ("guinea",
     &["conakry", "ebola", "bauxite", "west africa", "mining", "africa",],
     &["guinea pig", "guinea fowl",],
     None),
    ("jordan",
     &[
         "king", "amman", "jordanian", "aqaba", "petra", "dead sea", "syria", "israel", "refugee",
         "border", "army", "military", "government", "minister", "parliament", "economy", "airport",
     ],
     &[
         "michael jordan", "jordan peterson", "nba", "basketball", "bulls", "nike", "jordan love",
         "touchdown", "quarterback",
     ],
     None),
    ("salvador",
     &["bahia", "brazil", "brazilian", "carnival", "samba",],
     &[],
     None),
    ("turkey",
     &[
         "ankara", "istanbul", "erdogan", "türkiye", "turkiye", "bosphorus", "syria", "earthquake",
         "parliament", "lira", "kurdistan", "aegean",
     ],
     &["thanksgiving", "roast", "stuffing", "gravy", "bird",],
     None),
    ("tyre",
     &[
         "lebanon", "lebanese", "hezbollah", "israel", "israeli", "south lebanon", "sidon", "naqoura",
         "port",
     ],
     &[],
     None),
];

/// Capital-as-government metonyms: iso3 and the cue words that mean it.
pub static GOV_METONYMS: &[(&str, &str, &[&str])] = &[
    ("washington", "USA",
     &[
         "administration", "senate", "congress", "lawmakers", "sanctions", "tariffs", "federal",
         "capitol", "white house", "treasury", "regulators", "diplomats", "embassy", "ambassador",
         "pentagon", "president", "officials", "bill", "vote", "policy",
     ]),
];

/// Words that may follow a hit and void it (an outlet, not a dateline).
pub static GUARDS: &[(&str, &[&str])] = &[
    ("georgetown", &["university",]),
    ("new york", &["times",]),
    ("the city", &["of",]),
    ("wall street", &["journal",]),
    ("washington", &["post", "times", "square", "university", "bridge", "heights",]),
];

/// Words before a hit that void it ("South America", "New Mexico").
pub static PRE_GUARDS: &[(&str, &[&str])] = &[
    ("america", &["south", "central", "latin", "north", "rural", "corporate", "urban", "asian", "african",]),
    ("mexico", &["new",]),
];

/// (surface, kind, target) — localized names, in NON_LATIN order.
pub static NON_LATIN: &[(&str, &str, &str)] = &[
    ("москва", "city", "Moscow"),
    ("кремль", "city", "Moscow"),
    ("санкт-петербург", "city", "Saint Petersburg"),
    ("россия", "country", "RUS"),
    ("киев", "city", "Kyiv"),
    ("київ", "city", "Kyiv"),
    ("одесса", "city", "Odesa"),
    ("одеса", "city", "Odesa"),
    ("украина", "country", "UKR"),
    ("україна", "country", "UKR"),
    ("中国", "country", "CHN"),
    ("北京", "city", "Beijing"),
    ("上海", "city", "Shanghai"),
    ("深圳", "city", "Shenzhen"),
    ("香港", "city", "Hong Kong"),
    ("台湾", "country", "TWN"),
    ("台北", "city", "Taipei"),
    ("日本", "country", "JPN"),
    ("東京", "city", "Tokyo"),
    ("东京", "city", "Tokyo"),
    ("大阪", "city", "Osaka"),
    ("美国", "country", "USA"),
    ("英国", "country", "GBR"),
    ("法国", "country", "FRA"),
    ("德国", "country", "DEU"),
    ("俄罗斯", "country", "RUS"),
    ("乌克兰", "country", "UKR"),
    ("印度尼西亚", "country", "IDN"),
    ("印度", "country", "IND"),
    ("巴基斯坦", "country", "PAK"),
    ("韩国", "country", "KOR"),
    ("朝鲜", "country", "PRK"),
    ("以色列", "country", "ISR"),
    ("伊朗", "country", "IRN"),
    ("巴西", "country", "BRA"),
    ("墨西哥", "country", "MEX"),
    ("加拿大", "country", "CAN"),
    ("澳大利亚", "country", "AUS"),
    ("서울", "city", "Seoul"),
    ("한국", "country", "KOR"),
    ("평양", "city", "Pyongyang"),
    ("القاهرة", "city", "Cairo"),
    ("مصر", "country", "EGY"),
    ("بيروت", "city", "Beirut"),
    ("لبنان", "country", "LBN"),
    ("دمشق", "city", "Damascus"),
    ("سوريا", "country", "SYR"),
    ("بغداد", "city", "Baghdad"),
    ("العراق", "country", "IRQ"),
    ("طهران", "city", "Tehran"),
    ("إيران", "country", "IRN"),
    ("الرياض", "city", "Riyadh"),
    ("السعودية", "country", "SAU"),
    ("القدس", "city", "Jerusalem"),
    ("إسرائيل", "country", "ISR"),
    ("غزة", "city", "Gaza City"),
    ("تل أبيب", "city", "Tel Aviv"),
    ("موسكو", "city", "Moscow"),
    ("واشنطن", "city", "Washington"),
    ("أوكرانيا", "country", "UKR"),
    ("كييف", "city", "Kyiv"),
    ("Αθήνα", "city", "Athens"),
    ("Ελλάδα", "country", "GRC"),
    ("ירושלים", "city", "Jerusalem"),
    ("ישראל", "country", "ISR"),
    ("กรุงเทพ", "city", "Bangkok"),
];
