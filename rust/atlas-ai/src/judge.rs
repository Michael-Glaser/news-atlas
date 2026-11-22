//! One judgement turn: a question whose answer is exactly one JSON object, asked in a
//! bounded sequence that cannot end in prose.
//!
//! The monitor's two questions ("is this reporting?", "how relevant is it?") are asked of a
//! thinking model with a few hundred tokens of budget. Measured on this machine: a one-line
//! JSON verdict spent **827 completion tokens of reasoning** before it wrote the answer, and
//! a batched check measured **~2.5 minutes per article**. A call with `max_tokens: 600` and
//! reasoning on therefore spends the whole budget thinking and comes back with an empty
//! answer channel — `finish_reason: length`, no object. Read strictly (one object or
//! nothing), that is "the model did not answer", and every caller then fell back to
//! whatever it had, which for the monitor was **scoring articles by the query's own words**:
//! the one thing the reader forbade, arrived at by a budget rather than by a decision.
//!
//! So a judgement is a *turn*, not a call, and it has three bounded steps:
//!
//! 1. **Think, with room to answer.** [`THINKING_TOKENS`] is reasoning *and* answer on one
//!    call; the whole reply must still be one object, parsed strictly, because the article
//!    sits inside the prompt and a model quoting the page would otherwise hand the page its
//!    own score.
//! 2. **Rescue, once.** The same question with the caller's one-line reminder appended,
//!    reasoning **off** and [`RESCUE_TOKENS`]: a reflex answer. Nothing else is in that
//!    reply, so the schema can be read out of it.
//! 3. **Read it out of prose.** The rescue's reply is scanned for balanced JSON objects and
//!    the *last* one carrying every required key wins — the model's own verdict comes after
//!    anything it quoted. This is the shape a model actually writes when it thinks out loud:
//!    a sentence, then `{"relevance": 10, ...}`.
//!
//! What is never done: a number the model did not state. If all three steps fail, the answer
//! is one sentence ([`NO_ANSWER`] and its variants) and the caller scores nothing — an
//! article kept unjudged is visible to the reader, a number invented here is not.

use serde_json::Value;

use crate::budget::Budgets;
use crate::chat::{ChatMessage, ChatReply, ChatRequest};
use crate::error::AiError;
use crate::prompts::cut_chars;
use crate::LlamaClient;

/// The temperature a judgement is asked at (`ai.analyze_for_monitor` used 0.2).
pub const TEMPERATURE: f32 = 0.2;

/// The first call's budget when the app has no reader setting to obey, in completion
/// tokens: reasoning **and** answer together.
///
/// Measured: 827 tokens of reasoning for a one-line verdict, ~2 500 for a judgement over a
/// 2 400-character article excerpt. 4 096 leaves the answer its ~150 tokens with room, and
/// keeps one judgement inside the quarter minute a local 9B model needs for it.
///
/// A run uses [`judgement_tokens`] instead: the thinking budget is the reader's
/// `model_reasoning` now, and a starved judgement was exactly the bug this budget used to
/// hide.
pub const THINKING_TOKENS: u32 = 4_096;

/// The rescue call's budget: no reasoning, one object, forced.
pub const RESCUE_TOKENS: u32 = 512;

/// What the answer half of a judgement needs: one object with a score, a few terms, a
/// sentence of reason and maybe four entities. 256 is several times what was measured.
pub const ANSWER_TOKENS: u32 = 256;

/// The first call's budget from the reader's numbers: the thought it allows, plus room to
/// write the answer inside the same completion (llama.cpp counts them together).
///
/// With `model_reasoning = 0` the judgement is a reflex: no thinking block, and the rescue
/// budget is the whole allowance.
pub fn judgement_tokens(budgets: &Budgets) -> u32 {
    if budgets.thinking() {
        budgets.reasoning.saturating_add(ANSWER_TOKENS)
    } else {
        RESCUE_TOKENS
    }
}

/// How much of a failed reply the refusal quotes back, in characters.
pub const GLIMPSE_CHARS: usize = 120;

/// One question and the shape its answer must have.
#[derive(Debug, Clone, Copy)]
pub struct Question<'a> {
    /// The prompt, exactly as the caller built it (article already fenced as untrusted).
    pub prompt: &'a str,
    /// One line restating the schema, appended to the **rescue** call only.
    ///
    /// Not appended to the first call: a reminder repeated on every call is how a prompt
    /// grows a second, weaker copy of its own instructions.
    pub reminder: &'a str,
    /// The keys the object must carry to count as the answer.
    pub required: &'a [&'a str],
    /// The untrusted text the prompt puts in front of the model — the page, the article.
    ///
    /// Provenance, not a word rule: an object that appears *inside this text* is the page's
    /// own words quoted back, so accepting it would let a page score itself. Empty means
    /// "the caller showed the model nothing it does not trust", and then no refusal can be
    /// made on provenance grounds.
    pub untrusted: &'a str,
}

impl<'a> Question<'a> {
    pub fn new(prompt: &'a str, reminder: &'a str, required: &'a [&'a str]) -> Question<'a> {
        Question {
            prompt,
            reminder,
            required,
            untrusted: "",
        }
    }

    /// Name the page the prompt shows, so a quoted object can be told from an answer.
    pub fn with_untrusted(mut self, untrusted: &'a str) -> Question<'a> {
        self.untrusted = untrusted;
        self
    }
}

/// What one judgement turn produced, and what it cost.
#[derive(Debug, Clone)]
pub struct Judged {
    pub value: Value,
    /// `1` when the thinking call answered, `2` when the rescue did, `3` when the rescue's
    /// prose had to be read. A caller that logs it is saying which path its number came from.
    pub step: u8,
    /// Completion tokens the server reported, summed over the calls that ran.
    pub tokens: u64,
    /// How many characters the model reasoned, summed: a zero here on a model that can think
    /// is the measurement that says the answer came from a reflex.
    pub thought_chars: usize,
}

/// Ask one judgement question and return the one object it answered with.
///
/// See the module comment for the three steps. `Err` is one sentence a reader can act on —
/// never a panic, never a partial answer dressed as one.
pub fn ask(
    client: &(dyn LlamaClient + Send + Sync),
    question: &Question<'_>,
) -> Result<Judged, String> {
    ask_with(client, question, &Budgets::DEFAULT)
}

/// [`ask`], with the reader's budgets: `model_reasoning` is what the thinking step may
/// spend, and `model_reasoning = 0` means the judgement is asked as a reflex (no thinking
/// block) with a small budget — the reader's answer, applied to a question the app asks
/// on its own.
pub fn ask_with(
    client: &(dyn LlamaClient + Send + Sync),
    question: &Question<'_>,
    budgets: &Budgets,
) -> Result<Judged, String> {
    let mut tokens = 0u64;
    let mut thought_chars = 0usize;
    // Set when a candidate object was refused because the page wrote it first. It changes the
    // refusal's sentence: "it quoted the page" is a different fact from "it said nothing".
    let mut quoted_page = false;
    let thinking = budgets.thinking();

    // 1. Think, with room left to answer.
    let first = complete(client, question.prompt, thinking, judgement_tokens(budgets))?;
    tokens += first.usage_tokens.unwrap_or(0);
    thought_chars += first.thinking.chars().count();
    match pick(&first.content, question, false) {
        Offer::Answer(value) => {
            return Ok(Judged {
                value,
                step: 1,
                tokens,
                thought_chars,
            })
        }
        Offer::Quoted => quoted_page = true,
        Offer::Absent => {}
    }
    // A reply that thought and then wrote nothing is the starvation this module exists for;
    // anything else is a model that answered in the wrong shape. Either way: ask once more.

    // 2. Rescue: the same question, the schema restated, no reasoning, a short budget.
    let rescued_prompt = format!("{}\n\n{}", question.prompt, question.reminder);
    let second = complete(client, &rescued_prompt, false, RESCUE_TOKENS)?;
    tokens += second.usage_tokens.unwrap_or(0);
    thought_chars += second.thinking.chars().count();
    match pick(&second.content, question, false) {
        Offer::Answer(value) => {
            return Ok(Judged {
                value,
                step: 2,
                tokens,
                thought_chars,
            })
        }
        Offer::Quoted => quoted_page = true,
        Offer::Absent => {}
    }

    // 3. Read the object out of what it wrote. Strictly one object is what was asked for;
    // this is the answer that came wrapped in a sentence, which is a real answer and the
    // reason a judgement should not be lost to punctuation.
    match pick(&second.content, question, true) {
        Offer::Answer(value) => {
            return Ok(Judged {
                value,
                step: 3,
                tokens,
                thought_chars,
            })
        }
        Offer::Quoted => quoted_page = true,
        Offer::Absent => {}
    }

    Err(no_answer(&first, &second, question, quoted_page))
}

/// What one reply offered to the turn.
enum Offer {
    /// An object of the asked shape that the page does not contain: the answer.
    Answer(Value),
    /// The right shape, but the page spells those bytes itself — a quote, not a judgement.
    Quoted,
    /// No object of that shape at all.
    Absent,
}

/// Read one reply: strictly (whole text, a fence unwrapped) or out of a sentence.
fn pick(content: &str, question: &Question<'_>, prose: bool) -> Offer {
    let found = if prose {
        from_prose(content, question.required)
    } else {
        object_in(content, question.required)
    };
    match found {
        Some(value) if quoted_back(&value, question.untrusted) => Offer::Quoted,
        Some(value) => Offer::Answer(value),
        None => Offer::Absent,
    }
}

/// Was this object written by the page rather than by the model?
///
/// The article sits inside the prompt, so a model that reasons out loud ("the article block
/// contains …") can emit the page's `{"relevance": 10}` as its whole answer — and step 3, which
/// exists to rescue an answer wrapped in a sentence, would read it out and score the page with
/// its own number. Comparing the object against the untrusted text settles it without a keyword
/// rule: if the page already spells those bytes, the page said it. Key order and spacing are
/// normalised by round-tripping through `serde_json` and dropping whitespace, so a reformatted
/// copy is still recognised. A false positive costs an unjudged article — the safe side; a
/// false negative hands a page its own verdict.
fn quoted_back(value: &Value, untrusted: &str) -> bool {
    if untrusted.trim().is_empty() {
        return false;
    }
    let Ok(compact) = serde_json::to_string(value) else {
        return false;
    };
    let squashed =
        |text: &str| -> String { text.chars().filter(|ch| !ch.is_whitespace()).collect() };
    squashed(untrusted).contains(&squashed(&compact))
}

/// One call. `thinking` off is what makes the rescue a reflex answer rather than a second
/// deliberation with the same budget problem.
///
/// A transient failure is retried **once**, by [`worth_retrying`]: a llama.cpp that accepted
/// the socket a moment before it was ready answered nothing, and one flaky second used to
/// turn a whole check into "the model did not answer". The retry lives here so both answers of
/// a judgement — and every caller of this turn — get it from one place.
fn complete(
    client: &(dyn LlamaClient + Send + Sync),
    prompt: &str,
    thinking: bool,
    max_tokens: u32,
) -> Result<ChatReply, String> {
    let mut request = ChatRequest::new(client.model().to_string(), vec![ChatMessage::user(prompt)]);
    request.temperature = TEMPERATURE;
    request.max_tokens = max_tokens;
    request.enable_thinking = thinking;
    match client.complete(&request) {
        Err(error) if worth_retrying(&error) => {
            client.complete(&request).map_err(|error| failure(&error))
        }
        other => other.map_err(|error| failure(&error)),
    }
}

/// Exactly one more attempt, for the failures a second identical question can fix: nothing was
/// listening yet (still loading), the connection dropped, the deadline passed mid-request, or
/// the server answered 5xx. A bad request, a prompt that does not fit the context and a
/// malformed body are not retried — asking again produces the same refusal.
pub fn worth_retrying(error: &AiError) -> bool {
    match error {
        AiError::Unreachable(_) | AiError::Timeout(_) => true,
        AiError::Http { status, .. } => *status >= 500,
        _ => false,
    }
}

fn failure(error: &AiError) -> String {
    format!(
        "The model could not be asked for a judgement: {}",
        one_line(&error.human())
    )
}

/// The answer when it is exactly one object carrying every required key.
///
/// A fence is unwrapped first, because models write it and it carries no claim of its own;
/// after that the whole text must parse, so prose around the object, two objects and a
/// fragment are all refused.
pub fn object_in(raw: &str, required: &[&str]) -> Option<Value> {
    let text = unfenced(raw);
    if !text.starts_with('{') {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    if carries(&value, required) {
        Some(value)
    } else {
        None
    }
}

/// The last balanced object in `raw` that carries every required key.
///
/// The *last*, not the first: the prompt holds the article, so a reply that quotes the page
/// quotes an object before its own — and the verdict is whatever it said after. Balanced
/// braces are found with string and escape awareness, so a `{` inside a quoted headline does
/// not shift the scan.
pub fn from_prose(raw: &str, required: &[&str]) -> Option<Value> {
    objects_in(raw)
        .into_iter()
        .rev()
        .find(|value| carries(value, required))
}

/// Every balanced `{…}` in `raw` that parses as a JSON object, in the order written.
pub fn objects_in(raw: &str) -> Vec<Value> {
    let text = unfenced(raw);
    let bytes: Vec<char> = text.chars().collect();
    let mut out: Vec<Value> = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] != '{' {
            at += 1;
            continue;
        }
        match balanced(&bytes, at) {
            Some(end) => {
                let candidate: String = bytes[at..=end].iter().collect();
                if let Ok(value) = serde_json::from_str::<Value>(&candidate) {
                    if value.is_object() {
                        out.push(value);
                    }
                }
                // Continue *inside* the object too: an answer wrapped around another object
                // would otherwise hide it.
                at += 1;
            }
            None => at += 1,
        }
    }
    out
}

/// The index of the `}` that closes the `{` at `start`, respecting strings and escapes.
fn balanced(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in chars.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *ch == '\\' {
                escaped = true;
            } else if *ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// The reply without one surrounding ``` fence (with or without a language tag).
///
/// Shared with [`crate::brief`]: both readers take an answer a model wrote, and both have to
/// accept a fence around it without accepting prose.
pub(crate) fn unfenced(raw: &str) -> &str {
    let text = raw.trim();
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let body = match rest.find('\n') {
        Some(newline) => &rest[newline + 1..],
        None => rest.trim_start_matches('`'),
    };
    let body = body.trim_end();
    body.strip_suffix("```").unwrap_or(body).trim()
}

/// Every required key is present. Values are not checked here: what a key's value may be is
/// the caller's rule (`atlas-monitor` refuses `42` as a score, and names it).
fn carries(value: &Value, required: &[&str]) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    required.iter().all(|key| map.contains_key(*key))
}

/// The one sentence a caller shows when the turn produced no object.
///
/// It says what was asked for, and quotes what the model wrote — a judgement that failed is
/// a fact the reader is owed, and the glimpse is what makes it checkable instead of a shrug.
fn no_answer(
    first: &ChatReply,
    second: &ChatReply,
    question: &Question<'_>,
    quoted_page: bool,
) -> String {
    if quoted_page {
        // The shape was right and the bytes were the page's. Saying so is worth more than
        // quoting the reply: the reader learns the page tried, not that the model mumbled.
        return format!(
            "The model wrote back the page's own text instead of a judgement of it ({}).",
            wanted(question.required)
        );
    }
    let required = question.required;
    let glimpse = if !second.content.trim().is_empty() {
        second.content.trim()
    } else if !first.thinking.trim().is_empty() {
        // No answer twice, but it did think: quote the working, which is what a reader can
        // check, and say how much of it there was.
        return format!(
            "The model reasoned for {} characters and never answered with the JSON object that \
             was asked for ({}).",
            first.thinking.chars().count(),
            wanted(required)
        );
    } else {
        first.content.trim()
    };
    if glimpse.is_empty() {
        return format!(
            "The model answered nothing for the judgement that was asked for ({}).",
            wanted(required)
        );
    }
    format!(
        "The model answered \"{}\" and not the JSON object that was asked for ({}).",
        one_line(&cut_chars(glimpse, GLIMPSE_CHARS)),
        wanted(required)
    )
}

fn wanted(required: &[&str]) -> String {
    if required.is_empty() {
        "one JSON object".to_string()
    } else {
        format!("the keys {}", required.join(", "))
    }
}

/// One line, no control characters: a reply is page-shaped text and may hold anything.
fn one_line(text: &str) -> String {
    let folded: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    cut_chars(folded.trim(), GLIMPSE_CHARS)
}

#[cfg(test)]
mod tests;
