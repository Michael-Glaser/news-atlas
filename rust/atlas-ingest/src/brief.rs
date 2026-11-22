//! `brief` — the digest of the stories, written once by the model and kept.
//!
//! The reader's question is "was ist da so?" — for a country, a place, a topic, a monitor,
//! an entity or the whole window. Answering it by asking the model again on every frame
//! would be a call per repaint; answering it from a fixed template would be a summary of
//! nothing. So the digest is built here, once, from the stories the archive already holds,
//! and the answer is one row in `briefings` that the window draws.
//!
//! # One digest, one call (at most two)
//!
//! Nothing in this file loops. The steps are: read the stories, build one text block, ask
//! the model once, and — only when the answer was not the JSON object that was asked for —
//! ask once more with "answer with the JSON object only". Then either the row is written or
//! the answer is a refusal with one sentence. There is no retry loop, no timer and no
//! background job, exactly like [`crate::research`].
//!
//! # What it does not decide
//!
//! No word list, no publisher list, no query of its own. Which stories belong to a scope is
//! one SQL statement per scope over the columns and join tables the archive already keeps
//! ([`Pipeline::brief_stories`] names the grouping each one mirrors), and what the stories
//! *mean* is the model's answer. The prompt, the shape of the answer and the caps on it are
//! `atlas-ai`'s ([`atlas_ai::brief`]); this file joins them to the archive.
//!
//! # Untrusted text
//!
//! A title or a summary is a string a page chose. Each field goes through
//! [`atlas_ai::prompts::field_line`] (collapsed, cut, defanged) and the whole list is fenced
//! by [`atlas_ai::prompts::data_block`], so a story that prints a fence marker or an
//! instruction cannot speak to the model outside the data block.

use std::time::Instant;

use serde_json::{json, Value};

use atlas_store::{Briefing, SqlValue};

use crate::Pipeline;

/// The most stories one briefing reads, newest first.
pub const BRIEF_STORIES: usize = 40;

/// The longest window a briefing may cover, in hours.
///
/// The **slot** is the ask: `briefings` is keyed by `(scope, key, hours)`, and the window shows
/// the reader a tab per window ([`24`, `72`, `168`] hours) and asks for each one. Clamping that
/// number to the ingest's own 72-hour ceiling (`MAX_AGE_H`, the freshness rule for *collecting*)
/// wrote a 168-hour ask into the 72-hour slot: request id 6 came back `done`, with no error, and
/// the table had no `window//168` row at all — so the seven-day tab read a slot nothing ever
/// writes and said "no briefing yet" forever.
///
/// 720 hours is the queue's own ceiling for a briefing ask (`atlas-monitor`'s `Ask::parse`
/// refuses past it), so the store's slot and the door agree; the model still reads at most
/// [`BRIEF_STORIES`] stories and [`BRIEF_CHARS`] characters of digest, whatever the window.
pub const BRIEF_MAX_H: i64 = 720;

/// The most characters the story digest may take.
///
/// 24 000 characters is roughly 6 000 tokens of English, which fits the default 32 768-token
/// window beside the prompt and 8 192 tokens of thinking with room to spare. The digest is
/// cut here rather than at the model's context: a list that does not fit is a list whose
/// tail was never going to be read.
pub const BRIEF_CHARS: usize = 24_000;

/// Characters a title may take in one digest line.
pub const TITLE_CHARS: usize = 120;

/// Characters a summary may take in one digest line.
pub const SUMMARY_CHARS: usize = 220;

/// Characters an outlet name may take in one digest line.
pub const SOURCE_CHARS: usize = 60;

/// Characters the date may take: an ISO stamp, and never a reformat of one.
pub const PUBLISHED_CHARS: usize = 25;

/// The bullets one briefing asks for (the prompt clamps them to `atlas_ai::brief::BULLET_RANGE`).
pub const BULLETS: usize = 5;

/// The six scopes a briefing can be about, in the order a reader meets them.
pub const SCOPES: [&str; 6] = ["window", "country", "place", "topic", "monitor", "entity"];

/// The one line a corrective re-ask adds, verbatim: the JSON object and nothing around it.
pub const ONLY_JSON: &str = "Answer with the JSON object only, no prose.";

/// One story, as a digest line needs it. No body: a briefing is about what is there.
pub(crate) struct Story {
    pub(crate) id: i64,
    pub(crate) published: String,
    pub(crate) source: String,
    pub(crate) title: String,
    pub(crate) summary: String,
}

impl Pipeline {
    /// Write the briefing for one scope and store it. One model call, plus one correction.
    ///
    /// `scope` is one of [`SCOPES`]; `key` is what the scope is about (an ISO code, a place,
    /// a topic id or name, a monitor id or name, an entity id or name, and nothing for the
    /// window); `hours` is how far back the reader is looking. The answer is the JSON the
    /// queue and the window speak: `{"ok":true,…,"stored":1}` when a row was written, and
    /// `{"ok":false,"error":"<one sentence>"}` when it was not — never a row about no stories.
    pub fn brief(&self, scope: &str, key: &str, hours: i64) -> Value {
        let started = Instant::now();
        let scope = scope.trim().to_lowercase();
        let key = key.trim();
        if !SCOPES.contains(&scope.as_str()) {
            return refused(format!(
                "unknown briefing scope {:?}: it has to be one of {}",
                scope,
                SCOPES.join(", ")
            ));
        }
        // One sentence, and it names where the model was expected: a reader whose briefing
        // did not arrive because the server is down has been told something they can act on.
        if !self.model().online() {
            return refused(format!(
                "the local model is not answering on {}, so there is no briefing: start it \
                 (`atlas start`) and ask again",
                crate::endpoint_string()
            ));
        }
        // The ask *is* the slot: `briefings` is keyed by the hours, so a clamp here does not
        // shorten a summary, it writes a different slot than the one the window reads back
        // ([`BRIEF_MAX_H`]). Clamped to the door's own ceiling only, which `atlas-monitor`
        // already refuses above.
        let hours = hours.clamp(1, BRIEF_MAX_H);
        // Read **before** the digest is built: the watermark is "what the archive held when
        // this briefing was asked for", and the window compares it with what arrives next.
        let watermark = self.article_watermark();
        let stories = match self.brief_stories(&scope, key, hours) {
            Ok(stories) => stories,
            Err(sentence) => return refused(sentence),
        };
        if stories.is_empty() {
            return refused(format!(
                "no story in the last {hours} hours is in this {scope} ({}), so there is \
                 nothing to brief yet: collect some first, then ask again",
                if key.is_empty() { "the whole window" } else { key }
            ));
        }
        let (digest, count) = brief_digest(&stories, BRIEF_CHARS);
        let budgets = self.budgets();
        let prompt = format!(
            "{}\n\n{}\n\n{}",
            atlas_ai::brief::BRIEF_PROMPT,
            atlas_ai::brief::digest_instruction(BULLETS),
            atlas_ai::prompts::data_block(&digest, "STORIES", BRIEF_CHARS)
        );
        let text = match self.model().brief(&prompt, &budgets) {
            Ok(text) => text,
            Err(reason) => {
                return refused(format!(
                    "the local model could not answer the briefing ({}), so nothing was stored",
                    crate::scrub(&reason)
                ))
            }
        };
        let written = match atlas_ai::brief::parse(&text) {
            Some(written) => written,
            // One correction, one sentence, no echo of the answer: a model that wrapped the
            // object in prose answers again, and a model that cannot is a refusal a reader
            // can act on instead of a blank panel.
            None => {
                let corrective = format!("{prompt}\n\n{ONLY_JSON}");
                let second = match self.model().brief(&corrective, &budgets) {
                    Ok(text) => text,
                    Err(reason) => {
                        return refused(format!(
                            "the local model could not answer the briefing ({}), so nothing \
                             was stored",
                            crate::scrub(&reason)
                        ))
                    }
                };
                match atlas_ai::brief::parse(&second) {
                    Some(written) => written,
                    None => {
                        return refused(format!(
                            "the local model answered twice without the json object a \
                             briefing is ({} line and bullets), so nothing was stored",
                            count
                        ))
                    }
                }
            }
        };
        let ms = started.elapsed().as_millis() as i64;
        let row = Briefing {
            scope: scope.clone(),
            key: key.to_string(),
            hours,
            watermark,
            created: self.now_iso(),
            line: written.line.clone(),
            bullets: written.bullets.clone(),
            articles: count as i64,
            model: self.model().model_id(),
            ms,
        };
        if let Err(sentence) = self.brief_store(&row) {
            return refused(sentence);
        }
        json!({
            "ok": true,
            "scope": row.scope,
            "key": row.key,
            "hours": row.hours,
            "line": row.line,
            "bullets": row.bullets,
            "articles": row.articles,
            "watermark": row.watermark,
            "model": row.model,
            "ms": row.ms,
            "stored": 1,
        })
    }

    /// The stories one scope is about, newest first, at most [`BRIEF_STORIES`] of them.
    ///
    /// Each arm is one statement over the archive's own columns and join tables, and each is
    /// the statement the window reads for the same page — same predicate, same order, same
    /// window — so a briefing cannot disagree with the list beside it:
    ///
    /// | scope | the statement it mirrors (`atlas-desktop/src/data.rs`) |
    /// | --- | --- |
    /// | `window` | `articles` (`:474`) — the window rule, `published DESC, id DESC` |
    /// | `country` | `country_stories` (`:672`) — `a.iso` **or** an `article_countries` row: the union the chip's number counts |
    /// | `place` | `place_stories` (`:621`) — the `PLACE_LABEL` expression ([`PLACE_LABEL`]), not the bare column |
    /// | `topic` | `topic_articles` (`:915`) — the `article_topics` join |
    /// | `monitor` | `monitor_stories` (`:986`) — the `monitor_seen` mark `'a'||id` **or** an `alerts` row |
    /// | `entity` | no window list of stories exists (the graph draws nodes, not stories); `article_entities`, the archive's own answer |
    ///
    /// The rule behind this table is the bug that produced it: a briefing for ITA was refused
    /// — "no story … is in this country (ITA)" — while the window's ITA chip listed stories,
    /// because the chip counts `articles.iso ∪ article_countries.iso` and this read the column
    /// alone. A door has to open the list the number beside it counts.
    fn brief_stories(&self, scope: &str, key: &str, hours: i64) -> Result<Vec<Story>, String> {
        let cutoff = self.iso_ago(hours);
        let limit = SqlValue::Integer(BRIEF_STORIES as i64);
        let window = SqlValue::Text(cutoff);
        let rows = match scope {
            "window" => self.store().query(
                &format!("{SELECT} WHERE {FRESH} ORDER BY a.published DESC, a.id DESC LIMIT ?"),
                &[window, limit],
            ),
            "country" => {
                if key.is_empty() {
                    return Err(
                        "a country briefing needs the country's ISO code, for example \"DEU\""
                            .to_string(),
                    );
                }
                // `?` binds the code twice, then the cutoff, then the limit: the text order
                // of the statement, which is the order `Store::query` binds in.
                let code = SqlValue::Text(key.to_string());
                self.country_rows(code, window, limit)
            }
            "place" => {
                if key.is_empty() {
                    return Err(
                        "a place briefing needs the place as the archive stores it, for \
                         example \"Berlin\""
                            .to_string(),
                    );
                }
                self.store().query(
                    &format!(
                        "{SELECT} WHERE {PLACE_LABEL} = ? AND {FRESH} \
                         ORDER BY a.published DESC, a.id DESC LIMIT ?"
                    ),
                    &[SqlValue::Text(key.to_string()), window, limit],
                )
            }
            "topic" => {
                let id = self.brief_topic(key)?;
                self.store().query(
                    &format!(
                        "{SELECT} JOIN article_topics t ON t.article_id = a.id AND t.topic_id = ? \
                         WHERE {FRESH} ORDER BY a.published DESC, a.id DESC LIMIT ?"
                    ),
                    &[SqlValue::Integer(id), window, limit],
                )
            }
            "monitor" => {
                let id = self.brief_monitor(key)?;
                // The mark's key is the tool protocol's (`'a' || article id`), not a column:
                // a mark of any other shape names a page that never became a story.
                self.store().query(
                    &format!(
                        "{SELECT} WHERE {FRESH} AND (EXISTS (SELECT 1 FROM monitor_seen m \
                           WHERE m.monitor_id = ? AND m.key = 'a' || a.id) \
                         OR EXISTS (SELECT 1 FROM alerts al \
                           WHERE al.monitor_id = ? AND al.article_id = a.id)) \
                         ORDER BY a.published DESC, a.id DESC LIMIT ?"
                    ),
                    &[window, SqlValue::Integer(id), SqlValue::Integer(id), limit],
                )
            }
            "entity" => {
                let id = self.brief_entity(key)?;
                self.store().query(
                    &format!(
                        "{SELECT} JOIN article_entities e ON e.article_id = a.id AND e.entity_id = ? \
                         WHERE {FRESH} ORDER BY a.published DESC, a.id DESC LIMIT ?"
                    ),
                    &[SqlValue::Integer(id), window, limit],
                )
            }
            other => return Err(format!("unknown briefing scope {:?}", other)),
        };
        let rows = rows.map_err(|error| {
            format!(
                "the archive could not be read for this briefing ({}), so nothing was stored",
                crate::scrub(error.message())
            )
        })?;
        Ok(rows
            .iter()
            .map(|row| Story {
                id: row.int("id").unwrap_or(0),
                published: row.text("published").unwrap_or("").to_string(),
                source: row.text("source").unwrap_or("").to_string(),
                title: row.text("title").unwrap_or("").to_string(),
                summary: row.text("summary").unwrap_or("").to_string(),
            })
            .collect())
    }

    /// The stories of one country: the primary column **and** every further place the story
    /// names — the union `atlas-desktop`'s `country_stories` reads and its chip counts.
    ///
    /// Two shapes of one read, as the window has them: an archive from before
    /// `article_countries` existed has no second places, so the column is then the whole
    /// answer rather than a failure. The fallback is detected the way the window detects it —
    /// the statement does not prepare — and only that one reason falls back; any other read
    /// failure is still a failure the caller must see.
    fn country_rows(
        &self,
        code: SqlValue,
        window: SqlValue,
        limit: SqlValue,
    ) -> Result<Vec<atlas_store::Row>, atlas_store::StoreError> {
        let both = format!(
            "{SELECT} WHERE (a.iso = ? OR EXISTS (SELECT 1 FROM article_countries c \
               WHERE c.article_id = a.id AND c.iso = ?)) \
             AND {FRESH} ORDER BY a.published DESC, a.id DESC LIMIT ?"
        );
        match self.store().query(
            &both,
            &[code.clone(), code.clone(), window.clone(), limit.clone()],
        ) {
            Ok(rows) => Ok(rows),
            Err(error) if error.message().contains("no such table: article_countries") => {
                let primary =
                    format!("{SELECT} WHERE a.iso = ? AND {FRESH} ORDER BY a.published DESC, a.id DESC LIMIT ?");
                self.store().query(&primary, &[code, window, limit])
            }
            Err(error) => Err(error),
        }
    }

    /// The topic id `key` names: the number itself, or the topic's name.
    fn brief_topic(&self, key: &str) -> Result<i64, String> {
        if key.is_empty() {
            return Err("a topic briefing needs the topic's id or its name".to_string());
        }
        let found = self
            .store()
            .query_one(
                "SELECT id FROM topics WHERE id = ? OR name = ? COLLATE NOCASE",
                &[SqlValue::Integer(number(key).unwrap_or(-1)), SqlValue::Text(key.to_string())],
            )
            .ok()
            .flatten()
            .and_then(|row| row.int("id"));
        found.ok_or_else(|| {
            format!(
                "there is no topic {:?} in this archive, so there is nothing to brief: add it \
                 first and collect some stories",
                key
            )
        })
    }

    /// The monitor id `key` names: the number itself, or the monitor's name.
    fn brief_monitor(&self, key: &str) -> Result<i64, String> {
        if key.is_empty() {
            return Err("a monitor briefing needs the monitor's id or its name".to_string());
        }
        let found = self
            .store()
            .query_one(
                "SELECT id FROM monitors WHERE id = ? OR name = ? COLLATE NOCASE",
                &[SqlValue::Integer(number(key).unwrap_or(-1)), SqlValue::Text(key.to_string())],
            )
            .ok()
            .flatten()
            .and_then(|row| row.int("id"));
        found.ok_or_else(|| {
            format!(
                "there is no monitor {:?} in this archive, so there is nothing to brief: add it \
                 first and let it collect",
                key
            )
        })
    }

    /// The entity id `key` names: the number itself, or the entity's name.
    fn brief_entity(&self, key: &str) -> Result<i64, String> {
        if key.is_empty() {
            return Err("an entity briefing needs the entity's id or its name".to_string());
        }
        let found = self
            .store()
            .query_one(
                "SELECT id FROM entities WHERE id = ? OR name = ? COLLATE NOCASE",
                &[SqlValue::Integer(number(key).unwrap_or(-1)), SqlValue::Text(key.to_string())],
            )
            .ok()
            .flatten()
            .and_then(|row| row.int("id"));
        found.ok_or_else(|| {
            format!(
                "there is no entity {:?} in this archive, so there is nothing to brief: collect \
                 some stories that name it",
                key
            )
        })
    }

    /// Write the row, through the store's own statement.
    ///
    /// `atlas-store` owns the `briefings` table and the upsert on `(scope,key,hours)`
    /// ([`atlas_store::Store::briefing_put`]), so this file writes no SQL of its own: a
    /// second writer is how two crates come to disagree about a cap or a conflict target.
    /// The store's refusal is a sentence already (it names the cap and the number), so it
    /// is carried through rather than paraphrased.
    fn brief_store(&self, row: &Briefing) -> Result<(), String> {
        match self.store().briefing_put(row) {
            Ok(()) => Ok(()),
            // The one failure a reader can act on: the table is `atlas-store`'s, so a
            // missing one is a build of this workspace out of step, not a bad briefing.
            Err(error) if error.message().contains("no such table") => Err(format!(
                "the archive has no briefings table yet ({}), so nothing was stored: \
                 atlas_store's briefings schema has to be applied first",
                crate::scrub(error.message())
            )),
            Err(error) => Err(format!(
                "the briefing could not be stored ({}), so nothing was written",
                crate::scrub(error.message())
            )),
        }
    }
}

/// The columns and the freshness rule every story statement shares.
///
/// `a` is the article, `s` the outlet: the archive keeps the publisher on `sources`, not on
/// the article (`handlers::t_get_article` reads it the same way and answers `"?"` for a
/// missing row), so a briefing names the outlet the reader would see beside the story.
const SELECT: &str = "SELECT a.id AS id, coalesce(a.published,'') AS published, \
     coalesce(s.name,'') AS source, coalesce(a.title,'') AS title, \
     coalesce(a.summary,'') AS summary \
     FROM articles a LEFT JOIN sources s ON s.id = a.source_id";

/// What the archive calls "current": the window rule `atlas-store`'s search and every list
/// in the app use — relevant, not a duplicate, and published inside the cutoff.
const FRESH: &str = "a.relevant = 1 AND a.dupe = 0 AND a.published >= ?";

/// The label a place is *named* by — `atlas-desktop`'s `PLACE_LABEL`
/// (`src/data.rs:262`), character for character.
///
/// `place` first, because that is the name the reader clicked; `iso` second, for a story the
/// geocoder only placed in a country. Reading `a.place` alone, which this did before, left
/// every story whose place fell back to its country in no place's briefing while the map's
/// dot listed it — the same disagreement as the country chip, one column over.
const PLACE_LABEL: &str = "coalesce(nullif(a.place, ''), nullif(a.iso, ''), '')";

/// The digest of the stories and how many of them made it.
///
/// One line per story, newest first: `[Article #ID] <published> · <source> · <title> —
/// <summary>`. The shape is `atlas-ai`'s (the prompt names it), and it is the shape the
/// model cites back as `[Article #1234]`.
///
/// Two caps, and both are the caller's: at most [`BRIEF_STORIES`] lines (the statement asks
/// for that many, and a caller with a longer list gets the same digest) and at most `chars`
/// characters — the line that would cross the budget is left out rather than cut, because
/// half a story reads like a fact and is not one.
///
/// `chars` is a parameter and [`BRIEF_CHARS`] is what the pipeline passes: with the per-line
/// caps above, forty lines come to about 15 000 characters and cannot reach 24 000, so a cap
/// baked into this function could never be proved to work. A test hands it a budget that
/// binds.
pub(crate) fn brief_digest(stories: &[Story], chars: usize) -> (String, usize) {
    let mut out = String::new();
    let mut count = 0usize;
    for story in stories.iter().take(BRIEF_STORIES) {
        let line = digest_line(story);
        if line.is_empty() {
            continue;
        }
        let added = if out.is_empty() {
            line.chars().count()
        } else {
            line.chars().count() + 1
        };
        if out.chars().count() + added > chars {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
        count += 1;
    }
    (out, count)
}

/// One story as one line, with each field defanged and cut.
///
/// HTML is stripped first: an RSS title can arrive with `<b>` in it, and a digest is not a
/// page. Nothing here decides what a story means — the caps are the layout's (one line) and
/// the fence's ([`atlas_ai::prompts::field_line`]).
fn digest_line(story: &Story) -> String {
    let published = field(&story.published, PUBLISHED_CHARS);
    let source = field(&story.source, SOURCE_CHARS);
    let title = field(&story.title, TITLE_CHARS);
    let summary = field(&story.summary, SUMMARY_CHARS);
    if title.is_empty() && summary.is_empty() {
        return String::new();
    }
    let mut line = format!("[Article #{}]", story.id);
    if !published.is_empty() {
        line.push(' ');
        line.push_str(&published);
    }
    if !source.is_empty() {
        line.push_str(" · ");
        line.push_str(&source);
    }
    if !title.is_empty() {
        line.push_str(" · ");
        line.push_str(&title);
    }
    if !summary.is_empty() {
        line.push_str(" — ");
        line.push_str(&summary);
    }
    line
}

/// One field of a digest line: tags out, then the fence's one-line rule.
fn field(text: &str, limit: usize) -> String {
    atlas_ai::prompts::field_line(&strip_tags(text), limit)
}

/// Drop HTML: tags, and the four named entities a feed actually emits.
///
/// Not a parser and not a sanitiser — the archive's text was cleaned on its way in. This
/// exists because a title from a feed can still carry markup, and a digest line is read by a
/// model that was told to quote dates and names, not to tidy a page. A `<` that does not
/// open a tag (a comparison in a headline) is kept, and so is an entity nobody maps: guessing
/// what `&eacute;` should become is how a name gets misspelled.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text.chars().peekable();
    while let Some(ch) = rest.next() {
        if ch == '&' {
            if let Some(decoded) = entity(&mut rest) {
                out.push(decoded);
                continue;
            }
            out.push(ch);
            continue;
        }
        if ch != '<' {
            out.push(ch);
            continue;
        }
        // A tag opens with a letter, `/` or `!`; anything else is text that happens to
        // contain the character.
        let opens = matches!(rest.peek(), Some(next) if next.is_ascii_alphabetic() || *next == '/' || *next == '!');
        if !opens {
            out.push(ch);
            continue;
        }
        let mut closed = false;
        for next in rest.by_ref() {
            if next == '>' {
                closed = true;
                break;
            }
        }
        if !closed {
            // An unclosed `<b` is not a tag: the text the reader has is all there is.
            out.push(ch);
        }
    }
    out
}

/// One named entity, or `None` and the stream left where it was.
///
/// Only the ones an RSS title carries in practice, and only their literal spellings: the
/// input is already this app's text, so a numeric reference is left as written rather than
/// decoded into a character nothing else would produce.
fn entity(rest: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<char> {
    const NAMED: [(&str, char); 4] = [
        ("amp;", '&'),
        ("lt;", '<'),
        ("gt;", '>'),
        ("quot;", '"'),
    ];
    let mut seen = String::new();
    let mut probe = rest.clone();
    for _ in 0..6 {
        let Some(next) = probe.next() else {
            return None;
        };
        if !next.is_ascii_alphanumeric() && next != ';' {
            return None;
        }
        seen.push(next);
        if let Some((_, decoded)) = NAMED.iter().find(|(name, _)| *name == seen) {
            // Consume exactly what was matched, so the text after it is untouched.
            for _ in 0..seen.chars().count() {
                rest.next();
            }
            return Some(*decoded);
        }
    }
    None
}

/// A positive whole number, or `None`: the id half of a scope key.
fn number(key: &str) -> Option<i64> {
    key.trim().parse::<i64>().ok().filter(|value| *value > 0)
}

/// One refusal, in the two keys the queue and the window read.
fn refused(sentence: impl Into<String>) -> Value {
    json!({ "ok": false, "error": crate::scrub(&sentence.into()) })
}
