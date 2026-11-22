//! Cities whose name another country also uses.
//!
//! `CITIES` holds one row per city name, so a name that two countries share had
//! no way to say so: whichever row came first won, every time, and a text that
//! named the *other* country was overruled by table order. This table is the
//! second (and, where it exists, the third) city of such a shared name.
//!
//! Decides: which other countries use a city name, and where their city is.
//! Does not decide: which one a text means — that is `crate::namesake`, read off
//! the rest of the text. Data only, no logic.
//!
//! A row belongs here only when the other city is one a newsroom would actually
//! mean when it prints that name (Alabama's Birmingham, Venezuela's Valencia,
//! Lebanon's Tripoli), never merely because some village shares a name. The name
//! must be spelled exactly as the `CITIES` row spells it: the index links the two
//! by that spelling, and `namesakes_are_spelled_the_way_the_gazetteer_spells_them`
//! fails if it drifts. The alias is the qualified form a dateline uses, so
//! "Birmingham, Alabama" is answered exactly rather than by country context.
//!
//! Coordinates are the city itself, to the same precision `CITIES` uses: a pin
//! that names a city must sit on that city, not on its country.

/// name, iso3, lat, lon, qualified aliases (| separated) — read after `CITIES`.
pub static NAMESAKES: &[(&str, &str, f64, f64, &str)] = &[
    ("Birmingham", "USA", 33.5186, -86.8104, "Birmingham, Alabama"),
    ("Manchester", "USA", 42.9956, -71.4548, "Manchester, New Hampshire"),
    ("Valencia", "VEN", 10.1620, -68.0077, "Valencia, Venezuela"),
    ("Córdoba", "ESP", 37.8882, -4.7794, "Córdoba, Spain"),
    ("Tripoli", "LBN", 34.4367, 35.8497, "Tripoli, Lebanon"),
    ("Alexandria", "USA", 38.8048, -77.0469, "Alexandria, Virginia"),
    ("Naples", "USA", 26.1423, -81.7948, "Naples, Florida"),
    ("Kingston", "CAN", 44.2312, -76.4860, "Kingston, Ontario"),
    ("Vancouver", "USA", 45.6387, -122.6615, "Vancouver, Washington"),
    ("Perth", "GBR", 56.3950, -3.4308, "Perth, Scotland"),
];
