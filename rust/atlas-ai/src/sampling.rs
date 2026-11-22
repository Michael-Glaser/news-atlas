//! The sampling parameters, per job, and the one clamp that bounds them.
//!
//! Decisions: the legal range of `temperature` and `max_tokens` for the local
//! server ([`MIN_TEMPERATURE`], [`MAX_TOKENS`]), the documented pair for each job
//! ATLAS asks the model for ([`Job`]), and how a temperature is rounded onto the
//! wire ([`wire_temperature`]).
//!
//! Why a bound exists at all: `temperature` and `max_tokens` are the only two
//! numbers in a request that can waste minutes of local inference or make
//! llama.cpp answer `400`. A caller that computes a temperature can hand over
//! `NaN` (which `serde_json` cannot represent at all — it would send
//! `"temperature": null` and the server would reject the whole request) or `-1.0`,
//! and a caller that passes `0` for `max_tokens` asks for an empty answer. So the
//! clamp is applied once, in [`crate::chat::ChatRequest::body`], to every request
//! this crate sends — the agent loop, the CLI and `chat_simple` alike.
//!
//! It does not: decide which job a caller is running, budget a whole run
//! ([`crate::agent::AgentConfig`] does), or choose a prompt.

use crate::chat::{DEFAULT_MAX_TOKENS, DEFAULT_TEMPERATURE};

/// A temperature below this is not a temperature. Zero is greedy decoding, which
/// the news and monitor questions deliberately use.
pub const MIN_TEMPERATURE: f32 = 0.0;

/// Above this llama.cpp's sampling is noise for a news task, and the request is
/// the caller's mistake rather than a setting.
pub const MAX_TEMPERATURE: f32 = 2.0;

/// An answer needs at least one token.
pub const MIN_TOKENS: u32 = 1;

/// The largest answer this crate will ask for in one call.
///
/// Half the 32 768-token window: `prompt_room` keeps the other half for the prompt, so a
/// caller asking for the ceiling still sends a request the server can finish. Raised from
/// 8 192 with the window, so a caller may ask for the eight thousand tokens of thinking the
/// model needs on a long article instead of having the request clamped below them.
pub const MAX_TOKENS: u32 = 16384;

/// Clamp one request's sampling to the legal range.
///
/// A `NaN` or infinite temperature becomes [`DEFAULT_TEMPERATURE`] instead of the
/// `null` `serde_json` would otherwise write, so a caller's arithmetic slip cannot
/// turn into a rejected request.
pub fn bounded(temperature: f32, max_tokens: u32) -> (f32, u32) {
    let temperature = if temperature.is_finite() {
        temperature.clamp(MIN_TEMPERATURE, MAX_TEMPERATURE)
    } else {
        DEFAULT_TEMPERATURE
    };
    (temperature, max_tokens.clamp(MIN_TOKENS, MAX_TOKENS))
}

/// A bounded temperature as it goes on the wire: two decimal places.
///
/// `temperature` is an `f32` because that is the type a caller computes with, and
/// widening `0.3f32` to `f64` prints `0.30000001192092896` — a number no tuning
/// session ever chose, in a request a reader may inspect. Rounding to hundredths
/// keeps the value a caller asked for (`0.35`, `0.1`, `0.0`) exactly.
pub fn wire_temperature(temperature: f32) -> f64 {
    let (temperature, _) = bounded(temperature, MIN_TOKENS);
    ((temperature as f64) * 100.0).round() / 100.0
}

/// One thing ATLAS asks the model to do.
///
/// Every arm carries the numbers the job was tuned with, and where they come from,
/// so the whole table can be read in one screen instead of eight call sites. A
/// caller outside this crate that still owns its own constant (`atlas-ingest`:
/// verdict/enrichment, `atlas-monitor`: the monitor question, `atlas-web`: the
/// digest) is named in the comment; the arm is the number to move them to, and the
/// values here are pinned by [`tests::every_job_keeps_the_parameters_it_was_tuned_with`]
/// so the two cannot drift silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// "Is this page reporting?" — `ai.news_verdict`: cold and 90 tokens, because
    /// the answer is one JSON object.
    Verdict,
    /// The enrichment pass (`enrichment.MAX_TOKENS`): near-greedy, one object.
    Enrich,
    /// `ai.analyze_article`: entities, summary, sentiment.
    Analysis,
    /// The digest (`ai.briefing`): slightly warmer, 700 tokens of prose.
    Briefing,
    /// The `summarize` tool: prose over stored summaries.
    Summarize,
    /// One `chat_simple` question from the CLI or the server.
    Chat,
    /// A turn that may call tools (the agent loop).
    Agent,
    /// The last turn of a run: no tools, a shorter answer.
    AgentFinal,
}

impl Job {
    /// `(temperature, max_tokens)` for this job.
    pub fn sampling(self) -> (f32, u32) {
        match self {
            Job::Verdict => (0.0, 90),
            Job::Enrich => (0.1, 320),
            Job::Analysis => (0.2, 900),
            Job::Briefing => (0.35, 700),
            Job::Summarize => (0.3, 1_200),
            Job::Chat => (0.3, 1_200),
            Job::Agent => (DEFAULT_TEMPERATURE, DEFAULT_MAX_TOKENS),
            Job::AgentFinal => (DEFAULT_TEMPERATURE, 1_024),
        }
    }

    /// The name a status line or a test prints. Stable: it is matched on.
    pub fn name(self) -> &'static str {
        match self {
            Job::Verdict => "verdict",
            Job::Enrich => "enrich",
            Job::Analysis => "analysis",
            Job::Briefing => "briefing",
            Job::Summarize => "summarize",
            Job::Chat => "chat",
            Job::Agent => "agent",
            Job::AgentFinal => "agent-final",
        }
    }

    /// Every job, for a caller that checks them all instead of trusting the table.
    pub fn all() -> [Job; 8] {
        [
            Job::Verdict,
            Job::Enrich,
            Job::Analysis,
            Job::Briefing,
            Job::Summarize,
            Job::Chat,
            Job::Agent,
            Job::AgentFinal,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonsense_is_bounded_not_forwarded() {
        assert_eq!(bounded(f32::NAN, 0), (DEFAULT_TEMPERATURE, MIN_TOKENS));
        assert_eq!(
            bounded(f32::INFINITY, 1_000_000),
            (DEFAULT_TEMPERATURE, MAX_TOKENS)
        );
        assert_eq!(bounded(f32::NEG_INFINITY, 4), (DEFAULT_TEMPERATURE, 4));
        assert_eq!(bounded(-1.0, 4), (MIN_TEMPERATURE, 4));
        assert_eq!(bounded(7.5, 4), (MAX_TEMPERATURE, 4));
        // A sane value is untouched: the clamp is a fence, not a policy.
        assert_eq!(bounded(0.35, 700), (0.35, 700));
    }

    #[test]
    fn the_wire_temperature_is_the_number_the_caller_asked_for() {
        // The defect this pins: `0.3f32 as f64` serialised as
        // `0.30000001192092896`, a value no tuning session chose.
        assert_eq!(wire_temperature(0.3), 0.3);
        assert_eq!(wire_temperature(0.3).to_string(), "0.3");
        assert_eq!(wire_temperature(0.25), 0.25);
        assert_eq!(wire_temperature(0.35), 0.35);
        assert_eq!(wire_temperature(0.1), 0.1);
        assert_eq!(wire_temperature(0.0), 0.0);
        // A nonsense value is bounded first, then rounded.
        assert_eq!(wire_temperature(f32::NAN), DEFAULT_TEMPERATURE as f64);
        assert_eq!(wire_temperature(9.0), MAX_TEMPERATURE as f64);
    }

    #[test]
    fn every_job_is_inside_the_bounds() {
        for job in Job::all() {
            let (temperature, max_tokens) = job.sampling();
            assert!(
                (MIN_TEMPERATURE..=MAX_TEMPERATURE).contains(&temperature),
                "{}: temperature {} is outside the bounds",
                job.name(),
                temperature
            );
            assert!(
                (MIN_TOKENS..=MAX_TOKENS).contains(&max_tokens),
                "{}: max_tokens {} is outside the bounds",
                job.name(),
                max_tokens
            );
            assert_eq!(bounded(temperature, max_tokens), (temperature, max_tokens));
            assert!(!job.name().is_empty());
        }
        // One entry per job: `all()` is what a checker iterates, so an arm added to
        // the enum without an entry here would be invisible.
        let mut names: Vec<&str> = Job::all().iter().map(|job| job.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Job::all().len());
    }

    #[test]
    fn every_job_keeps_the_parameters_it_was_tuned_with() {
        // The numbers the rest of the workspace is built on. If one moves, the
        // call site that owns it has to move too — this is the test that says so.
        assert_eq!(Job::Verdict.sampling(), (0.0, 90), "atlas-ingest/monitor: news_verdict");
        assert_eq!(Job::Enrich.sampling(), (0.1, 320), "atlas-ingest: ENRICH_*");
        assert_eq!(Job::Analysis.sampling(), (0.2, 900), "ai.analyze_article");
        assert_eq!(Job::Briefing.sampling(), (0.35, 700), "atlas-web: briefing");
        assert_eq!(Job::Summarize.sampling(), (0.3, 1_200), "the summarize tool");
        assert_eq!(Job::Chat.sampling(), (0.3, 1_200), "ai.chat_simple");
        assert_eq!(
            Job::Agent.sampling(),
            (DEFAULT_TEMPERATURE, DEFAULT_MAX_TOKENS),
            "ai.py's agent: 0.25 / 2048"
        );
        assert_eq!(Job::AgentFinal.sampling(), (DEFAULT_TEMPERATURE, 1_024), "the last turn");
    }
}
