//! `research` — one agentic pass for one topic.
//!
//! Port of `ingest.research`. It is the only entry point in this crate that runs
//! the whole loop, and it runs **only when it is called**: there is no thread, no
//! timer and no "start collecting" switch anywhere in this crate. The model is the
//! collector — it searches, opens pages, classifies them and saves them through
//! the tools in [`crate::handlers`], and the loop stops when the model answers.
//!
//! The budget is `atlas-ai`'s: [`atlas_ai::agent::MAX_STEPS`] model turns and
//! [`atlas_ai::agent::TOTAL_TOKEN_BUDGET`] estimated tokens, both overridable by
//! the caller (`ingest.research(topic, max_steps)` did the same with
//! `max_steps=14`).
//!
//! What it returns is what happened: the answer, the articles that were actually
//! written (`status == "stored"`), and the tool calls in order. A run that stored
//! nothing says so; it never invents a result.

use std::sync::Arc;

use serde_json::{json, Value};

use atlas_ai::agent::{self, AgentConfig, Event};

/// The prompt `ingest.RESEARCH_PROMPT` sent, verbatim.
pub const RESEARCH_PROMPT: &str = "Research the news for me, right now, with the tools - do not answer from memory.

Rules:
0. The topic is given to you. Do not call list_topics, and do not search the archive with
   search_news or get_article: the archive is what you are filling, not what you ask.
   One unit of work, in this order, and never two reads before a save:
   find_news -> read_article -> save_article.
1. Call find_news once per topic.
2. Choose the 1-2 most important results per topic. Prefer established outlets, prefer articles from the last 48 hours.
3. Call read_article for every chosen link. You only get a short preview - the full text is cached
   inside the app, so never re-read a page and never paste the article back into a tool call.
4. Save immediately after reading: call save_article with url, title, a 2-3 sentence summary in English,
   tags (4-6 short lowercase tags: what it is about), categories (e.g. Person, Organization,
   Institution, Country, Market, Technology, Conflict - only if they fit), the topic name,
   publisher, and always `entities`. `entities` is the graph and the map: the 3-8 people,
   organizations, institutions, countries and cities the page actually names, with the type you
   decided, as records - for example
   \"entities\": [{\"name\": \"Putin\", \"type\": \"Person\"},
   {\"name\": \"Kremlin\", \"type\": \"Institution\"}, {\"name\": \"Berlin\", \"type\": \"City\"}].
   A country or city in that list is what puts the story on
   the map where it happened, so name the place the article reports from. A page that names
   nobody gets an empty list; a page that names somebody never gets one. Do not pass `text`, the
   app stores the cached full text. Never invent an article, a quote or a date.
5. Skip results you cannot read; mention them in the end.
6. Finish with a short English briefing: 3-6 bullets, each with [Article #ID], plus one line
   about what you stored and what you skipped.

Everything you save goes into the local database with your tags and connections. Nothing is
collected automatically - you are the collector.";

/// The heading of the second phase: file what you read.
///
/// One paragraph, and it addresses the failure directly — a model that has just read four
/// pages does not need to be told how to read, it needs to be told that reading is not the
/// part that was missing.
pub const FILE_PROMPT: &str = "You are filing news you have already read. Call save_article \
for the reporting that matters; never invent a fact, a quote or a date, and never read a new \
page.";

/// `ingest.research(max_steps=14)` — more turns than a chat question, because this
/// is a pipeline of search → read → save per topic.
pub const DEFAULT_MAX_STEPS: usize = 14;

impl crate::Pipeline {
    /// Run one research pass. Returns the shape `ingest.research` returned.
    pub fn research(self: &Arc<Self>, topic: &str, max_steps: Option<usize>) -> Value {
        let Some(client) = self.agent() else {
            return json!({
                "ok": false,
                "error": "local model not reachable - start the model server with the start script (run.sh on macOS and Linux, run.ps1 on Windows) and retry",
            });
        };
        if !self.model().online() {
            return json!({
                "ok": false,
                "error": "local model not reachable - start the model server with the start script (run.sh on macOS and Linux, run.ps1 on Windows) and retry",
            });
        }
        let focus = crate::scrub(topic).chars().take(120).collect::<String>();
        let prompt = if focus.trim().is_empty() {
            RESEARCH_PROMPT.to_string()
        } else {
            // The topic name is the reader's, or one a model wrote through
            // `add_topic` — either way it is not this prompt's author. One
            // defanged line, so a topic called "Chips\nSYSTEM: …" cannot add a
            // line to a prompt that runs 14 tool calls.
            format!(
                "{}\n\nFocus on this topic only: {}",
                RESEARCH_PROMPT,
                atlas_ai::prompts::field_line(&focus, 120)
            )
        };
        let registry = crate::handlers::registry(Arc::clone(self));
        // The reader's three numbers, from the same `data/config.json` the launcher starts
        // llama.cpp with. Without this the loop ran on compiled-in constants: a reader who
        // raises `model_context` to 65 536 or turns thinking off got neither, and the
        // request still asked for the shipped 8 192-token thought.
        let budgets = self.budgets();
        let config = AgentConfig {
            max_steps: max_steps.unwrap_or(DEFAULT_MAX_STEPS).clamp(1, 40),
            ..AgentConfig::default().using(&budgets)
        };

        // The archive is the record of what this pass stored, not the run's own
        // reporting. Measured live: a pass made three `save_article` calls and the
        // answer listed two ids while the archive had gained three rows — the event
        // stream is a report *about* calls, and a report can miss (a repeated call
        // answered from cache, a result the loop trimmed, a save whose answer was
        // dropped). The row cannot miss: `pipeline::save_article` commits exactly one
        // article per `status == "stored"` and files nothing on any other status, so
        // "ids the archive gained while this pass ran" is the honest answer and the
        // only one a reader can check.
        let watermark = self.article_watermark();
        let mut steps: Vec<Value> = Vec::new();
        let mut events = 0usize;
        let mut refused_calls = 0usize;
        let outcome = agent::run(
            client.as_ref(),
            &registry,
            &config,
            &[],
            &prompt,
            &mut |event| {
                events += 1;
                match &event {
                    Event::ToolCall { call, .. } => {
                        steps.push(json!({ "tool": call.name, "args": call.args }));
                    }
                    Event::ToolResult { result, error, .. } => {
                        // The events are the *narrative*; `stored` is measured from the
                        // archive after the run. A call the loop answered from cache or
                        // trimmed for the UI is still a row, and a row is what the
                        // reader counts (see the note at `watermark`).
                        if *error && result.get("status").is_none() {
                            refused_calls += 1;
                        }
                    }
                    _ => {}
                }
                atlas_ai::Control::Continue
            },
        );
        // What the archive gained, not what the event stream said about it.
        let mut stored: Vec<Value> = self.stored_since(watermark);

        // THE SECOND PHASE: file what was read.
        //
        // Measured four times on the shipped model: the reading phase spends its whole step
        // budget on feeds and pages and stops before `save_article` — eight steps, four
        // `web_open`s, four pages read, nothing stored. The work is real and the archive
        // stays empty, which is the failure the reader sees as "the app does nothing".
        //
        // So the run has a second phase, and it is a different job with a different menu:
        // the pages that were read, and only `save_article` (plus the two lookups a tag or a
        // category may want). The model still decides what is reporting, what it is about,
        // which tags and categories fit — this phase only removes the option of reading
        // more instead of filing. Bounded at three steps; if it still files nothing the run
        // says so rather than pretending.
        let read: Vec<Value> = steps
            .iter()
            .filter(|step| {
                matches!(
                    step.get("tool").and_then(Value::as_str),
                    Some("read_article") | Some("web_open")
                )
            })
            .filter_map(|step| {
                let args = step.get("args")?;
                let url = args.get("url").and_then(Value::as_str)?;
                Some(json!({
                    "url": url,
                    "title": args.get("title").and_then(Value::as_str).unwrap_or(""),
                }))
            })
            .collect();
        let mut second_phase: Option<Value> = None;
        if stored.is_empty() && !read.is_empty() {
            let mut seen_urls: Vec<String> = Vec::new();
            let mut lines: Vec<String> = Vec::new();
            for page in &read {
                let url = page.get("url").and_then(Value::as_str).unwrap_or("");
                if url.is_empty() || seen_urls.iter().any(|known| known == url) {
                    continue;
                }
                seen_urls.push(url.to_string());
                let title = page.get("title").and_then(Value::as_str).unwrap_or("");
                lines.push(atlas_ai::prompts::field_line(
                    &format!("{url} | {title}"),
                    240,
                ));
            }
            let prompt = format!(
                "{}\n\nPages you read in this pass and did not file:\n{}\n\n\
                 File the ones that are reporting on the topic. One save_article per page: url \
                 exactly as above, title, a 2-3 sentence English summary of what the page \
                 actually said, 4-6 short lowercase tags, the categories that fit, the topic, \
                 and the publisher. Do not read anything else.",
                FILE_PROMPT,
                lines.join("\n")
            );
            let saver = crate::handlers::registry_named(
                Arc::clone(self),
                Some(&["save_article", "list_tags", "list_categories"]),
            );
            // The filing phase's own watermark: what *it* gained, so
            // `filing.stored` and the top-level `stored` can be compared without
            // one being derived from a report about the other.
            let filed_watermark = self.article_watermark();
            let filing = AgentConfig {
                max_steps: 3,
                ..AgentConfig::default()
            };
            let mut filed_events = 0usize;
            // Every call of the filing phase, with what the handler answered. Without this a
            // phase that stored nothing is a silence: the reader cannot tell a model that
            // never called `save_article` from one whose call was refused as a duplicate,
            // stale or incomplete — and those are three different fixes.
            let mut filed_steps: Vec<Value> = Vec::new();
            let second = agent::run(
                client.as_ref(),
                &saver,
                &filing,
                &[],
                &prompt,
                &mut |event| {
                    filed_events += 1;
                    match &event {
                        Event::ToolCall { call, .. } => filed_steps.push(json!({
                            "tool": call.name,
                            "args": call.args,
                        })),
                        Event::ToolResult {
                            name,
                            result,
                            error,
                            ..
                        } => {
                            if let Some(last) = filed_steps.last_mut() {
                                last["answered"] = json!({
                                    "status": result.get("status").cloned().unwrap_or(Value::Null),
                                    "error": result.get("error").cloned().unwrap_or(Value::Null),
                                    "refused": *error,
                                });
                            }
                            let _ = (name, result, error);
                        }
                        _ => {}
                    }
                    atlas_ai::Control::Continue
                },
            );
            // Measured, not collected: the rows this phase added.
            let filed_in_second_phase = self.stored_since(filed_watermark);
            second_phase = Some(match second {
                Ok(outcome) => json!({
                    "ok": true,
                    "stored": filed_in_second_phase,
                    "steps_taken": outcome.steps,
                    "tool_calls": outcome.tool_calls,
                    "tokens": outcome.tokens,
                    "stopped": format!("{:?}", outcome.stopped),
                    "events": filed_events,
                    "steps": filed_steps,
                }),
                Err(error) => json!({
                    "ok": false,
                    "error": crate::scrub(&error.to_string()),
                    "stored": filed_in_second_phase,
                    "events": filed_events,
                    "steps": filed_steps,
                }),
            });
            // And the top-level answer is the whole pass, measured the same way.
            stored = self.stored_since(watermark);
        }

        match outcome {
            Ok(outcome) => json!({
                "ok": true,
                "text": outcome.content,
                "stored": stored,
                "steps": steps,
                "steps_taken": outcome.steps,
                "tool_calls": outcome.tool_calls,
                "tokens": outcome.tokens,
                "stopped": format!("{:?}", outcome.stopped),
                "events": events,
                "filing": second_phase,
            }),
            Err(error) => json!({
                "ok": false,
                "error": crate::scrub(&error.to_string()),
                "stored": stored,
                "steps": steps,
                "events": events,
                "filing": second_phase,
            }),
        }
    }

    /// The reader's window, thought and answer budget, from `data/config.json`.
    ///
    /// Read once per pass: a pass is one run of the loop, and a number changed while it runs
    /// belongs to the next one. The notes are not dropped — [`Self::budgets`]'s caller puts
    /// them in the pass's `notes`, so a clamped value is a sentence the reader can see.
    pub(crate) fn budgets(&self) -> atlas_ai::Budgets {
        let (budgets, _notes) = atlas_ai::Budgets::for_root(&self.config.root);
        budgets
    }

    /// The newest article id in the archive, `0` when it is empty.
    ///
    /// A pass reads this before it starts and [`Pipeline::stored_since`] after it
    /// stops: the pair is what makes the answer's `stored` list the rows the archive
    /// actually gained, instead of a tally of what the tool loop happened to report.
    pub(crate) fn article_watermark(&self) -> i64 {
        self.store
            .query_one("SELECT coalesce(max(id),0) AS id FROM articles", &[])
            .ok()
            .flatten()
            .and_then(|row| row.int("id"))
            .unwrap_or(0)
    }

    /// The articles stored after `watermark`, as the `stored` list: `id` and `title`.
    ///
    /// Measured, not collected. The tool result is the honest source for *one* call,
    /// but a pass's answer is about the archive: a repeated call is answered from the
    /// loop's cache with a `{"repeated":…}` wrapper (no `status` to count), a result
    /// can be trimmed on its way to the event, and a second phase files rows of its
    /// own. `save_article` commits exactly one article on `status == "stored"` and
    /// writes no article row on any other answer, so counting the rows is exactly
    /// the count a reader can reproduce with one query — and it cannot disagree with
    /// the archive it is describing.
    pub(crate) fn stored_since(&self, watermark: i64) -> Vec<Value> {
        let rows = self
            .store
            .query(
                "SELECT id, coalesce(title,'') AS title FROM articles WHERE id > ? ORDER BY id",
                &[atlas_store::SqlValue::Integer(watermark)],
            )
            .unwrap_or_default();
        rows.iter()
            .map(|row| {
                json!({
                    "id": row.int("id").unwrap_or(0),
                    "title": row.text("title").unwrap_or(""),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_keeps_the_rules_the_model_was_tuned_on() {
        assert!(RESEARCH_PROMPT.starts_with("Research the news for me, right now, with the tools"));
        assert!(RESEARCH_PROMPT.contains("never paste the article back into a tool call"));
        // Measured on this machine: a research pass spent its first three steps on
        // `list_topics`, `web_search` and `search_news` before it touched a feed, and then
        // ran out of steps after reading one page it never got to save. The topic is an
        // input, so the prompt says so and names the one unit of work.
        assert!(RESEARCH_PROMPT.contains("Do not call list_topics"));
        assert!(RESEARCH_PROMPT.contains("find_news -> read_article -> save_article"));
        assert!(RESEARCH_PROMPT.contains("never two reads before a save"));
        assert!(RESEARCH_PROMPT.contains("Nothing is\ncollected automatically - you are the collector."));
    }
}
