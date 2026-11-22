//! Anchor ranges to result cards: a headline, a snippet, the text a date could be
//! written in.
//!
//! A card is found **structurally**, so this module knows no engine's markup:
//!
//! * the card is the tightest block element around a link that carries text
//!   outside the link itself. A headline inside its own heading element is not a
//!   card (that block holds nothing but the headline); the list item or the box
//!   around it is. So the walk starts at the tightest block and moves outwards
//!   until it finds prose.
//! * the snippet is the longest uninterrupted text run in that card which is not
//!   the headline and not the address the card links to — a search page printing
//!   the URL it is about to link to is furniture, not prose.
//! * the short runs in the card are what [`crate::dates`] turns into date
//!   candidates; `atlas-dates` decides which of them is a date.
//!
//! It decides no vocabulary: "short" is a word count, and a run that reads like a
//! date is whatever `atlas-dates` can parse. It does not resolve a link and does
//! not clean text.

use crate::html::{Anchor, Block, Markup};

/// The shortest snippet worth returning, in characters. A run below this is a
/// label ("Business", "2 hours ago"), not a summary.
pub const MIN_SNIPPET_CHARS: usize = 25;

/// How far outwards the walk for a card may go before it gives up. A link that
/// has no prose within six enclosing blocks has no snippet, and the whole document
/// is not a card.
pub const MAX_CLIMB: usize = 6;

/// A card is a card, not a page: a block larger than this is the list the cards
/// sit in, or the document, so the walk stops there and the tightest block is
/// used instead.
pub const MAX_CARD_BYTES: usize = 24_000;

/// One result card, as the page wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Card {
    /// The `href` as written (entities decoded, not resolved).
    pub href: String,
    /// The link text.
    pub title: String,
    pub snippet: String,
    /// The card's short text runs, shortest first — where a date could be.
    pub runs: Vec<String>,
}

/// Every card the page offers, in page order.
pub fn cards(markup: &Markup) -> Vec<Card> {
    let mut out: Vec<Card> = Vec::with_capacity(markup.anchors.len());
    for anchor in &markup.anchors {
        if let Some(card) = card_of(anchor, markup) {
            out.push(card);
        }
    }
    out
}

fn card_of(anchor: &Anchor, markup: &Markup) -> Option<Card> {
    let title = anchor.title.trim();
    if title.is_empty() {
        return None;
    }
    let block = enclosing_with_prose(anchor, markup)?;
    let mut snippet = String::new();
    // The snippet is the longest run, counted in characters — the same unit
    // [`MIN_SNIPPET_CHARS`] is expressed in. Comparing `str::len()` here would
    // compare bytes, so a 30-character sentence in a script that spends three bytes
    // per character beat a 60-character Latin one and the card showed the shorter
    // summary.
    let mut snippet_chars = 0usize;
    let mut runs: Vec<String> = Vec::new();
    for segment in markup.segments_within(block.start, block.end) {
        if segment.overlaps(anchor.start, anchor.end) {
            continue;
        }
        let text = segment.text.trim();
        if text.is_empty() || text == title || echo_of_a_link(text) {
            continue;
        }
        runs.push(text.to_string());
        let chars = text.chars().count();
        if chars >= MIN_SNIPPET_CHARS && chars > snippet_chars {
            snippet = text.to_string();
            snippet_chars = chars;
        }
    }
    Some(Card {
        href: anchor.href.clone(),
        title: title.to_string(),
        snippet,
        runs,
    })
}

/// The tightest block around the link that carries text besides the link.
///
/// The tightest block is always a candidate even when it carries nothing, so a
/// card whose only text is the headline still becomes a hit — with no snippet.
fn enclosing_with_prose(anchor: &Anchor, markup: &Markup) -> Option<Block> {
    let blocks = markup.enclosing(anchor.start, anchor.end);
    let tightest = *blocks.first()?;
    for block in blocks.iter().take(MAX_CLIMB) {
        let has_prose = markup
            .segments_within(block.start, block.end)
            .iter()
            .any(|segment| {
                !segment.overlaps(anchor.start, anchor.end)
                    && !segment.text.trim().is_empty()
                    && !echo_of_a_link(segment.text.trim())
            });
        if has_prose && block.len() <= MAX_CARD_BYTES {
            return Some(*block);
        }
    }
    Some(tightest)
}

/// Is this run the address the card links to (`publisher.com/2026/11/story`)
/// rather than prose? A run with no spaces that carries both a dot and a slash is
/// an address, whatever the engine calls the element it sits in.
fn echo_of_a_link(text: &str) -> bool {
    !text.contains(' ') && text.contains('/') && text.contains('.')
}
