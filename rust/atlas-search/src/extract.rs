//! Cards to hits: resolve the link, unwrap the wrapper, gate it, drop the
//! duplicate, and read the date the page printed.
//!
//! The order of the five decisions is the point, and it is the order a link
//! travels: nothing here can hand the model a URL this crate did not resolve,
//! did not unwrap, and did not put in front of `atlas-net::gate`.
//!
//! 1. **resolve** — an `href` that is not an absolute http(s) target is not a
//!    result at all ([`crate::link::resolve`]).
//! 2. **unwrap** — the real link behind the engine's redirect wrapper, so the
//!    wrapper cannot smuggle an address past the gate.
//! 3. **navigation** — the engine's own host is the page's furniture, not a
//!    publisher ([`crate::link::engine_owns`]).
//! 4. **gate** — `atlas-net::gate`, which is `atlas-guard`'s SSRF verdict plus
//!    the service-port policy. A refused link is counted, never silently dropped:
//!    a search page must not be able to point this machine at itself, and the
//!    count says how often it tried.
//! 5. **duplicate** — `atlas-text`'s `norm_url` decides whether two links are the
//!    same page, so a card that appears twice (with and without `utm_source`)
//!    appears once. The coarse index in [`Seen`] only decides *when to ask*.
//!
//! Then, and only then, the row is built: publisher and headline from
//! `atlas-text`, and the date candidates the card carries
//! ([`crate::dates::candidates`]). Reading a date costs an engine call, so it is
//! the *caller* that decides when to spend it: [`fill_shown_dates`] for the rows
//! that will be returned, [`keep_current`] when a window has to be applied to
//! every candidate. Every string is cleaned and capped before it leaves — a
//! result page is untrusted input.

use std::collections::HashMap;

use url::Url;

use crate::cards::Card;
use crate::dates;
use crate::hit::Hit;
use crate::link;
use crate::rules::{Rules, Window};

/// How many date questions one call may ask the engine.
///
/// A page you do not control could print four hundred different labels; this is
/// the ceiling on what that costs. A real page uses a tenth of it, and the memo in
/// [`crate::rules`] means a repeated label is free — 40 cards that all say
/// "2 hours ago" cost one answer.
pub const MAX_DATE_LOOKUPS: usize = 40;

/// One hit on its way to an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub hit: Hit,
    /// Where a date could be written on this card ([`crate::dates`]). Reading it
    /// is a separate step, because it is the expensive one.
    pub dates: Vec<String>,
}

/// What one page yielded, and what it cost.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    pub candidates: Vec<Candidate>,
    /// Anchors that looked like a link to somewhere else.
    pub parsed: usize,
    /// The engine's own navigation.
    pub navigation: usize,
    /// An `href` that points nowhere useful.
    pub unusable: usize,
    /// Links `atlas-guard` refuses for safety.
    pub refused: usize,
    /// The same page, twice.
    pub duplicates: usize,
    /// The page showed no date this crate could read.
    pub undated: usize,
    /// The date was readable but not inside the window.
    pub out_of_window: usize,
}

impl Outcome {
    /// Everything that did not become a hit.
    pub fn dropped(&self) -> usize {
        self.navigation + self.unusable + self.refused + self.duplicates
    }
}

/// Turn a page's cards into hits.
pub fn extract(
    cards: &[Card],
    base: &str,
    engine_host: &str,
    rules: &dyn Rules,
) -> Outcome {
    let mut outcome = Outcome::default();
    let mut seen = Seen::default();
    for card in cards {
        let Some(raw) = link::resolve(&card.href, base) else {
            outcome.unusable += 1;
            continue;
        };
        outcome.parsed += 1;
        let url = link::unwrap(&raw).unwrap_or(raw);
        if link::engine_owns(&link::host(&url), engine_host) {
            outcome.navigation += 1;
            continue;
        }
        // The gate runs on the link the client would actually request.
        let verdict = atlas_net::gate(&link::href_of(&url));
        if !verdict.ok {
            outcome.refused += 1;
            continue;
        }
        if !seen.admit(verdict.url(), rules) {
            outcome.duplicates += 1;
            continue;
        }

        let publisher = crate::untrusted(&rules.publisher(verdict.url()), 60);
        let headline = crate::untrusted(&rules.clean_title(&card.title, &publisher), 300);
        let title = if headline.is_empty() {
            crate::untrusted(&card.title, 300)
        } else {
            headline
        };
        if title.is_empty() {
            outcome.unusable += 1;
            continue;
        }
        let snippet = crate::untrusted(&card.snippet, 400);
        let hit = Hit {
            title,
            url: verdict.url().to_string(),
            publisher,
            snippet,
            published: String::new(),
            printed: String::new(),
            age_s: -1,
            score: 0.0,
            page_rank: outcome.candidates.len() + 1,
        };

        outcome.candidates.push(Candidate {
            hit,
            dates: dates::candidates(&card.runs),
        });
    }
    outcome
}

/// Read the date on each card and show it, for the rows that are about to be
/// returned. A card with no readable date is left as it is: an undated result is
/// still a result, and the model can see the field is empty.
///
/// The date and its age are read in one question (`window`, with a century of
/// look-back so every readable timestamp is inside it) rather than two: a web
/// result that shows `published = "2026-11-05T10:00:00Z"` and no `age_s` while the
/// same card on the news surface shows both is a row the model has to interpret
/// differently for no reason. `age_s` means what [`crate::hit::Hit`] says it means:
/// seconds since that date, or -1 when there is no date.
///
/// At most [`MAX_DATE_LOOKUPS`] questions are asked, so a page cannot decide how
/// long this takes.
pub fn fill_shown_dates(candidates: &mut [Candidate], rules: &dyn Rules, now: i64) {
    /// A century of look-back: no result page prints a datestamp older than this,
    /// so the question is "what date is this", not "is it fresh".
    const ANY_AGE_S: i64 = 100 * 365 * 86_400;
    let mut asked = 0usize;
    for candidate in candidates.iter_mut() {
        if asked >= MAX_DATE_LOOKUPS {
            return;
        }
        for label in candidate.dates.clone() {
            if asked >= MAX_DATE_LOOKUPS {
                break;
            }
            asked += 1;
            let Some(window) = rules.window(&label, now, ANY_AGE_S) else {
                continue;
            };
            if window.unparsed() {
                continue;
            }
            candidate.hit.published = window.iso;
            candidate.hit.printed = label;
            candidate.hit.age_s = window.age_s;
            break;
        }
    }
}

/// What a window did to one page: what it kept, and the three counts that explain
/// the rest.
///
/// The three are separate because they mean different things to the model deciding
/// what to do next: `out_of_window` says the surface dates its results and this one
/// is old, `undated` says the surface does not date this card at all, and
/// `not_asked` says the page had more date labels than one call may read — nobody
/// looked, so it is not evidence about the page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Windowed {
    /// The cards inside the window, in page order.
    pub kept: Vec<Candidate>,
    /// The date was readable and older than the window.
    pub out_of_window: usize,
    /// The card carried no date this crate could read.
    pub undated: usize,
    /// The call ran out of [`MAX_DATE_LOOKUPS`] before it could decide this card's
    /// date. Not evidence about the page: the budget is this crate's, not the page's.
    pub not_asked: usize,
}

impl Windowed {
    /// Everything that did not become a kept candidate.
    pub fn dropped(&self) -> usize {
        self.out_of_window + self.undated + self.not_asked
    }
}

/// Keep only the cards whose date `atlas-dates` reads as inside the window that
/// reaches `max_age_s` back from `now`, and say how many were dropped for which
/// reason.
///
/// A card with no readable date is **not** kept: it cannot be shown to be current.
/// It is counted as `undated`, which is the model's evidence that the surface it
/// asked does not date its results. A card the date budget never reached is
/// counted as `not_asked` instead — the difference is whether the page or this
/// crate's budget is the reason.
pub fn keep_current(
    candidates: Vec<Candidate>,
    rules: &dyn Rules,
    now: i64,
    max_age_s: i64,
) -> Windowed {
    let mut windowed = Windowed {
        kept: Vec::with_capacity(candidates.len()),
        ..Windowed::default()
    };
    let mut asked = 0usize;
    for mut candidate in candidates {
        let mut verdict: Option<Window> = None;
        let mut exhausted = false;
        for label in candidate.dates.clone() {
            if asked >= MAX_DATE_LOOKUPS {
                exhausted = true;
                break;
            }
            asked += 1;
            match rules.window(&label, now, max_age_s) {
                // Readable: that is the card's date, inside the window or not.
                Some(window) if !window.unparsed() => {
                    candidate.hit.printed = label;
                    verdict = Some(window);
                    break;
                }
                // Not a date: the next candidate may be.
                _ => continue,
            }
        }
        match verdict {
            Some(window) if window.fresh => {
                candidate.hit.published = window.iso;
                candidate.hit.age_s = window.age_s;
                windowed.kept.push(candidate);
            }
            Some(_) => windowed.out_of_window += 1,
            // The budget, not the page: this card's date was never decided.
            None if exhausted => windowed.not_asked += 1,
            None => windowed.undated += 1,
        }
    }
    windowed
}

/// The duplicate index: a coarse key per link, and `atlas-text` called only where
/// two links could be the same page.
///
/// The guarantee is one-directional and that is deliberate: two links `norm_url`
/// calls equal always land in the same bucket here (the bucket ignores the query
/// and the trailing slash, the canonical form only narrows further), so the
/// confirmation is never skipped. Two links in the same bucket that `norm_url`
/// keeps apart stay apart.
#[derive(Debug, Default)]
pub struct Seen {
    buckets: HashMap<String, Vec<Entry>>,
}

#[derive(Debug, Clone)]
struct Entry {
    href: String,
    key: Option<String>,
}

impl Seen {
    /// Is this link a page that has not been seen yet? Asks `atlas-text` only
    /// when another link already sits in the same bucket.
    pub fn admit(&mut self, href: &str, rules: &dyn Rules) -> bool {
        let Ok(url) = Url::parse(href) else {
            return true;
        };
        let bucket = link::shadow_key(&url);
        let entries = self.buckets.entry(bucket).or_default();
        if entries.is_empty() {
            entries.push(Entry {
                href: href.to_string(),
                key: None,
            });
            return true;
        }
        let key = rules.norm_url(href);
        for entry in entries.iter_mut() {
            if entry.key.is_none() {
                entry.key = Some(rules.norm_url(&entry.href));
            }
            if !key.is_empty() && entry.key.as_deref() == Some(key.as_str()) {
                return false;
            }
        }
        entries.push(Entry {
            href: href.to_string(),
            key: Some(key),
        });
        true
    }
}

/// The URL without the query parameters that `atlas-text`'s canonical form
/// ignores.
///
/// No list of tracking parameters lives in this crate. The question is asked of
/// the rule itself, one parameter at a time: when the canonical form does not
/// change after a parameter is removed, that parameter was tracking and the
/// returned link is cleaner without it. A parameter that decides *which* page it
/// is — `?id=7` — changes the canonical form, so it stays.
///
/// The cost is bounded: at most eight parameters are asked about, and the memo in
/// [`crate::rules`] makes the repeats free.
pub fn untracked(href: &str, rules: &dyn Rules) -> String {
    /// More parameters than this and the question is not worth the processes.
    const MAX_PARAMS: usize = 8;
    let Ok(start) = Url::parse(href) else {
        return href.to_string();
    };
    if start.query().is_none() {
        return href.to_string();
    }
    let mut current = href.to_string();
    let key = rules.norm_url(&current);
    if key.is_empty() {
        return current;
    }
    let mut names: Vec<String> = start
        .query_pairs()
        .map(|(name, _)| name.to_string())
        .take(MAX_PARAMS)
        .collect();
    names.dedup();
    for name in names {
        let Ok(mut candidate) = Url::parse(&current) else {
            continue;
        };
        let kept: Vec<(String, String)> = candidate
            .query_pairs()
            .filter(|(key, _)| key != &name)
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        {
            let mut pairs = candidate.query_pairs_mut();
            pairs.clear();
            for (key, value) in &kept {
                pairs.append_pair(key, value);
            }
        }
        if kept.is_empty() {
            candidate.set_query(None);
        }
        let reduced = link::href_of(&candidate);
        // The rule decides: when the canonical form does not notice the parameter
        // went away, it was tracking and the link is cleaner without it.
        if rules.norm_url(&reduced) == key {
            current = reduced;
        }
    }
    current
}

/// A short, safe reason from a page — never page text.
pub fn reason_field(answer: &crate::fetch::Answer) -> String {
    if !answer.refused.is_empty() {
        return format!("the url gate refused it ({})", crate::scrub(&answer.refused));
    }
    if !answer.error.is_empty() {
        return format!("the request failed ({})", crate::scrub(&answer.error));
    }
    format!("the engine answered {} with no usable body", answer.status)
}

