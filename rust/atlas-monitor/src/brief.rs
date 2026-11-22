//! `brief` — a prepared summary the reader can open, and the bounded refresh that keeps
//! one from going stale.
//!
//! Decisions: the six scopes a briefing can be written for, what a payload has to look
//! like before a model call is paid for, the order a finished run refreshes in, and how
//! many model calls that refresh may spend.
//!
//! It does not: write the `briefings` table or prompt the model. The table is
//! `atlas-store`'s (`Store::briefing_get`), the pass is `atlas-ingest`'s
//! (`Pipeline::brief`), and both are another owner's file. This crate holds the seam
//! ([`Briefs`]) so the loop can be tested without a model, and so exactly one place
//! ([`crate::wiring::briefs_for`]) has to change when those two symbols land: today's
//! build answers [`NoBrief`], one sentence, instead of pretending to write a briefing.
//!
//! The scope words are the window's vocabulary, not a news vocabulary: `window` (the
//! whole reader), `country`, `place`, `topic`, `monitor`, `entity`. Nothing here filters
//! words or names a publisher.

use std::collections::BTreeMap;

use atlas_store::{SqlValue, Store};
use serde_json::{json, Value};

use crate::error::Result;

/// The six scopes a briefing can be written for. A closed set, because the window draws
/// one page per scope and the archive keeps one row per `(scope, key, hours)`.
pub const SCOPES: [&str; 6] = ["window", "country", "place", "topic", "monitor", "entity"];

/// The `window` scope has no key of its own — the reader asked for everything — and the
/// table spells that as the empty key (`atlas-store`'s `briefings` doc: `key` is `""` for
/// the window). One constant instead of an empty string spelled in three places.
pub const WINDOW: &str = "window";
pub const WINDOW_KEY: &str = "";

/// Model calls one finished run may spend keeping briefings fresh.
///
/// Three, because a pass that stored five stories across five countries would otherwise
/// turn one click into five more model calls the reader is waiting behind — and because
/// the ones it skips are still stale, so the next run picks them up. The window comes
/// first (it is the page the reader opens), then the countries that gained the most
/// stories, then the run's own topic or monitor.
pub const BRIEF_REFRESH: usize = 3;

/// Longest key a request may name, in bytes: an ISO code, a monitor id and a topic name
/// all fit with room to spare; a pasted article would not.
pub const MAX_KEY_BYTES: usize = 128;

/// The hours a briefing may cover. One hour is the narrowest useful slice, thirty days
/// the widest the archive's own window allows.
pub const MIN_HOURS: i64 = 1;
pub const MAX_HOURS: i64 = 720;

/// Longest `why` a request may carry. It is the click's own sentence; it is checked and
/// then dropped, because nothing on the action line quotes a reader back.
pub const MAX_WHY_CHARS: usize = 200;

/// One `brief` request's payload, validated: `{"scope","key","hours","why"}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub scope: String,
    pub key: String,
    pub hours: i64,
}

impl Ask {
    /// Read a `brief` payload, or answer the sentence that refused it.
    ///
    /// A refusal is final and says what to write instead: the queue stores a click's
    /// worth of intent, so a row it cannot run has to be readable by the window that
    /// wrote it.
    pub fn parse(payload: &Value) -> std::result::Result<Ask, String> {
        // A credential never reaches the model, the archive or a log — the same sentence
        // the queue's own door uses, so a reader sees one rule and not two.
        //
        // The scan reads the three values the window fills, not the JSON around them: the
        // guard's name-based check treats `"key": "…"` as a secret assigned to a field
        // called `key`, and *every* brief payload has that field. A token pasted into a
        // scope, a key or a `why` is still caught, and the row's own door scanned the raw
        // payload when it was pushed.
        let text = ["scope", "key", "why"]
            .iter()
            .filter_map(|field| payload.get(*field).and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        if atlas_guard::redact::carries_a_credential(&text)
            || atlas_guard::redact::credential_in(&text).is_some()
        {
            return Err(atlas_store::requests::CREDENTIAL_REFUSAL.to_string());
        }
        let scope = payload
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !SCOPES.contains(&scope.as_str()) {
            // Refused by name, whatever was written: a silent default would brief the
            // wrong page and look like it worked.
            let named = if scope.is_empty() {
                "a brief request needs a scope".to_string()
            } else {
                format!("\"{scope}\" is not a scope ATLAS briefs")
            };
            return Err(format!("{named}: {}", SCOPES.join(", ")));
        }
        let key = payload
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        // The window is the one scope without an address, and the table spells that as the
        // empty key. Every other scope needs one; a window with a key would be a row no
        // page reads, so it is refused rather than stored where nothing looks.
        if scope == WINDOW {
            if !key.is_empty() {
                return Err(format!(
                    "a window brief carries \"key\": \"\" — the window has no address of its \
                     own, and {key:?} would be a row no page reads"
                ));
            }
        } else if key.is_empty() {
            return Err(format!(
                "a {scope} brief needs a key — one ISO code, id or name"
            ));
        }
        if key.len() > MAX_KEY_BYTES {
            return Err(format!(
                "the {scope} brief's key is {} bytes; {MAX_KEY_BYTES} at most — nothing was asked",
                key.len()
            ));
        }
        if key.chars().any(char::is_control) {
            return Err(format!(
                "the {scope} brief's key carries a control character — nothing was asked"
            ));
        }
        let hours = match payload.get("hours").and_then(Value::as_i64) {
            Some(hours) if (MIN_HOURS..=MAX_HOURS).contains(&hours) => hours,
            Some(hours) => {
                return Err(format!(
                    "{hours} hours is outside the range a briefing covers \
                     ({MIN_HOURS}..={MAX_HOURS}) — nothing was asked"
                ))
            }
            None => {
                return Err(format!(
                    "a {scope} brief needs \"hours\" as a whole number \
                     ({MIN_HOURS}..={MAX_HOURS}) — nothing was asked"
                ))
            }
        };
        // Validated, then dropped: a `why` that does not fit a row on screen is a sign the
        // caller wrote something else into it, and the action line's set of keys is fixed.
        if let Some(why) = payload.get("why").and_then(Value::as_str) {
            if why.chars().count() > MAX_WHY_CHARS {
                return Err(format!(
                    "\"why\" is {} characters; {MAX_WHY_CHARS} at most — nothing was asked",
                    why.chars().count()
                ));
            }
        }
        Ok(Ask {
            scope,
            key,
            hours,
        })
    }
}

/// What one briefing costs and what it answers: the seam `atlas-ingest` fills.
pub trait Briefs: Send + Sync {
    /// Whether this build can brief at all. A build without the seam says so once per
    /// request instead of refreshing nothing at every run.
    fn ready(&self) -> bool;

    /// The newest article id the cached `(scope, key, hours)` briefing had read when it
    /// was written, or `None` when there is no row yet.
    fn watermark(&self, scope: &str, key: &str, hours: i64) -> Option<i64>;

    /// Write the briefing and answer its record:
    /// `{"ok": true, "stored": 1, …}` or `{"ok": false, "error": "<one sentence>"}`.
    fn brief(&self, scope: &str, key: &str, hours: i64) -> Value;
}

/// The seam a build without `Pipeline::brief` holds: every request is one sentence, and
/// the refresh does nothing rather than spend three model calls on a page it cannot write.
pub struct NoBrief {
    sentence: String,
}

impl NoBrief {
    pub fn new(sentence: impl Into<String>) -> NoBrief {
        NoBrief {
            sentence: sentence.into(),
        }
    }
}

impl Default for NoBrief {
    fn default() -> NoBrief {
        NoBrief::new(
            "this build has no briefing seam yet: the archive's briefings table and \
             atlas-ingest's Pipeline::brief are not in the tree",
        )
    }
}

impl Briefs for NoBrief {
    fn ready(&self) -> bool {
        false
    }

    fn watermark(&self, _scope: &str, _key: &str, _hours: i64) -> Option<i64> {
        None
    }

    fn brief(&self, _scope: &str, _key: &str, _hours: i64) -> Value {
        json!({ "ok": false, "error": self.sentence, "stored": 0 })
    }
}

/// The seam over the two halves another owner writes: the pass is
/// `atlas_ingest::Pipeline::brief` (one model call, the row written by it) and the cached
/// watermark is `atlas_store::Store::briefing_get` (the shelf).
pub struct PipelineBriefs {
    store: std::sync::Arc<Store>,
    pipeline: std::sync::Arc<atlas_ingest::Pipeline>,
}

impl PipelineBriefs {
    pub fn new(
        store: std::sync::Arc<Store>,
        pipeline: std::sync::Arc<atlas_ingest::Pipeline>,
    ) -> PipelineBriefs {
        PipelineBriefs { store, pipeline }
    }
}

impl Briefs for PipelineBriefs {
    fn ready(&self) -> bool {
        true
    }

    /// A row that does not parse answers `Ok(None)` in the store — which is exactly
    /// "no briefing yet" here, so the page is refreshed rather than trusted.
    fn watermark(&self, scope: &str, key: &str, hours: i64) -> Option<i64> {
        self.store
            .briefing_get(scope, key, hours)
            .ok()
            .flatten()
            .map(|row| row.watermark)
    }

    fn brief(&self, scope: &str, key: &str, hours: i64) -> Value {
        self.pipeline.brief(scope, key, hours)
    }
}

/// What the run that just finished added, as the refresh needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fresh {
    /// The article ids the pass stored, oldest first. The newest one is the watermark a
    /// briefing has to have reached to count as current.
    pub ids: Vec<i64>,
    /// How many of the new stories each country carries: `[("US", 3), ("DE", 2)]`.
    pub countries: Vec<(String, i64)>,
    /// The saved scope the run itself belongs to — `("monitor", "7")` or
    /// `("topic", "3")` — when the run has one. A bare `research` request is a question,
    /// not a saved scope, so it has none.
    pub own: Option<(String, String)>,
}

impl Fresh {
    /// The newest article id, which is the watermark a cached briefing must have reached.
    pub fn newest(&self) -> Option<i64> {
        self.ids.last().copied()
    }
}

/// What the archive gained while a pass ran, framed for [`refresh_after`].
///
/// The same rule `atlas-ingest`'s research answer uses for its `stored` list: what the
/// rows say, not what an event stream said about them. `since` is the newest article id
/// before the pass started.
pub fn fresh(store: &Store, since: i64, own: Option<(String, String)>) -> Result<Fresh> {
    let params = [SqlValue::Integer(since)];
    let ids: Vec<i64> = store
        .query("SELECT id FROM articles WHERE id > ? ORDER BY id", &params)?
        .iter()
        .filter_map(|row| row.int("id"))
        .collect();
    let countries = store
        .query(
            "SELECT iso, count(*) AS stories FROM article_countries \
             WHERE article_id > ? AND iso <> '' GROUP BY iso",
            &params,
        )?
        .iter()
        .filter_map(|row| {
            let iso = row.text("iso")?.to_string();
            let stories = row.int("stories").unwrap_or(0);
            Some((iso, stories))
        })
        .collect();
    Ok(Fresh {
        ids,
        countries,
        own,
    })
}

/// The newest article id in the archive, or `0` when it holds none.
///
/// A watermark and not a report: the research answer and the check log count what they
/// stored, and a count cannot say *which* stories a briefing has to have read before it
/// counts as current.
pub fn newest_article(store: &Store) -> i64 {
    store
        .query_one("SELECT max(id) AS newest FROM articles", &[])
        .ok()
        .flatten()
        .and_then(|row| row.int("newest"))
        .unwrap_or(0)
}

/// [`refresh_after`] with the archive read: what the run that just ended added, and what
/// that means for the reader's briefings.
///
/// The loop hands in the newest article id it saw *before* the pass, so the set is what
/// the rows gained — the same rule `atlas-ingest`'s research answer uses for `stored`.
pub fn refresh_run(
    store: &Store,
    briefs: &dyn Briefs,
    hours: i64,
    since: i64,
    own: Option<(String, String)>,
) -> Result<Vec<Visit>> {
    let fresh = fresh(store, since, own)?;
    Ok(refresh_after(briefs, hours, &fresh))
}

/// What happened to one scope the refresh looked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Visit {
    /// The model wrote it.
    Refreshed {
        scope: String,
        key: String,
        articles: i64,
    },
    /// The cached row already read the newest story, so nothing was spent.
    Current {
        scope: String,
        key: String,
        watermark: i64,
    },
    /// The pass answered `ok: false`. One log line for the caller, never a failed run.
    Failed {
        scope: String,
        key: String,
        error: String,
    },
    /// This build has no briefing seam; nothing was attempted.
    Unwired,
}

/// Keep the reader's briefings current after a run that stored at least one article.
///
/// The order is fixed, because the budget is small and the page the reader opens first is
/// the one that must be fresh:
///
/// 1. `window` — everything the reader can see;
/// 2. the countries of the new stories, busiest first (ties by ISO code, so two runs over
///    one archive refresh the same page rather than a coin-flip's worth);
/// 3. the run's own scope — its topic when the monitor watches one, else its monitor.
///
/// At most [`BRIEF_REFRESH`] model calls per run, and an attempt that fails has spent
/// one: a model that refuses three times must not become five calls on the next scope. A
/// scope whose cached watermark is not older than the newest article id is skipped for
/// free (no call), so a second run over unchanged stories costs nothing.
///
/// Nothing here fails a run: the caller turns [`Visit::Failed`] into one log line and
/// keeps the action's own outcome.
pub fn refresh_after(briefs: &dyn Briefs, hours: i64, fresh: &Fresh) -> Vec<Visit> {
    if !briefs.ready() {
        return vec![Visit::Unwired];
    }
    let Some(newest) = fresh.newest() else {
        return Vec::new();
    };
    let mut plan: Vec<(String, String)> = Vec::with_capacity(fresh.countries.len() + 2);
    plan.push((WINDOW.to_string(), WINDOW_KEY.to_string()));
    let mut busiest: BTreeMap<String, i64> = BTreeMap::new();
    for (iso, stories) in &fresh.countries {
        *busiest.entry(iso.to_uppercase()).or_insert(0) += stories;
    }
    let mut countries: Vec<(String, i64)> = busiest.into_iter().collect();
    countries.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    plan.extend(
        countries
            .into_iter()
            .map(|(iso, _stories)| ("country".to_string(), iso)),
    );
    if let Some(own) = &fresh.own {
        plan.push(own.clone());
    }
    let mut visits = Vec::new();
    let mut spent = 0usize;
    let mut seen: Vec<(String, String)> = Vec::new();
    for (scope, key) in plan {
        if spent >= BRIEF_REFRESH {
            break;
        }
        if seen.contains(&(scope.clone(), key.clone())) {
            continue;
        }
        seen.push((scope.clone(), key.clone()));
        match briefs.watermark(&scope, &key, hours) {
            Some(watermark) if watermark >= newest => visits.push(Visit::Current {
                scope,
                key,
                watermark,
            }),
            _ => {
                spent += 1;
                let answer = briefs.brief(&scope, &key, hours);
                let ok = answer.get("ok").and_then(Value::as_bool).unwrap_or(false);
                if ok {
                    visits.push(Visit::Refreshed {
                        scope,
                        key,
                        articles: answer.get("articles").and_then(Value::as_i64).unwrap_or(0),
                    })
                } else {
                    visits.push(Visit::Failed {
                        scope,
                        key,
                        error: answer
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("the briefing pass did not answer")
                            .to_string(),
                    })
                }
            }
        }
    }
    visits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::FakeBriefs;

    #[test]
    fn the_door_and_the_store_agree_on_the_widest_slot() {
        // The defect this pins: `atlas-ingest` clamped a briefing's hours to its own 72-hour
        // collection rule, so a 168-hour ask the door had accepted was stored in the 72-hour
        // slot and the seven-day tab read a slot nothing wrote. The door refuses past
        // [`MAX_HOURS`] and the pipeline clamps to `atlas_ingest::brief::BRIEF_MAX_H`; those
        // two numbers have to be one number, and an ask inside them has to arrive unchanged.
        assert_eq!(
            atlas_ingest::brief::BRIEF_MAX_H,
            MAX_HOURS,
            "the queue's door and the briefing's clamp are the same ceiling"
        );
        for hours in [24, 72, 168, MAX_HOURS] {
            let ask = Ask::parse(&json!({"scope": "window", "key": "", "hours": hours}))
                .expect("a window the door allows");
            assert_eq!(ask.hours, hours, "the door passes the ask through unchanged");
        }
    }

    #[test]
    fn a_payload_is_read_as_the_four_fields_the_window_writes() {
        let ask = Ask::parse(&json!({
            "scope": "Country", "key": " de ", "hours": 72, "why": "the reader asked"
        }))
        .expect("parse");
        assert_eq!(
            ask.scope, "country",
            "the scope is folded, the key only trimmed"
        );
        assert_eq!(ask.key, "de");
        assert_eq!(ask.hours, 72);
    }

    #[test]
    fn an_unknown_scope_is_refused_by_name() {
        let error = Ask::parse(&json!({"scope": "planet", "key": "mars", "hours": 24}))
            .expect_err("refused");
        assert!(error.contains("\"planet\""), "{error}");
        assert!(
            error.contains("window, country, place, topic, monitor, entity"),
            "{error}"
        );
        let empty = Ask::parse(&json!({"key": "mars", "hours": 24})).expect_err("refused");
        assert!(empty.contains("needs a scope"), "{empty}");
    }

    #[test]
    fn a_payload_carrying_a_credential_is_refused_with_the_queues_own_sentence() {
        let error = Ask::parse(&json!({
            "scope": "topic", "key": "chips", "hours": 24,
            "why": "my key is sk-live-9f8e7d6c5b4a3210"
        }))
        .expect_err("refused");
        assert_eq!(error, atlas_store::requests::CREDENTIAL_REFUSAL);
    }

    #[test]
    fn the_window_carries_no_key_and_every_other_scope_needs_one() {
        let window =
            Ask::parse(&json!({"scope": "window", "key": "", "hours": 72})).expect("parse");
        assert_eq!(window.key, "");
        let addressed = Ask::parse(&json!({"scope": "window", "key": "all", "hours": 72}))
            .expect_err("refused");
        assert!(addressed.contains("no address"), "{addressed}");
        let country =
            Ask::parse(&json!({"scope": "country", "key": "", "hours": 72})).expect_err("refused");
        assert!(country.contains("needs a key"), "{country}");
    }

    #[test]
    fn a_key_and_an_hours_outside_their_bounds_are_refused_not_clamped() {
        let long = "x".repeat(MAX_KEY_BYTES + 1);
        let error =
            Ask::parse(&json!({"scope": "topic", "key": long, "hours": 24})).expect_err("refused");
        assert!(error.contains("129 bytes"), "{error}");
        assert!(Ask::parse(&json!({"scope": "topic", "key": "", "hours": 24})).is_err());
        assert!(Ask::parse(&json!({"scope": "topic", "key": "a\tb", "hours": 24})).is_err());
        let hours = Ask::parse(&json!({"scope": "topic", "key": "chips", "hours": 0}))
            .expect_err("refused");
        assert!(hours.contains("1..=720"), "{hours}");
        assert!(Ask::parse(&json!({"scope": "topic", "key": "chips", "hours": 721})).is_err());
        assert!(Ask::parse(&json!({"scope": "topic", "key": "chips"})).is_err());
    }

    /// The fixture the order is documented for: one topic of the run, five countries, the
    /// busiest of them carrying three stories.
    fn five_countries() -> Fresh {
        Fresh {
            ids: vec![101, 102, 103, 104, 105],
            countries: vec![
                ("DE".to_string(), 1),
                ("US".to_string(), 3),
                ("FR".to_string(), 1),
                ("JP".to_string(), 1),
                ("BR".to_string(), 1),
            ],
            own: Some(("topic".to_string(), "7".to_string())),
        }
    }

    #[test]
    fn the_refresh_spends_three_calls_on_the_window_the_busiest_country_then_its_own_scope() {
        let briefs = FakeBriefs::recording();
        let visits = refresh_after(briefs.as_ref(), 72, &five_countries());
        assert_eq!(
            briefs.calls(),
            vec![
                ("window".to_string(), String::new()),
                ("country".to_string(), "US".to_string()),
                ("country".to_string(), "BR".to_string()),
            ],
            "three calls, in the documented order"
        );
        assert_eq!(visits.len(), 3);
        assert!(matches!(&visits[0], Visit::Refreshed { scope, .. } if scope == "window"));
    }

    #[test]
    fn a_scope_whose_watermark_is_current_is_skipped_and_the_budget_moves_on() {
        let briefs = FakeBriefs::recording();
        briefs.set_watermark("window", "", 72, 105);
        let visits = refresh_after(briefs.as_ref(), 72, &five_countries());
        assert_eq!(
            briefs.calls(),
            vec![
                ("country".to_string(), "US".to_string()),
                ("country".to_string(), "BR".to_string()),
                ("country".to_string(), "DE".to_string()),
            ],
            "the current window cost nothing, so the third call reached one scope further"
        );
        assert!(matches!(&visits[0], Visit::Current { watermark, .. } if *watermark == 105));
    }

    #[test]
    fn a_failed_briefing_is_one_visit_and_still_spends_its_call() {
        let briefs = FakeBriefs::recording();
        briefs.fail("window");
        let visits = refresh_after(briefs.as_ref(), 72, &five_countries());
        assert_eq!(briefs.calls().len(), 3, "an attempt spends the budget");
        assert!(matches!(&visits[0], Visit::Failed { scope, error, .. }
            if scope == "window" && error == "the model did not answer"));
    }

    #[test]
    fn the_run_that_stored_nothing_refreshes_nothing() {
        let briefs = FakeBriefs::recording();
        let empty = Fresh {
            own: Some(("topic".to_string(), "7".to_string())),
            ..Fresh::default()
        };
        assert_eq!(refresh_after(briefs.as_ref(), 72, &empty), Vec::new());
        assert!(briefs.calls().is_empty());
    }

    #[test]
    fn a_build_without_the_seam_answers_once_and_costs_nothing() {
        let visits = refresh_after(&NoBrief::default(), 72, &five_countries());
        assert_eq!(visits, vec![Visit::Unwired]);
        let answer = NoBrief::default().brief("window", "", 72);
        assert_eq!(answer["ok"], json!(false));
        assert!(answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("briefings table"));
    }

    #[test]
    fn the_runs_own_scope_is_reached_when_the_countries_are_few() {
        let briefs = FakeBriefs::recording();
        let fresh = Fresh {
            ids: vec![9],
            countries: vec![("DE".to_string(), 1)],
            own: Some(("topic".to_string(), "7".to_string())),
        };
        let visits = refresh_after(briefs.as_ref(), 72, &fresh);
        assert_eq!(
            briefs.calls(),
            vec![
                ("window".to_string(), String::new()),
                ("country".to_string(), "DE".to_string()),
                ("topic".to_string(), "7".to_string()),
            ],
            "window, the country, then the run's own scope"
        );
        assert_eq!(visits.len(), 3);
    }

    #[test]
    fn fresh_reads_what_the_archive_gained_and_the_country_of_each_story() {
        let store = crate::fixtures::store();
        crate::fixtures::insert_article(&store, 7, "2026-11-22T08:00:00Z", "an older story", true);
        crate::fixtures::insert_country(&store, 7, "US");
        crate::fixtures::insert_article(&store, 8, "2026-11-22T09:00:00Z", "a new story", true);
        crate::fixtures::insert_country(&store, 8, "DE");
        crate::fixtures::insert_country(&store, 8, "FR");
        let fresh = fresh(&store, 7, None).expect("fresh");
        assert_eq!(fresh.ids, vec![8], "only what the run added");
        assert_eq!(fresh.newest(), Some(8));
        let mut countries = fresh.countries.clone();
        countries.sort();
        assert_eq!(
            countries,
            vec![("DE".to_string(), 1), ("FR".to_string(), 1)],
            "the country of each new story, counted"
        );
    }
}
