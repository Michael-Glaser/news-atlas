//! [`NativeRules`]: the five engines, wired for real, behind the [`Rules`] trait.
//!
//! Decides: that every engine is answered by its own library in this process (the
//! transport is still a value, because the parity witness asks the binaries the
//! same questions), that the question is serialised **once** whichever transport
//! asks it, and how each answer's fields become a Rust value (a place, the topics
//! that matched, a freshness verdict).
//!
//! Does not decide: any rule itself, or any cap. Every value in an answer is the
//! engine's, and every bound (how many tokens, how much text, how large a request)
//! is read from the engine that applies it — this file spells field names and
//! refuses a field of the wrong type by leaving it at its documented default (an
//! empty place, `fresh: false`).
//!
//! # Why one question means one serialisation
//!
//! [`Self::ask`] takes a [`Value`] from [`super::locate_request`] and friends,
//! writes it to JSON text once, and hands *that text* to whichever transport
//! answers. The child gets it on stdin; the library gets it as `raw`. The answer
//! text goes through [`super::answer::parse`] either way. There is therefore no
//! "in-process answer" and "binary answer" to drift apart — there is one answer,
//! produced by one function, and the parity test spawns the binary to prove it.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::rules::answer::{self, Spoken};
use crate::rules::spawn;
use crate::rules::{
    inproc, locate_request, parse_request, topics_request, window_request, ArticleText, Locate,
    Parsed, Rules, RulesError, TopicMatch, TopicRow, Window, SPAWNED,
};
use crate::Config;

/// Which way one engine's question travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// The engine's own library, in this process.
    InProcess,
    /// The engine's binary, in a child process.
    Spawn,
}

impl Transport {
    /// The engine's own answer when it has a library entry point, else a process.
    fn of(engine: &str) -> Transport {
        if inproc::answers_here(engine) {
            Transport::InProcess
        } else {
            Transport::Spawn
        }
    }
}

/// The real thing: five engines in this process, no child unless a test asks for
/// one (`spawn_only`).
#[derive(Debug, Clone)]
pub struct NativeRules {
    bin_dir: PathBuf,
    timeout: Duration,
    /// Set only by [`NativeRules::spawn_only`], so a test can ask the binaries the
    /// same question the app asks in-process.
    forced: Option<Transport>,
}

impl NativeRules {
    /// Check that every engine this build *needs as a file* is there, and refuse
    /// to start if one is not.
    ///
    /// Only [`SPAWNED`] is checked, and that list is empty: all five engines
    /// answer from this process, so a missing engine binary is not a reason to
    /// refuse to start an app that works. The loop stays so that an engine which
    /// ever has to go back to a child process is checked the moment it is named
    /// there, and the message names the file and the command that builds it.
    pub fn discover(config: &Config) -> Result<NativeRules, RulesError> {
        let mut missing: Vec<String> = Vec::new();
        for engine in SPAWNED {
            let path = config.bin(engine);
            if !spawn::is_executable(&path) {
                missing.push(path.display().to_string());
            }
        }
        if !missing.is_empty() {
            return Err(RulesError::new(format!(
                "the ingest pipeline needs {} engine binary/binaries that are not built: {} - run: cargo build --release --manifest-path rust/Cargo.toml",
                missing.len(),
                missing.join(", ")
            )));
        }
        Ok(NativeRules {
            bin_dir: config.bin_dir.clone(),
            timeout: config.rules_timeout,
            forced: None,
        })
    }

    /// A rules engine bound to one directory of binaries (tests, embedding).
    pub fn with_bin_dir(bin_dir: impl Into<PathBuf>, timeout: Duration) -> NativeRules {
        NativeRules {
            bin_dir: bin_dir.into(),
            timeout,
            forced: None,
        }
    }

    /// The same requests, every one of them as a child process.
    ///
    /// The parity witness: `rules::tests::the_in_process_answer_is_the_binary_answer`
    /// asks this and the shipped wiring the same question and compares the
    /// answers byte for byte. It is public because a caller that wants the
    /// process path back (a benchmark of the spawn cost, a host that distrusts
    /// in-process state) must not have to fork this crate to get it.
    pub fn spawn_only(bin_dir: impl Into<PathBuf>, timeout: Duration) -> NativeRules {
        NativeRules {
            bin_dir: bin_dir.into(),
            timeout,
            forced: Some(Transport::Spawn),
        }
    }

    fn bin(&self, engine: &str) -> PathBuf {
        spawn::bin_path(&self.bin_dir, engine)
    }

    fn transport(&self, engine: &str) -> Transport {
        self.forced.unwrap_or_else(|| Transport::of(engine))
    }

    /// Ask one engine one question, over whichever transport that engine uses.
    ///
    /// `pub(crate)` so the tests in [`super::tests`] can ask a question at this
    /// level — the op and the payload, not the trait method — which is what makes
    /// "the two transports answer the same bytes" and "no question needs a
    /// process" provable here rather than only through each caller's field.
    pub(crate) fn ask(&self, engine: &str, op: &str, request: &Value) -> Result<Value, RulesError> {
        // One serialisation: the same bytes reach a pipe and a library call.
        let raw = request.to_string();
        answer::check_payload(engine, op, &raw)?;
        let spoken: Spoken = match self.transport(engine) {
            Transport::InProcess => inproc::ask(engine, op, &raw)?,
            Transport::Spawn => spawn::ask(
                &self.bin(engine),
                engine,
                op,
                &raw,
                self.timeout,
            )?,
        };
        answer::parse(engine, op, &spoken)
    }

    fn text_op(&self, op: &str, request: Value, key: &str) -> Result<String, RulesError> {
        let answer = self.ask("atlas-text", op, &request)?;
        Ok(answer
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }
}

impl Rules for NativeRules {
    fn clean_text(&self, text: &str) -> Result<String, RulesError> {
        self.text_op("clean_text", serde_json::json!({ "text": text }), "text")
    }

    fn norm_url(&self, url: &str) -> Result<String, RulesError> {
        self.text_op("norm_url", serde_json::json!({ "url": url }), "url")
    }

    fn publisher(&self, url: &str) -> Result<String, RulesError> {
        self.text_op("publisher", serde_json::json!({ "url": url }), "publisher")
    }

    fn clean_title(&self, title: &str, publisher: &str) -> Result<String, RulesError> {
        self.text_op(
            "clean_title",
            serde_json::json!({ "title": title, "publisher": publisher }),
            "title",
        )
    }

    fn tokenize(&self, text: &str, max_words: usize) -> Result<Vec<String>, RulesError> {
        // `max_words` travels unclamped: `atlas_text::run` clamps it to its own
        // `atlas_text::MAX_WORDS` (1..=32, `atlas_text::DEFAULT_MAX_WORDS` when
        // absent), and a second clamp here would be a bound written twice — the
        // payload would ask for a cap the engine never applied or, worse, apply a
        // narrower one of its own.
        let answer = self.ask(
            "atlas-text",
            "tokenize",
            &serde_json::json!({ "text": text, "max_words": max_words }),
        )?;
        Ok(answer
            .get("tokens")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|token| token.to_string())
                    .collect()
            })
            .unwrap_or_default())
    }

    fn extract_html(&self, html: &str, url: &str, title: &str) -> Result<String, RulesError> {
        let answer = self.ask(
            "atlas-core",
            "extract",
            &serde_json::json!({ "html": html, "url": url, "title": title }),
        )?;
        Ok(answer
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    fn entity_candidates(
        &self,
        text: &str,
        known: &[String],
        min_count: usize,
        max: usize,
    ) -> Result<Vec<(String, usize)>, RulesError> {
        let answer = self.ask(
            "atlas-core",
            "entities",
            &serde_json::json!({
                "text": text,
                "known": known,
                "min_count": min_count.max(1),
                "max": max.max(1),
            }),
        )?;
        let mut out: Vec<(String, usize)> = Vec::new();
        if let Some(items) = answer.get("entities").and_then(Value::as_array) {
            for item in items {
                let name = item.get("name").and_then(Value::as_str).unwrap_or("").trim();
                if name.is_empty() {
                    continue;
                }
                let count = item
                    .get("count")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .max(1) as usize;
                out.push((name.to_string(), count));
            }
        }
        Ok(out)
    }

    fn locate(&self, article: &ArticleText) -> Result<Locate, RulesError> {
        let answer = self.ask("atlas-places", "locate", &locate_request(article))?;
        Ok(Locate {
            place: answer.get("place").and_then(Value::as_str).unwrap_or("").to_string(),
            iso: answer.get("iso").and_then(Value::as_str).unwrap_or("").to_string(),
            lat: answer.get("lat").and_then(Value::as_f64).unwrap_or(0.0),
            lon: answer.get("lon").and_then(Value::as_f64).unwrap_or(0.0),
            prec: answer.get("prec").and_then(Value::as_str).unwrap_or("").to_string(),
            conf: answer.get("conf").and_then(Value::as_f64).unwrap_or(0.0),
        })
    }

    fn match_topics(
        &self,
        topics: &[TopicRow],
        article: &ArticleText,
    ) -> Result<Vec<TopicMatch>, RulesError> {
        // No topics, no question: an article that matches nothing cannot match a
        // topic the reader does not have.
        if topics.is_empty() {
            return Ok(Vec::new());
        }
        let answer = self.ask("atlas-topics", "match", &topics_request(topics, article))?;
        let mut out: Vec<TopicMatch> = Vec::new();
        if let Some(items) = answer.get("matches").and_then(Value::as_array) {
            for item in items {
                let id = item.get("id").and_then(Value::as_i64).unwrap_or(0);
                if id == 0 {
                    continue;
                }
                out.push(TopicMatch {
                    id,
                    name: item.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    score: item.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                    matched: item
                        .get("matched")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(|term| term.to_string())
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
        }
        Ok(out)
    }

    fn parse_date(&self, ts: &str, now: i64) -> Result<Parsed, RulesError> {
        let answer = self.ask("atlas-dates", "parse", &parse_request(ts, now))?;
        Ok(Parsed {
            ok: answer.get("ok").and_then(Value::as_bool).unwrap_or(false),
            epoch: answer.get("epoch").and_then(Value::as_i64).unwrap_or(0),
            iso: answer.get("iso").and_then(Value::as_str).unwrap_or("").to_string(),
            reason: answer
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
    }

    fn window(&self, ts: &str, max_age_s: i64, now: i64) -> Result<Window, RulesError> {
        let answer = self.ask("atlas-dates", "window", &window_request(ts, max_age_s, now))?;
        Ok(Window {
            fresh: answer.get("fresh").and_then(Value::as_bool).unwrap_or(false),
            reason: answer
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unparsed")
                .to_string(),
            age_h: answer.get("age_h").and_then(Value::as_f64).unwrap_or(0.0),
            iso: answer.get("iso").and_then(Value::as_str).unwrap_or("").to_string(),
            note: answer
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_engine_gets_the_transport_it_can_answer_on() {
        for engine in inproc::ENGINES {
            assert_eq!(
                Transport::of(engine),
                Transport::InProcess,
                "{} exposes run(cmd, raw)",
                engine
            );
            assert!(!SPAWNED.contains(engine));
        }
        for engine in SPAWNED {
            assert_eq!(Transport::of(engine), Transport::Spawn, "{}", engine);
        }
        for engine in crate::rules::ENGINES {
            assert!(
                inproc::ENGINES.contains(engine) || SPAWNED.contains(engine),
                "{} is in no transport list, so nothing can answer it",
                engine
            );
        }
        assert_eq!(
            crate::rules::ENGINES.len(),
            inproc::ENGINES.len() + SPAWNED.len(),
            "every engine is in exactly one transport list"
        );
    }

    #[test]
    fn a_spawn_only_rules_object_asks_the_binaries() {
        let rules = NativeRules::spawn_only(std::env::temp_dir(), Duration::from_millis(20));
        assert_eq!(rules.transport("atlas-places"), Transport::Spawn);
        assert_eq!(rules.transport("atlas-text"), Transport::Spawn);
        // …and the shipped wiring asks nothing of a process, not even the two
        // engines that were children until their envelopes became library calls.
        let shipped = NativeRules::with_bin_dir(std::env::temp_dir(), Duration::from_millis(20));
        for engine in crate::rules::ENGINES {
            assert_eq!(
                shipped.transport(engine),
                Transport::InProcess,
                "{} still starts a process",
                engine
            );
        }
        assert!(
            crate::rules::SPAWNED.is_empty(),
            "no engine needs a binary any more: {:?}",
            crate::rules::SPAWNED
        );
    }

    #[test]
    fn a_bin_dir_with_no_engines_in_it_is_not_a_reason_to_refuse_to_start() {
        // Every engine answers from this crate's dependencies, so a machine with
        // no `atlas-text`/`atlas-core`/… file still runs the pipeline: the boot
        // check has nothing to insist on.
        let mut config = Config::for_root(std::env::temp_dir());
        config.bin_dir = std::path::Path::new("/nowhere/atlas-bin").to_path_buf();
        NativeRules::discover(&config)
            .expect("an empty directory of engine binaries is enough, because no engine needs one");
    }
}
