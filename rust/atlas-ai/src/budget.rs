//! The three numbers the reader sets, and which of them are machine guards.
//!
//! The model used to run on constants nobody could see: a 32 768-token window, 8 192
//! tokens of thinking and 8 192 tokens per answer, all compiled in. They are now
//! `data/config.json`'s `model_context`, `model_reasoning` and `model_answer`
//! ([`Budgets`]), and this module is the one place that turns them into what a request
//! carries.
//!
//! # Reader setting or machine guard
//!
//! * [`Budgets`] — a **reader setting**. A bigger window and a bigger thought cost
//!   time and memory the reader owns, so the app obeys and says what it obeys.
//! * [`crate::agent::MAX_STEPS`], [`crate::agent::TOTAL_TOKEN_BUDGET`],
//!   [`crate::agent::TURN_DEADLINE`] — **machine guards**. They do not express a
//!   preference; they stop a wedged model from holding a reader for hours, so they stay
//!   hard whatever the config says. A reader who wants more *work* raises the window and
//!   the answer budget; the guards still bound the run.
//!
//! # Which number goes where
//!
//! llama.cpp counts **one** completion budget, reasoning included, so a turn asks for
//! `model_reasoning + model_answer` tokens ([`Budgets::max_tokens`]). Thinking off
//! (`model_reasoning = 0`) means the request carries `model_answer` alone and no
//! `chat_template_kwargs.enable_thinking` — asking a model not to think while still
//! giving it room to think is how a small budget came back empty.
//!
//! The window ([`Budgets::context`]) is a promise `llama-server -c` has to keep. This
//! crate cannot set the flag (it does not spawn the server), so it uses the number for
//! its trimming decisions and, when the server reports something smaller, says so
//! ([`Budgets::against`]) instead of trimming in silence.

use std::ops::RangeInclusive;

use atlas_config::Config;

/// The window a request may be trimmed to, in tokens: `model_context`.
///
/// The floor is llama.cpp's smallest useful `-c` with a system prompt and one page in
/// front; the ceiling is what a 262 144-token model would need and what a machine with
/// this project's memory cannot exceed — a value outside it is clamped, never refused,
/// because a run with a wrong window is still a run and the note says what happened.
pub const CONTEXT_RANGE: RangeInclusive<usize> = 1024..=262_144;

/// Tokens the model may spend thinking: `model_reasoning` (`0` = do not think).
pub const REASONING_RANGE: RangeInclusive<u32> = 0..=32_768;

/// Tokens one final answer may use: `model_answer`.
pub const ANSWER_RANGE: RangeInclusive<u32> = 256..=32_768;

/// What a request is built from: the window, the thought, the answer.
///
/// `Copy` and small: a caller passes it by value to every run it starts, so there is no
/// second copy of the numbers to drift from the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    /// `model_context` — the window `llama-server` was started with, in tokens.
    pub context: usize,
    /// `model_reasoning` — tokens the model may spend thinking on one call.
    pub reasoning: u32,
    /// `model_answer` — tokens one answer may use.
    pub answer: u32,
}

impl Default for Budgets {
    fn default() -> Budgets {
        Budgets::DEFAULT
    }
}

impl Budgets {
    /// Today's shipped numbers: 32 768 / 8 192 / 8 192.
    ///
    /// The defaults are the ones measured on this machine (a one-line verdict spent 827
    /// tokens of reasoning before it wrote the answer), and they are what a caller with
    /// no config — a test, a tool — runs on.
    pub const DEFAULT: Budgets = Budgets {
        context: crate::context::CONTEXT_TOKENS,
        reasoning: crate::chat::DEFAULT_MAX_TOKENS,
        answer: crate::chat::DEFAULT_MAX_TOKENS,
    };

    /// Read the three numbers, clamping each into its range and reporting every clamp.
    ///
    /// The notes are not refusals: a window of `512` runs at `1024`, a thought of
    /// `999999` runs at `32 768`. `atlas-config` writes the note the reader sees in
    /// `/api/status`; this is the same decision made again at the point of use, so a
    /// caller that assembled the numbers by hand cannot hand llama.cpp a `max_tokens`
    /// that is not a number.
    pub fn new(context: u64, reasoning: u64, answer: u64) -> (Budgets, Vec<String>) {
        let mut notes = Vec::new();
        let context = clamp(
            context as usize,
            CONTEXT_RANGE,
            "model_context",
            &mut notes,
        );
        let reasoning = clamp(
            reasoning as u32,
            REASONING_RANGE,
            "model_reasoning",
            &mut notes,
        );
        let answer = clamp(answer as u32, ANSWER_RANGE, "model_answer", &mut notes);
        (
            Budgets {
                context,
                reasoning,
                answer,
            },
            notes,
        )
    }

    /// The three numbers as `data/config.json` has them, or the defaults.
    ///
    /// Read through [`Config::number`], the table-driven accessor: the config owner adds
    /// `model_context` / `model_reasoning` / `model_answer` to it, and until an older
    /// config file leaves a key out, the default is used. The raw file value is the
    /// fallback so a key the table does not know yet is still obeyed rather than
    /// silently ignored — a reader who writes `model_context: 65536` and gets 32 768
    /// back with no note has been lied to.
    pub fn from_config(config: &Config) -> (Budgets, Vec<String>) {
        let read = |key: &str, default: u64| -> u64 {
            if let Some(number) = config.number(key) {
                return number.max(0.0) as u64;
            }
            match config.stored.get(key) {
                Some(value) => value
                    .as_u64()
                    .or_else(|| value.as_i64().map(|n| n.max(0) as u64))
                    .unwrap_or(default),
                None => default,
            }
        };
        Budgets::new(
            read("model_context", Budgets::DEFAULT.context as u64),
            read("model_reasoning", Budgets::DEFAULT.reasoning as u64),
            read("model_answer", Budgets::DEFAULT.answer as u64),
        )
    }

    /// The three numbers as `<root>/data/config.json` has them.
    ///
    /// The convenience for a caller that does not link `atlas-config` itself (the pipeline):
    /// a config file that cannot be read is not an error here — the app runs on the shipped
    /// numbers, and every clamped or missing key is still named in the notes.
    pub fn for_root(root: impl AsRef<std::path::Path>) -> (Budgets, Vec<String>) {
        let paths = atlas_config::Paths::at(root.as_ref());
        match atlas_config::Config::load_at(&paths) {
            Ok(config) => Budgets::from_config(&config),
            Err(_) => (
                Budgets::DEFAULT,
                vec![format!(
                    "The settings file could not be read, so the model runs on the shipped \
                     numbers: {} tokens of context, {} to think, {} per answer.",
                    Budgets::DEFAULT.context,
                    Budgets::DEFAULT.reasoning,
                    Budgets::DEFAULT.answer
                )],
            ),
        }
    }

    /// May the model think? `model_reasoning = 0` is the reader saying no.
    pub fn thinking(&self) -> bool {
        self.reasoning > 0
    }

    /// What one call asks for: the thought and the answer, because llama.cpp counts them
    /// together. With thinking off this is [`Budgets::answer`] alone.
    pub fn max_tokens(&self) -> u32 {
        if self.thinking() {
            self.reasoning.saturating_add(self.answer)
        } else {
            self.answer
        }
    }

    /// The prompt estimate per call: a fifth of the window, never below the shipped
    /// [`crate::context::PROMPT_TOKENS`].
    ///
    /// A fifth leaves five calls' worth of room for schemas, tool results and the reply
    /// in a window the reader sized; the floor keeps a small window from trimming the
    /// system prompt away before the model has said anything.
    pub fn prompt_tokens(&self) -> usize {
        (self.context / 5).max(crate::context::PROMPT_TOKENS)
    }

    /// Reconcile with what the server says it has.
    ///
    /// `reported` is `/props`' `n_ctx`. A server loaded with `-c 8192` under a config that
    /// says 32 768 will not hold a 32 768-token prompt, and trimming to the configured
    /// number would be a silent truncation: the returned budgets use the smaller window
    /// and the second value is the sentence the reader is owed.
    pub fn against(&self, reported: Option<u64>) -> (Budgets, Option<String>) {
        let Some(reported) = reported else {
            return (*self, None);
        };
        let reported = reported as usize;
        if reported == 0 || reported >= self.context {
            return (*self, None);
        }
        let smaller = Budgets {
            context: reported,
            ..*self
        };
        (
            smaller,
            Some(format!(
                "The model server reports a {reported}-token context but this computer is \
                 configured for {} — answers are trimmed to the smaller window until \
                 llama.cpp is restarted with -c {}.",
                self.context, self.context
            )),
        )
    }
}

/// One number, inside its range, with a sentence when it was not.
fn clamp<T>(value: T, range: RangeInclusive<T>, key: &str, notes: &mut Vec<String>) -> T
where
    T: Copy + PartialOrd + std::fmt::Display,
{
    let (low, high) = (range.start(), range.end());
    let clamped = if value < *low {
        *low
    } else if value > *high {
        *high
    } else {
        value
    };
    if clamped != value {
        notes.push(format!(
            "{key} {value} is outside {low}..{high}; ATLAS uses {clamped}"
        ));
    }
    clamped
}

#[cfg(test)]
mod tests;
