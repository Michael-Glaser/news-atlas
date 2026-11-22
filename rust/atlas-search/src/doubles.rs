//! The doubles: a page that is a string, and the rules without their engines.
//!
//! Both exist so the suite needs no socket and no built binary. They are doubles,
//! not second implementations: everything a test asserts about behaviour is
//! asserted against the production path, and the fidelity tests
//! ([`crate::tests_engines`]) check the doubles against the real `atlas-text` /
//! `atlas-dates` whenever those are built.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::clock::{Clock, Fixed};
use crate::engine::Engine;
use crate::fetch::{Answer, Fetcher};
use crate::link;
use crate::rules::{Engines, Rules, Window};
use crate::search::Search;

/// NOW is a fixed Thursday: 2026-11-05T12:00:00Z.
pub const NOW: i64 = 1_793_880_000;

/// A transport that answers with one pasted page and records what it was asked.
pub struct Page {
    body: &'static str,
    asked: Mutex<Vec<String>>,
    status: u16,
}

impl Page {
    pub fn new(body: &'static str) -> Arc<Page> {
        Arc::new(Page {
            body,
            asked: Mutex::new(Vec::new()),
            status: 200,
        })
    }

    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("no panic").clone()
    }
}

impl Fetcher for Page {
    fn fetch(&self, url: &str, _max_bytes: usize) -> Answer {
        self.asked.lock().expect("no panic").push(url.to_string());
        Answer {
            status: self.status,
            final_url: url.to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            body: self.body.to_string(),
            ..Answer::default()
        }
    }
}

/// A transport that refuses everything (the gate said no).
pub struct Refused;

impl Fetcher for Refused {
    fn fetch(&self, url: &str, _max_bytes: usize) -> Answer {
        Answer {
            final_url: url.to_string(),
            refused: "private, loopback or link-local address is not allowed".to_string(),
            ..Answer::default()
        }
    }
}

/// The rules, without the engines: `atlas-text`'s documented answers, spelled out
/// so the suite is deterministic. It is a *double*, not a second implementation —
/// `norm_url` here does exactly what the engine's does for the shapes the
/// fixtures use, and the fidelity tests below check that against the binary.
#[derive(Default)]
pub struct Doubling {
    /// Every `norm_url` question, so a test can prove the rule was consulted.
    pub asked: Mutex<Vec<String>>,
}

impl Rules for Doubling {
    fn norm_url(&self, url: &str) -> String {
        self.asked.lock().expect("no panic").push(url.to_string());
        let raw = url.trim();
        let after = match raw.find("://") {
            Some(index) => &raw[index + 3..],
            None => raw,
        };
        let (authority, rest) = match after.find(['/', '?', '#']) {
            Some(index) => (&after[..index], &after[index..]),
            None => (after, ""),
        };
        let host = authority.rsplit('@').next().unwrap_or(authority).to_lowercase();
        let host = host.trim_end_matches(':').to_string();
        let path = match rest.find(['?', '#']) {
            Some(index) => &rest[..index],
            None => rest,
        };
        let mut path = path.trim_end_matches('/').to_string();
        if path.is_empty() {
            path.push('/');
        }
        let mut keep: Vec<&str> = Vec::new();
        if let Some(question) = rest.find('?') {
            let query = &rest[question + 1..];
            let query = match query.find('#') {
                Some(index) => &query[..index],
                None => query,
            };
            for pair in query.split('&') {
                let key = pair.split('=').next().unwrap_or("").to_lowercase();
                if key.starts_with("utm_") || matches!(key.as_str(), "fbclid" | "gclid" | "ref") {
                    continue;
                }
                keep.push(pair);
            }
        }
        keep.sort_unstable();
        let mut out = host;
        out.push_str(&path);
        if !keep.is_empty() {
            out.push('?');
            out.push_str(&keep.join("&"));
        }
        out
    }

    fn publisher(&self, url: &str) -> String {
        let host = link::host_of_url(url);
        let core = host
            .split('.')
            .filter(|label| !label.is_empty())
            .find(|label| !matches!(*label, "www" | "m" | "news" | "amp"))
            .unwrap_or("");
        let mut out = String::new();
        let mut start = true;
        for ch in core.chars() {
            if start {
                out.extend(ch.to_uppercase());
                start = false;
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn clean_title(&self, title: &str, publisher: &str) -> String {
        let mut out = title.trim().to_string();
        for separator in [" | ", " - ", " – ", " — ", " :: "] {
            if let Some(index) = out.rfind(separator) {
                let tail = out[index..].trim_start_matches(separator).trim().to_string();
                if tail.eq_ignore_ascii_case(publisher) && index > 0 {
                    out = out[..index].trim().to_string();
                }
            }
        }
        out
    }

    fn date_iso(&self, text: &str, now: i64) -> Option<String> {
        relative(text, now).map(stamp)
    }

    fn window(&self, text: &str, now: i64, max_age_s: i64) -> Option<Window> {
        let Some(epoch) = relative(text, now) else {
            return Some(Window {
                fresh: false,
                reason: "unparsed".to_string(),
                iso: String::new(),
                age_s: 0,
            });
        };
        let age = now - epoch;
        Some(Window {
            fresh: age <= max_age_s && age >= -120,
            reason: if age > max_age_s {
                "stale".to_string()
            } else if age < -120 {
                "skew".to_string()
            } else {
                "fresh".to_string()
            },
            iso: stamp(epoch),
            age_s: age.max(0),
        })
    }
}

/// The handful of spellings the fixtures use: a relative phrase, a German
/// numeric date and an ISO date. `atlas-dates` reads dozens more; this double
/// exists so a test can run on a machine with nothing built.
pub fn relative(text: &str, now: i64) -> Option<i64> {
    let text = text.trim().to_ascii_lowercase();
    for (suffix, seconds) in [
        ("hours ago", 3_600i64),
        ("hour ago", 3_600),
        ("days ago", 86_400),
        ("day ago", 86_400),
        ("minutes ago", 60),
    ] {
        if let Some(head) = text.strip_suffix(suffix) {
            let number: i64 = head.trim().parse().ok()?;
            return Some(now - number * seconds);
        }
    }
    // 05.11.2026 → 2026-11-05T00:00:00Z
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() == 3 {
        let day: i64 = parts[0].parse().ok()?;
        let month: i64 = parts[1].parse().ok()?;
        let year: i64 = parts[2].parse().ok()?;
        return Some(days_from_civil(year, month, day) * 86_400);
    }
    None
}

/// Howard Hinnant's civil-to-days, the standard arithmetic — a test double, not a
/// calendar rule the product uses.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn stamp(epoch: i64) -> String {
    // Only ever compared with itself in these tests, so the civil conversion is
    // the inverse of the one above and nothing else.
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn tool(body: &'static str) -> (Search, Arc<Page>, Arc<Doubling>) {
    tool_on(Engine::default(), body)
}

/// The same tool against another engine's surfaces.
pub fn tool_on(engine: Engine, body: &'static str) -> (Search, Arc<Page>, Arc<Doubling>) {
    let page = Page::new(body);
    let rules = Arc::new(Doubling::default());
    let search = Search::new(
        engine,
        rules.clone() as Arc<dyn Rules>,
        page.clone() as Arc<dyn Fetcher>,
    )
    .with_clock(Arc::new(Fixed(NOW)) as Arc<dyn Clock>);
    (search, page, rules)
}

/// The fixture engine: one plain HTML endpoint, two surfaces.
pub fn bing() -> Engine {
    Engine::new(
        "https://www.bing.com/search?q={query}",
        "https://www.bing.com/news/search?q={query}",
    )
    .expect("the fixture templates are valid")
}

pub fn rows(answer: &Value) -> Vec<Value> {
    answer["results"]
        .as_array()
        .expect("results is a list")
        .clone()
}

/// The row whose URL contains `marker`.
pub fn find<'a>(rows: &'a [Value], marker: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["url"].as_str().unwrap_or("").contains(marker))
        .unwrap_or_else(|| panic!("no row contains {}: {:?}", marker, rows))
}

pub fn pick<'a>(answer: &'a Value, key: &str) -> &'a str {
    answer.get(key).and_then(Value::as_str).unwrap_or("")
}

pub fn number(answer: &Value, key: &str) -> i64 {
    answer.get(key).and_then(Value::as_i64).unwrap_or(-1)
}

pub fn many(count: usize) -> String {
    let mut out = String::from("<html><body><div id=\"links\" class=\"results\">");
    for index in 0..count {
        out.push_str(&format!(
            "<div class=\"result\"><div class=\"links_main\">\
             <h2><a href=\"https://www.paper{index}.net/story/{index}\">Chip story number {index} about exports</a></h2>\
             <a class=\"snippet\" href=\"https://www.paper{index}.net/story/{index}\">Story {index} reports that chip exports moved again this week, in the {index} case.</a>\
             </div></div>",
            index = index
        ));
    }
    out.push_str("</div></body></html>");
    out
}

/// The workspace build, when it is there.
pub fn real() -> Option<Engines> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("target")
        .join("release");
    let engines = Engines::with_bin_dir(dir);
    if engines.check().is_ok() {
        Some(engines)
    } else {
        None
    }
}
