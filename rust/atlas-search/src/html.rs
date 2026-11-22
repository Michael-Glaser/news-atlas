//! The structural scan of a result page: blocks, text segments, anchors.
//!
//! A search result page is a list of cards, and every engine writes it
//! differently — so this module reads **no engine's markup**. It answers three
//! structural questions with one pass of `atlas-core`'s tag scanner
//! ([`atlas_core::htmlscan::Scanner`]):
//!
//! * which byte range belongs to a block element ([`Block`]),
//! * which text sits in one uninterrupted run between block boundaries, with its
//!   range ([`Segment`]),
//! * which links the page carries, with the range their own text occupies
//!   ([`Anchor`]).
//!
//! [`crate::cards`] then puts those three answers together: a link's card is the
//! smallest block around it that carries enough text to be something other than
//! the headline, its snippet is the longest text run in that block which is not
//! the headline, and a date is looked for among the short runs.
//!
//! It decides nothing about what is news, what is an article or which host is
//! allowed, and it never builds a tree: a hostile page cannot make the memory
//! grow faster than the page itself, and every list is capped.
//!
//! It does not decode a URL ([`crate::link`]) and does not clean text
//! (`atlas-guard`): the ranges are raw, the text is `atlas-core`'s plain text.

use atlas_core::htmlscan::{plain, Scanner, Token};

/// Anchors kept per page. A result page has tens; a hostile one may have tens of
/// thousands, and the caps are why it costs the same as a normal page.
pub const MAX_ANCHORS: usize = 400;

/// Text segments kept per page.
pub const MAX_SEGMENTS: usize = 8_000;

/// Block ranges kept per page.
pub const MAX_BLOCKS: usize = 8_000;

/// Elements whose text is never a result: the scanner's own removal list decides
/// which (`atlas_core::htmlscan::is_removed`), so there is no second copy here.
const BLOCK_TAGS: &[&str] = &[
    "div",
    "p",
    "li",
    "ul",
    "ol",
    "dl",
    "dd",
    "dt",
    "td",
    "th",
    "tr",
    "table",
    "tbody",
    "thead",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "article",
    "section",
    "main",
    "header",
    "footer",
    "nav",
    "aside",
    "form",
    "figure",
    "figcaption",
    "blockquote",
    "br",
    "body",
];

/// One block element's content range, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub start: usize,
    pub end: usize,
}

impl Block {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub fn holds(&self, start: usize, end: usize) -> bool {
        self.start <= start && end <= self.end
    }
}

/// One uninterrupted run of text between block boundaries, with its range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl Segment {
    pub fn overlaps(&self, start: usize, end: usize) -> bool {
        self.start < end && start < self.end
    }
}

/// One link, with the range its own text occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// The `href` as written, entities decoded, not yet resolved.
    pub href: String,
    /// The byte range of the link's text (`content_start .. </a>`).
    pub start: usize,
    pub end: usize,
    /// The link text, already plain.
    pub title: String,
}

/// What one pass over a page produced.
#[derive(Debug, Clone, Default)]
pub struct Markup {
    pub blocks: Vec<Block>,
    pub segments: Vec<Segment>,
    pub anchors: Vec<Anchor>,
}

impl Markup {
    /// Every block holding `start..end`, smallest first — the card boundaries the
    /// page offers, from the tightest one outwards.
    ///
    /// The order is the point: a headline sits in its own `<h2>` and that block
    /// carries nothing but the headline, so the search for a snippet starts at the
    /// tightest block and walks outwards until it finds one with prose in it.
    pub fn enclosing(&self, start: usize, end: usize) -> Vec<Block> {
        let mut found: Vec<Block> = self
            .blocks
            .iter()
            .filter(|block| block.holds(start, end))
            .copied()
            .collect();
        found.sort_by_key(|block| block.len());
        found
    }

    /// The text runs inside one range, in page order.
    ///
    /// Segments are pushed as the text is read, so they are ordered by `start` and
    /// never overlap; that is what lets this be a range lookup instead of a scan of
    /// the page. It matters: [`crate::cards`] asks once per candidate block per
    /// link, and on a 400-link page the scan was 8.7 ms against 4.3 ms for reading
    /// the page at all (`the_page_is_read_once_and_its_ranges_are_looked_up`).
    pub fn segments_within(&self, start: usize, end: usize) -> Vec<&Segment> {
        let from = self.segments.partition_point(|segment| segment.start < start);
        self.segments[from..]
            .iter()
            .take_while(|segment| segment.start < end)
            .filter(|segment| segment.end <= end)
            .collect()
    }

    /// Are the segments in the order [`Markup::segments_within`] assumes?
    ///
    /// A debugging question, not a policy: a text run is pushed when it ends, so the
    /// segments are in page order, and the lookup above is only correct while that
    /// holds. Blocks are *not* — a block is pushed when it closes, so `blocks` is
    /// ordered by `end`, and [`Markup::enclosing`] has to scan them all.
    pub fn segments_are_ordered(&self) -> bool {
        self.segments
            .windows(2)
            .all(|pair| pair[0].start <= pair[1].start)
    }
}

/// Scan a page once.
pub fn scan(src: &str) -> Markup {
    let mut scanner = Scanner::new(src);
    let mut markup = Markup::default();
    // Open block elements: (name, content start).
    let mut open_blocks: Vec<(&str, usize)> = Vec::new();
    // Open links: (content start, href).
    let mut open_anchors: Vec<(usize, String)> = Vec::new();
    // Open removed elements (`script`, `style`, …): their text is not text.
    let mut removed: Vec<&str> = Vec::new();
    let mut seg = String::new();
    let mut seg_start = 0usize;
    let mut seg_end = 0usize;
    let mut seg_open = false;

    while let Some(token) = scanner.next_token() {
        match token {
            Token::Text(chunk) => {
                if !removed.is_empty() || chunk.trim().is_empty() {
                    continue;
                }
                if !seg_open {
                    seg_start = scanner.pos().saturating_sub(chunk.len());
                    seg_open = true;
                }
                // Inline markup splits one sentence into chunks; a space between
                // them keeps `12<b>%</b>` from reading as `12%`-but-joined.
                seg.push(' ');
                seg.push_str(chunk);
                seg_end = scanner.pos();
            }
            Token::Start {
                name,
                attrs,
                self_closing,
                content_start,
                ..
            } => {
                // Inside `script`/`style`/`noscript` every token is script text,
                // including the `<a href>` a page hides in a string literal.
                if !removed.is_empty() {
                    if atlas_core::htmlscan::is_removed(name) && !self_closing {
                        removed.push(name);
                    }
                    continue;
                }
                if atlas_core::htmlscan::is_removed(name) {
                    flush(&mut markup, &mut seg, &mut seg_open, seg_start, seg_end);
                    if !self_closing {
                        removed.push(name);
                    }
                    continue;
                }
                if name.eq_ignore_ascii_case("a") {
                    flush(&mut markup, &mut seg, &mut seg_open, seg_start, seg_end);
                    if !self_closing {
                        open_anchors.push((content_start, attr(attrs, "href")));
                    }
                    continue;
                }
                if is_block(name) {
                    flush(&mut markup, &mut seg, &mut seg_open, seg_start, seg_end);
                    if !self_closing && !name.eq_ignore_ascii_case("br") {
                        if open_blocks.len() < MAX_BLOCKS {
                            open_blocks.push((name, content_start));
                        }
                    }
                }
            }
            Token::End { name, lt } => {
                if let Some(top) = removed.last() {
                    if !name.eq_ignore_ascii_case(top) {
                        continue;
                    }
                    removed.pop();
                    continue;
                }
                if name.eq_ignore_ascii_case("a") {
                    flush(&mut markup, &mut seg, &mut seg_open, seg_start, seg_end);
                    if let Some((start, href)) = open_anchors.pop() {
                        push_anchor(&mut markup, src, start, lt, href);
                    }
                    continue;
                }
                if is_block(name) {
                    flush(&mut markup, &mut seg, &mut seg_open, seg_start, seg_end);
                    close_block(&mut markup, &mut open_blocks, name, lt);
                }
            }
            Token::Other => {}
        }
    }

    // A page that ends inside a tag: close what is still open at the doc end.
    flush(&mut markup, &mut seg, &mut seg_open, seg_start, seg_end);
    let end = src.len();
    while let Some((start, href)) = open_anchors.pop() {
        push_anchor(&mut markup, src, start, end, href);
    }
    while let Some((_, start)) = open_blocks.pop() {
        if markup.blocks.len() < MAX_BLOCKS {
            markup.blocks.push(Block { start, end });
        }
    }
    markup
}

fn flush(markup: &mut Markup, seg: &mut String, open: &mut bool, start: usize, end: usize) {
    if !*open {
        return;
    }
    let text = atlas_core::text::normalize(seg);
    seg.clear();
    *open = false;
    if text.is_empty() || markup.segments.len() >= MAX_SEGMENTS {
        return;
    }
    markup.segments.push(Segment { start, end, text });
}

fn push_anchor(markup: &mut Markup, src: &str, start: usize, end: usize, href: String) {
    if markup.anchors.len() >= MAX_ANCHORS || href.trim().is_empty() {
        return;
    }
    let title = match src.get(start..end) {
        Some(fragment) => plain(fragment),
        None => String::new(),
    };
    markup.anchors.push(Anchor {
        href,
        start,
        end,
        title,
    });
}

fn close_block(markup: &mut Markup, open: &mut Vec<(&str, usize)>, name: &str, lt: usize) {
    let Some(index) = open
        .iter()
        .rposition(|(opened, _)| opened.eq_ignore_ascii_case(name))
    else {
        return;
    };
    let (_, start) = open[index];
    // Everything opened inside an unclosed element goes with it: the range is
    // still the truth, and a page cannot hand us a range that never closed.
    open.truncate(index);
    if markup.blocks.len() < MAX_BLOCKS && start <= lt {
        markup.blocks.push(Block { start, end: lt });
    }
}

fn is_block(name: &str) -> bool {
    BLOCK_TAGS.iter().any(|tag| name.eq_ignore_ascii_case(tag))
}

/// One attribute's value, entities decoded. `""` when it is not there.
pub fn attr(attrs: &str, name: &str) -> String {
    let bytes = attrs.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Attribute-name boundary: start of the string or whitespace before it.
        let boundary = i == 0 || (bytes[i - 1] as char).is_ascii_whitespace();
        if boundary
            && attrs.len() >= i + name.len()
            && attrs
                .get(i..i + name.len())
                .map(|slice| slice.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        {
            let after = i + name.len();
            let mut k = after;
            while k < bytes.len() && (bytes[k] as char).is_ascii_whitespace() {
                k += 1;
            }
            if k < bytes.len() && bytes[k] == b'=' {
                k += 1;
                while k < bytes.len() && (bytes[k] as char).is_ascii_whitespace() {
                    k += 1;
                }
                if k < bytes.len() && (bytes[k] == b'"' || bytes[k] == b'\'') {
                    let quote = bytes[k];
                    let value_start = k + 1;
                    let mut end = value_start;
                    while end < bytes.len() && bytes[end] != quote {
                        end += 1;
                    }
                    let raw = attrs.get(value_start..end).unwrap_or("");
                    return atlas_core::text::decode_entities(raw);
                }
                let mut end = k;
                while end < bytes.len() && !(bytes[end] as char).is_ascii_whitespace() {
                    end += 1;
                }
                let raw = attrs.get(k..end).unwrap_or("");
                return atlas_core::text::decode_entities(raw);
            }
        }
        i += 1;
    }
    String::new()
}
