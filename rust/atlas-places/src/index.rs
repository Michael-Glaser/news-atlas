//! The search index: every surface form a text can contain, resolved once.
//!
//! Decides: which surface forms exist (cities, their aliases, districts,
//! countries, country aliases, adjectives, localized names), which place each
//! one means, which guard/cue rules apply to it, and the lookup shape the scan
//! needs (a case-insensitive table, a case-sensitive table, the set of first
//! tokens worth a lookahead, and the localized names longest-first).
//!
//! Does not decide: what a text actually says. The scan reads this index, the
//! build never looks at a text. Build order is preserved exactly as the Python
//! index build depends on it.
//!
//! The index is built once per process, on first use, and then only borrowed.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::data;
use crate::labels::{iso_name, lookup_pair};
use crate::phrase::phrase_key;

/// Cyrillic inflects place names, so those are stored as a stem.
const CYR_VOWELS: &str = "аеёиоуыэюя";

/// Is this an ISO 3166-1 alpha-3 code: three capital letters?
///
/// It has to be asked. Natural Earth — and the geodata copied from it — writes
/// `-99` where a place has no code (Northern Cyprus, Somaliland), and a
/// placeholder is not a code: an answer carrying it names a country the map
/// cannot look up, and `data/countries.json` (what the server prints) maps `-99`
/// to whichever of those two it read last.
pub(crate) fn is_iso_code(iso: &str) -> bool {
    iso.len() == 3 && iso.bytes().all(|b| b.is_ascii_uppercase())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    City,
    Country,
}

impl Kind {
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Kind::City => 0,
            Kind::Country => 1,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::City => "city",
            Kind::Country => "country",
        }
    }
}

/// The context rules of one surface form, copied out of the static tables.
#[derive(Clone, Copy)]
pub(crate) struct Ambig {
    pub(crate) cues: &'static [&'static str],
    pub(crate) alt_cues: &'static [&'static str],
    pub(crate) alt: Option<&'static str>,
}

#[derive(Clone, Copy)]
pub(crate) struct Gov {
    pub(crate) iso: &'static str,
    pub(crate) cues: &'static [&'static str],
}

/// One surface form's place entry: what Python stores as the dict in `ci`/`cs`.
#[derive(Clone, Copy)]
pub(crate) struct Entry {
    pub(crate) kind: Kind,
    pub(crate) place: &'static str,
    pub(crate) iso: &'static str,
    pub(crate) lat: f64,
    pub(crate) lon: f64,
    pub(crate) adj: bool,
    pub(crate) guard: Option<&'static [&'static str]>,
    pub(crate) pre: Option<&'static [&'static str]>,
    pub(crate) ambig: Option<Ambig>,
    pub(crate) gov: Option<Gov>,
    /// Same-named cities in other countries: an index into [`Index::groups`], so
    /// a mention of a shared name offers every city that carries it and the text
    /// decides (`crate::namesake`). `None` for a name only one place uses.
    pub(crate) group: Option<u32>,
}

pub(crate) struct CountryRec {
    pub(crate) name: &'static str,
    pub(crate) lat: f64,
    pub(crate) lon: f64,
}

pub(crate) struct Index {
    pub(crate) entries: Vec<Entry>,
    /// ISO code -> the country-level entry `_iso_entry()` returns.
    pub(crate) iso: HashMap<&'static str, u32>,
    /// Case-insensitive surface table (surface of 5+ characters).
    pub(crate) ci: HashMap<String, u32>,
    /// Case-sensitive surface table (short surfaces: LA, SF, US, Rio, ...).
    pub(crate) cs: HashMap<String, u32>,
    /// First tokens of multi-word surfaces — the only ones worth a lookahead.
    pub(crate) first: HashSet<String>,
    pub(crate) maxrun: usize,
    /// (lowered surface, entry, needs a trailing word boundary), longest first.
    pub(crate) nonlatin: Vec<(String, u32, bool)>,
    /// Country-level hits are pinned on the capital, not a polygon centroid.
    pub(crate) anchors: HashMap<&'static str, (f64, f64)>,
    /// ISO code -> country record from the bundled GeoJSON.
    pub(crate) countries: HashMap<&'static str, CountryRec>,
    /// Every entry a shared city name belongs to, in gazetteer order.
    pub(crate) groups: Vec<Vec<u32>>,
}

/// The process-wide index. Built on first use, then borrowed for the rest of
/// the run: building it twice would be the only way to see two answers.
pub(crate) fn index() -> &'static Index {
    static ONCE: OnceLock<Index> = OnceLock::new();
    ONCE.get_or_init(build_index)
}


struct Builder {
    entries: Vec<Entry>,
    iso: HashMap<&'static str, u32>,
    countries: HashMap<&'static str, CountryRec>,
    city_base: HashMap<&'static str, u32>,
    ci: HashMap<String, u32>,
    cs: HashMap<String, u32>,
    groups: Vec<Vec<u32>>,
}

impl Builder {
    /// One country-level entry, built from its own record.
    ///
    /// A record whose "code" is not a code (Natural Earth writes `-99` where a
    /// place has none) still gets an entry — the place is real — but with no code
    /// and no place in the ISO table: an answer must never carry a placeholder,
    /// and two code-less places must not collapse into one entry.
    fn push_country(&mut self, place: &'static str, iso: &'static str, lat: f64, lon: f64) -> u32 {
        let code = if is_iso_code(iso) { iso } else { "" };
        let idx = self.entries.len() as u32;
        self.entries.push(Entry {
            kind: Kind::Country,
            place,
            iso: code,
            lat,
            lon,
            adj: false,
            guard: None,
            pre: None,
            ambig: None,
            gov: None,
            group: None,
        });
        if !code.is_empty() {
            self.iso.insert(code, idx);
        }
        idx
    }

    /// `_iso_entry(iso)`: the country-level entry, created on first use.
    fn country(&mut self, iso: &'static str) -> u32 {
        if let Some(&idx) = self.iso.get(iso) {
            return idx;
        }
        let (place, lat, lon) = match self.countries.get(iso) {
            Some(rec) => (rec.name, rec.lat, rec.lon),
            None => (iso_name(iso), 0.0, 0.0),
        };
        self.push_country(place, iso, lat, lon)
    }

    /// `add()`: one surface form -> one place entry, first surface wins.
    fn add(&mut self, surface: &str, base: u32, base_adj: bool, case_sensitive: bool) {
        // A surface with no letter is not a name. `-99` (the geodata's placeholder
        // for "no code") would otherwise be keyed as `99`, and every text with the
        // number 99 in it would answer "Northern Cyprus".
        if !surface.chars().any(char::is_alphabetic) {
            return;
        }
        let key = phrase_key(surface, case_sensitive);
        if key.is_empty() {
            return;
        }
        let lower = surface.to_lowercase();
        let guard = lookup_words(data::GUARDS, &lower);
        if let Some(&known) = if case_sensitive {
            self.cs.get(&key)
        } else {
            self.ci.get(&key)
        } {
            // A city keeps its name against a country alias, but still learns
            // the guard of the surface that lost.
            if let Some(g) = guard {
                let entry = &mut self.entries[known as usize];
                if entry.guard.is_none() {
                    entry.guard = Some(g);
                }
            }
            self.link_namesake(known, base);
            return;
        }
        let mut entry = self.entries[base as usize];
        entry.adj = base_adj;
        // A copy answers for its own surface only: a shared name is linked on the
        // surface the gazetteer spells bare ("Valencia"), never through an alias
        // ("Valencia, Venezuela"), which is already unambiguous.
        entry.group = None;
        if let Some(g) = guard {
            entry.guard = Some(g);
        }
        if let Some(p) = lookup_words(data::PRE_GUARDS, &lower) {
            entry.pre = Some(p);
        }
        if let Some(a) = lookup_ambig(&lower) {
            entry.ambig = Some(a);
        }
        if let Some(g) = lookup_gov(&lower) {
            entry.gov = Some(g);
            self.country(g.iso);
        }
        let idx = self.entries.len() as u32;
        self.entries.push(entry);
        if case_sensitive {
            self.cs.insert(key, idx);
        } else {
            self.ci.insert(key, idx);
        }
    }

    /// Two cities of one name in two countries: keep the first as the surface's
    /// answer, and record that the name is shared so the scan offers both.
    ///
    /// A city and its own country under one surface ("Singapore") are not a shared
    /// name — the city keeps it, exactly as before — and neither are two spellings
    /// of one city.
    fn link_namesake(&mut self, known: u32, base: u32) {
        let first = self.entries[known as usize];
        let second = self.entries[base as usize];
        if first.kind != Kind::City || second.kind != Kind::City || first.iso == second.iso {
            return;
        }
        let gid = match first.group {
            Some(gid) => gid,
            None => {
                let gid = self.groups.len() as u32;
                self.groups.push(vec![known]);
                self.entries[known as usize].group = Some(gid);
                gid
            }
        };
        let group = &mut self.groups[gid as usize];
        if !group.contains(&base) {
            group.push(base);
        }
    }

    /// One city: its own entry (the entry a shared name points at), then its name
    /// and `|`-separated aliases as surface forms. `CITIES` and `NAMESAKES` both
    /// come through here, so a shared name collides in `add()` and is linked.
    fn city(&mut self, name: &'static str, iso: &'static str, lat: f64, lon: f64, aliases: &str) {
        let base = self.entries.len() as u32;
        self.entries.push(Entry {
            kind: Kind::City,
            place: name,
            iso,
            lat,
            lon,
            adj: false,
            guard: None,
            pre: None,
            ambig: None,
            gov: None,
            group: None,
        });
        // First city of a name wins the anchor and any region alias: `NAMESAKES`
        // rows must not move a country's anchor off its capital.
        self.city_base.entry(name).or_insert(base);
        for surface in std::iter::once(name).chain(aliases.split('|')) {
            let surface = surface.trim();
            if surface.is_empty() {
                continue;
            }
            let cs = surface.chars().count() < 5;
            self.add(surface, base, false, cs);
        }
    }
}

fn build_index() -> Index {
    let mut b = Builder {
        entries: Vec::new(),
        iso: HashMap::new(),
        countries: HashMap::new(),
        city_base: HashMap::new(),
        ci: HashMap::new(),
        cs: HashMap::new(),
        groups: Vec::new(),
    };

    // The record per ISO code, in geodata order (the first for a code wins). A
    // record with no code is not keyed here: it is reached through its own entry.
    for &(name, iso, lat, lon, _aliases) in data::COUNTRIES {
        if is_iso_code(iso) {
            b.countries.entry(iso).or_insert(CountryRec { name, lat, lon });
        }
    }

    // 1. cities: the name first, then the "|" separated aliases.
    for &(name, iso, lat, lon, aliases) in data::CITIES {
        b.city(name, iso, lat, lon, aliases);
    }

    // 1b. the second city of a name two countries share. Read after `CITIES`, so
    //     the gazetteer's own order still decides which entry a shared surface
    //     points at; `add()` links the pair instead of dropping the newcomer.
    for &(name, iso, lat, lon, aliases) in data::NAMESAKES {
        b.city(name, iso, lat, lon, aliases);
    }

    // 2. districts and streets that hang off a city.
    for &(surface, city, mode) in data::REGION_ALIASES {
        if let Some(&base) = b.city_base.get(city) {
            b.add(surface, base, false, mode == "cs");
        }
    }

    // 3. country names and their aliases, in geodata order (first record per
    //    ISO code wins, exactly like `_country_table()`). A record with no ISO
    //    code gets its own entry, so two such places never merge.
    for &(name, iso, lat, lon, aliases) in data::COUNTRIES {
        let base = if is_iso_code(iso) {
            b.country(iso)
        } else {
            b.push_country(name, iso, lat, lon)
        };
        for surface in aliases {
            let surface = surface.trim();
            if surface.is_empty() {
                continue;
            }
            let adj = lookup_pair(data::ADJECTIVES, &surface.to_lowercase()).is_some();
            let cs = surface.chars().count() < 5;
            b.add(surface, base, adj, cs);
        }
    }

    // 4. surfaces that name a country rather than one of its cities.
    for &(surface, iso) in data::COUNTRY_ALIASES {
        let base = b.country(iso);
        let cs = surface.chars().count() < 5;
        b.add(surface, base, false, cs);
    }

    // 5. country adjectives: a weak hint, never a location on their own.
    for &(surface, iso) in data::ADJECTIVES {
        let base = b.country(iso);
        let cs = surface.chars().count() < 5;
        b.add(surface, base, true, cs);
    }

    // 6. localized names: substrings of the lowered text, longest surface first.
    let mut nonlatin: Vec<(String, u32, bool)> = Vec::new();
    for &(surface, kind, target) in data::NON_LATIN {
        let base = if kind == "city" {
            b.city_base.get(target).copied()
        } else {
            Some(b.country(target))
        };
        if let Some(base) = base {
            let (form, bounded) = nonlatin_form(surface);
            nonlatin.push((form.to_lowercase(), base, bounded));
        }
    }
    nonlatin.sort_by(|a, c| c.0.chars().count().cmp(&a.0.chars().count()));

    let mut first: HashSet<String> = HashSet::new();
    let mut maxrun = 1usize;
    for table in [&b.ci, &b.cs] {
        for key in table.keys() {
            let mut parts = key.split(' ');
            let head = match parts.next() {
                Some(h) => h,
                None => continue,
            };
            let mut count = 1usize;
            for _ in parts {
                count += 1;
            }
            if count > 1 {
                first.insert(head.to_lowercase());
                if count > maxrun {
                    maxrun = count;
                }
            }
        }
    }

    // Per-country anchor: the capital when the gazetteer has it, else the first
    // city listed for that country. `NAMESAKES` is deliberately not read here: a
    // namesake is a second city, and it must never become a country's anchor.
    let mut by_iso: HashMap<&'static str, Vec<(&'static str, f64, f64)>> = HashMap::new();
    for &(name, iso, lat, lon, _) in data::CITIES {
        by_iso.entry(iso).or_default().push((name, lat, lon));
    }
    let mut anchors: HashMap<&'static str, (f64, f64)> = HashMap::new();
    for (iso, cities) in &by_iso {
        let cap = lookup_pair(data::CAPITAL_CITY, iso);
        let pick = cap
            .and_then(|c| cities.iter().find(|city| city.0 == c))
            .or_else(|| cities.first());
        if let Some(city) = pick {
            anchors.insert(*iso, (city.1, city.2));
        }
    }

    Index {
        entries: b.entries,
        iso: b.iso,
        ci: b.ci,
        cs: b.cs,
        first,
        maxrun,
        nonlatin,
        anchors,
        countries: b.countries,
        groups: b.groups,
    }
}

fn nonlatin_form(surface: &'static str) -> (&'static str, bool) {
    if unspaced(surface) {
        return (surface, false);
    }
    if surface
        .chars()
        .all(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
    {
        if let Some(last) = surface.chars().next_back() {
            if CYR_VOWELS.contains(last) {
                return (&surface[..surface.len() - last.len_utf8()], false);
            }
        }
        return (surface, false);
    }
    (surface, true)
}

/// True for scripts that run words together and cannot be matched by boundary.
fn unspaced(surface: &str) -> bool {
    surface.chars().any(|c| {
        let o = c as u32;
        (0x3040..=0x30FF).contains(&o)
            || (0x3400..=0x9FFF).contains(&o)
            || (0xAC00..=0xD7AF).contains(&o)
            || (0x0E00..=0x0E7F).contains(&o)
    })
}

fn lookup_words(
    table: &'static [(&'static str, &'static [&'static str])],
    key: &str,
) -> Option<&'static [&'static str]> {
    table
        .binary_search_by(|(k, _)| (*k).cmp(key))
        .ok()
        .map(|i| table[i].1)
}

fn lookup_ambig(key: &str) -> Option<Ambig> {
    let table = data::AMBIGUOUS;
    table
        .binary_search_by(|(k, _, _, _)| (*k).cmp(key))
        .ok()
        .map(|i| {
            let &(_, cues, alt_cues, alt) = &table[i];
            Ambig {
                cues,
                alt_cues,
                alt,
            }
        })
}

fn lookup_gov(key: &str) -> Option<Gov> {
    let table = data::GOV_METONYMS;
    table
        .binary_search_by(|(k, _, _)| (*k).cmp(key))
        .ok()
        .map(|i| {
            let &(_, iso, cues) = &table[i];
            Gov { iso, cues }
        })
}
