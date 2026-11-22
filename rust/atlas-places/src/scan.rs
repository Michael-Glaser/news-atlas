//! The text scan: what one zone (title, summary or body) says about places.
//!
//! Decides: how a zone is split into word runs, which run sequences form a
//! name, when a guard or a pre-guard voids a hit, how far around a hit the cue
//! words are looked for, which localized names are found as substrings, and how
//! much one mention is worth before ranking (a plain mention, an adjective, an
//! ambiguous name with or without its supporting cue, a capital metonym
//! resolved to its government).
//!
//! Does not decide: how zones combine into a ranked list, what the confidence
//! is, or what the JSON looks like. That is `crate::score` and `crate::report`.
//!
//! One pass over the zone: no regex engine, no backtracking, no second sweep of
//! the text, and the lowered cue window is computed at most once per hit.

use std::collections::HashMap;

use crate::index::{Index, Kind};
use crate::phrase::{lower_into, occurs_as_words};
use crate::score::{ADJ_WEIGHT, AMBIG_WEIGHT};

/// Characters searched around a hit for context words.
const CUE_WINDOW: usize = 180;

/// A phrase may only bridge whitespace and the punctuation headlines use inside
/// names ("Washington, D.C.", "Tel-Aviv", "N'Djamena").
const GAP: [char; 8] = [' ', '\t', ',', '.', '-', '\'', '\u{2019}', '\u{2013}'];


// ---------------------------------------------------------------------------
// cue windows and word boundaries
// ---------------------------------------------------------------------------

/// Char index -> byte offset, so the cue window can clip at the same boundaries
/// as `text[start - 180:end + 180]` does in Python.
struct CharMap<'a> {
    text: &'a str,
    offs: Option<Vec<usize>>,
}

impl<'a> CharMap<'a> {
    fn new(text: &'a str) -> CharMap<'a> {
        let offs = if text.is_ascii() {
            None
        } else {
            let mut v: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
            v.push(text.len());
            Some(v)
        };
        CharMap { text, offs }
    }

    fn total(&self) -> usize {
        match &self.offs {
            None => self.text.len(),
            Some(v) => v.len() - 1,
        }
    }

    fn byte(&self, i: usize) -> usize {
        match &self.offs {
            None => i.min(self.text.len()),
            Some(v) => *v.get(i).unwrap_or(&self.text.len()),
        }
    }

    fn slice(&self, from: usize, to: usize) -> &'a str {
        let from = self.byte(from);
        let to = self.byte(to.min(self.total()));
        if from >= to {
            ""
        } else {
            &self.text[from..to]
        }
    }
}

/// Does any cue word appear in `hay` on word boundaries? The boundary rule is
/// `crate::phrase`'s: the publisher table asks the same question of a name.
fn has_cue(hay: &str, cue: &str) -> bool {
    occurs_as_words(hay, cue)
}

fn has_any_cue(hay: &str, cues: &'static [&'static str]) -> bool {
    cues.iter().any(|c| has_cue(hay, c))
}

// ---------------------------------------------------------------------------
// scanning one zone
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Tok {
    start: usize,
    end: usize,
    pos: usize,
    end_pos: usize,
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::with_capacity(text.len() / 8 + 4);
    let mut pos = 0usize;
    let mut cur: Option<(usize, usize)> = None;
    for (b, c) in text.char_indices() {
        if c.is_alphanumeric() {
            if cur.is_none() {
                cur = Some((b, pos));
            }
        } else if let Some((start, start_pos)) = cur.take() {
            out.push(Tok {
                start,
                end: b,
                pos: start_pos,
                end_pos: pos,
            });
        }
        pos += 1;
    }
    if let Some((start, start_pos)) = cur.take() {
        out.push(Tok {
            start,
            end: text.len(),
            pos: start_pos,
            end_pos: pos,
        });
    }
    out
}

/// True when tokens `i..i+run-1` are only separated by name punctuation.
fn gap_ok(text: &str, toks: &[Tok], i: usize, run: usize) -> bool {
    for j in i..i + run - 1 {
        let gap = &text[toks[j].end..toks[j + 1].start];
        let mut count = 0usize;
        for c in gap.chars() {
            count += 1;
            if count > 3 || !GAP.contains(&c) {
                return false;
            }
        }
    }
    true
}

/// One place as it appears in one zone: `{(kind, place, iso): slot}`.
///
/// The ISO code is part of the key because a name two countries share
/// ("Valencia") is two places: one slot per city, never one merged slot.
#[derive(Clone, Copy)]
pub(crate) struct Slot {
    pub(crate) kind: Kind,
    pub(crate) lat: f64,
    pub(crate) lon: f64,
    pub(crate) n: usize,
    pub(crate) w: f64,
    pub(crate) pw: f64,
    pub(crate) adj: usize,
    pub(crate) pos: usize,
}

pub(crate) type Hits = HashMap<(u8, &'static str, &'static str), Slot>;

/// `_hit`: resolve one mention to (place entry, weight) after its context checks.
fn hit(idx: &Index, entry_idx: u32, window: &mut WindowCache) -> (Option<u32>, f64) {
    let entry = &idx.entries[entry_idx as usize];
    if let Some(rules) = entry.ambig {
        let win = window.get();
        if has_any_cue(win, rules.alt_cues) {
            return match rules.alt {
                Some(iso) => match idx.iso.get(iso) {
                    Some(&alt) => (Some(alt), 1.0),
                    None => (None, 0.0),
                },
                None => (None, 0.0),
            };
        }
        let weight = if has_any_cue(win, rules.cues) {
            1.0
        } else {
            AMBIG_WEIGHT
        };
        return (Some(entry_idx), weight);
    }
    if let Some(gov) = entry.gov {
        if let Some(&alt) = idx.iso.get(gov.iso) {
            if has_any_cue(window.get(), gov.cues) {
                return (Some(alt), 1.0);
            }
        }
        return (Some(entry_idx), 1.0);
    }
    (Some(entry_idx), if entry.adj { ADJ_WEIGHT } else { 1.0 })
}

/// The lowered cue window of the current hit, computed at most once per hit.
struct WindowCache<'a, 'm> {
    map: &'m CharMap<'a>,
    start: usize,
    end: usize,
    value: Option<String>,
}

impl<'a, 'm> WindowCache<'a, 'm> {
    fn get(&mut self) -> &str {
        if self.value.is_none() {
            let from = self.start.saturating_sub(CUE_WINDOW);
            let to = self.end + CUE_WINDOW;
            self.value = Some(self.map.slice(from, to).to_lowercase());
        }
        self.value.as_deref().unwrap_or("")
    }
}

fn add_hit(out: &mut Hits, idx: &Index, entry_idx: u32, weight: f64, pos: usize) {
    if weight <= 0.0 {
        return;
    }
    let entry = &idx.entries[entry_idx as usize];
    // The key carries the ISO code, so the slot does not need to.
    let key = (entry.kind.as_u8(), entry.place, entry.iso);
    let slot = out.entry(key).or_insert(Slot {
        kind: entry.kind,
        lat: entry.lat,
        lon: entry.lon,
        n: 0,
        w: 0.0,
        pw: 0.0,
        adj: 0,
        pos,
    });
    slot.n += 1;
    slot.w += weight;
    if weight >= 1.0 {
        slot.pw += weight;
    } else if entry.adj {
        slot.adj += 1;
    }
    if pos < slot.pos {
        slot.pos = pos;
    }
}

/// `_scan`: one text zone.
pub(crate) fn scan(text: &str, idx: &Index) -> Hits {
    let mut out: Hits = HashMap::new();
    if text.is_empty() {
        return out;
    }
    let toks = tokenize(text);
    let count = toks.len();
    let map = CharMap::new(text);
    let mut low0 = String::new();
    let mut key_low = String::new();
    let mut key_raw = String::new();
    let mut low_next = String::new();

    let mut i = 0usize;
    while i < count {
        let tok = toks[i];
        let raw = &text[tok.start..tok.end];
        low0.clear();
        lower_into(&mut low0, raw);

        let mut entry: Option<u32> = None;
        let mut step = 1usize;
        let mut end = tok.end_pos;

        if idx.first.contains(low0.as_str()) {
            let mut run = idx.maxrun.min(count - i);
            while run > 1 {
                if gap_ok(text, &toks, i, run) {
                    key_low.clear();
                    key_raw.clear();
                    for k in 0..run {
                        if k > 0 {
                            key_low.push(' ');
                            key_raw.push(' ');
                        }
                        let t = toks[i + k];
                        lower_into(&mut key_low, &text[t.start..t.end]);
                        key_raw.push_str(&text[t.start..t.end]);
                    }
                    entry = idx
                        .ci
                        .get(key_low.as_str())
                        .or_else(|| idx.cs.get(key_raw.as_str()))
                        .copied();
                    if entry.is_some() {
                        step = run;
                        end = toks[i + run - 1].end_pos;
                        break;
                    }
                }
                run -= 1;
            }
        }
        if entry.is_none() {
            entry = idx
                .cs
                .get(raw)
                .or_else(|| idx.ci.get(low0.as_str()))
                .copied();
        }
        let entry_idx = match entry {
            Some(e) => e,
            None => {
                i += 1;
                continue;
            }
        };

        // An outlet name is not a dateline.
        if let Some(guard) = idx.entries[entry_idx as usize].guard {
            if i + step < count {
                let next = toks[i + step];
                low_next.clear();
                lower_into(&mut low_next, &text[next.start..next.end]);
                if guard.iter().any(|w| *w == low_next.as_str()) {
                    i += 1;
                    continue;
                }
            }
        }
        // "South America" is a continent, "New Mexico" is a US state.
        if let Some(pre) = idx.entries[entry_idx as usize].pre {
            if i > 0 {
                let prev = toks[i - 1];
                low_next.clear();
                lower_into(&mut low_next, &text[prev.start..prev.end]);
                if pre.iter().any(|w| *w == low_next.as_str()) {
                    i += 1;
                    continue;
                }
            }
        }

        let mut window = WindowCache {
            map: &map,
            start: tok.pos,
            end,
            value: None,
        };
        let (resolved, weight) = hit(idx, entry_idx, &mut window);
        if let Some(resolved) = resolved {
            add_hit(&mut out, idx, resolved, weight, tok.pos);
        }
        // A name several countries use: every city that carries it is a candidate,
        // one slot each. Which one the text means needs the whole text, so that
        // decision waits for `crate::namesake`.
        if let Some(group) = idx.entries[entry_idx as usize].group {
            for &sibling in &idx.groups[group as usize] {
                if sibling == entry_idx {
                    continue;
                }
                let (resolved, weight) = hit(idx, sibling, &mut window);
                if let Some(resolved) = resolved {
                    add_hit(&mut out, idx, resolved, weight, tok.pos);
                }
            }
        }
        i += step;
    }

    scan_other_scripts(text, &mut out, idx);
    out
}

/// `_scan_other_scripts`: localized names, substrings of the lowered text,
/// longest surface first; a shorter surface inside a longer hit does not fire.
fn scan_other_scripts(text: &str, out: &mut Hits, idx: &Index) {
    if text.is_ascii() {
        return;
    }
    let low = text.to_lowercase();
    let mut used = vec![false; low.len()];
    for (surface, entry, bounded) in &idx.nonlatin {
        let mut from = 0usize;
        // Char index of `at`, advanced as we go: `find` returns byte offsets
        // while a hit's `pos` is a character index, and walking forward from the
        // previous hit of this surface keeps that a single pass.
        let mut seen_byte = 0usize;
        let mut seen_chars = 0usize;
        while let Some(rel) = low[from..].find(surface.as_str()) {
            let at = from + rel;
            let stop = at + surface.len();
            if *bounded && stop < low.len() {
                let next = low[stop..].chars().next().unwrap_or(' ');
                if next.is_alphanumeric() {
                    from = at + low[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    if from >= low.len() {
                        break;
                    }
                    continue;
                }
            }
            if !used[at..stop].iter().any(|&b| b) {
                for b in &mut used[at..stop] {
                    *b = true;
                }
                while seen_byte < at {
                    seen_byte += low[seen_byte..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    seen_chars += 1;
                }
                add_hit(out, idx, *entry, 1.0, seen_chars);
            }
            from = at + low[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            if from >= low.len() {
                break;
            }
        }
    }
}
