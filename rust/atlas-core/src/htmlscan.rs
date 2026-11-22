//! `htmlscan` — the tag scanner: one pass over the page, no regex, no DOM.
//!
//! Decides where a tag starts and ends (quoted attributes included), which
//! elements are removed with their content, which are readable frames
//! (`article`, `p`), which block tags leave a word boundary behind, what text
//! sits in a byte range, and where the `ld+json` blocks are.
//!
//! It does not decide which text is the article: it reports every candidate
//! range and every paragraph, and `extract` chooses. It never builds a tree, so
//! nothing here needs memory proportional to the nesting of a hostile page.

use crate::text;

/// Elements whose content is dropped before text extraction.
pub fn is_removed(name: &str) -> bool {
    const REMOVED: &[&str] = &[
        "script", "style", "noscript", "svg", "nav", "header", "footer", "aside", "form", "iframe", "figure",
        "template",
    ];
    REMOVED.iter().any(|r| name.eq_ignore_ascii_case(*r))
}

/// Tags that should leave a word boundary behind when dropped.
fn is_block_tag(name: &str) -> bool {
    const BLOCK: &[&str] = &[
        "div", "p", "br", "li", "ul", "ol", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article", "main",
        "td", "th", "tr", "table", "blockquote", "pre", "hr", "dd", "dt", "dl", "figcaption", "caption",
    ];
    BLOCK.iter().any(|b| name.eq_ignore_ascii_case(*b))
}

#[derive(Debug, Clone, Copy)]
pub enum Token<'a> {
    Text(&'a str),
    Start {
        name: &'a str,
        attrs: &'a str,
        self_closing: bool,
        lt: usize,
        content_start: usize,
    },
    End {
        name: &'a str,
        lt: usize,
    },
    Other,
}

struct Frame {
    /// True for `<article>`, false for `<p>`.
    article: bool,
    buf: String,
    start: usize,
}

#[derive(Debug)]
pub struct RangeText {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Debug)]
pub struct Scan {
    pub articles: Vec<RangeText>,
    pub paragraphs: Vec<RangeText>,
    /// Every visible character of the page, block boundaries as newlines.
    /// Used only as a last resort when a page has no `<p>` or `<article>`.
    pub text: String,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b':' || b == b'_'
}

fn find_from(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() || hay.len() - from < needle.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub struct Scanner<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(src: &'a str) -> Self {
        Scanner {
            src,
            b: src.as_bytes(),
            pos: 0,
        }
    }

    /// Byte offset just past the token most recently returned.
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn next_token(&mut self) -> Option<Token<'a>> {
        let src = self.src;
        let bytes = self.b;
        let len = bytes.len();
        if self.pos >= len {
            return None;
        }
        let start = self.pos;
        let mut j = start;
        while j < len && bytes[j] != b'<' {
            j += 1;
        }
        if j > start {
            self.pos = j;
            return Some(Token::Text(src.get(start..j)?));
        }
        // At a '<'. A lone trailing '<' is text.
        if start + 1 >= len {
            self.pos = len;
            return Some(Token::Text(src.get(start..len)?));
        }
        let c = bytes[start + 1];
        if c == b'!' {
            let end = if bytes[start..].starts_with(b"<!--") {
                find_from(bytes, start + 4, b"-->").map(|k| k + 3)
            } else if bytes[start..].starts_with(b"<![CDATA[") {
                find_from(bytes, start + 9, b"]]>").map(|k| k + 3)
            } else {
                find_from(bytes, start + 2, b">").map(|k| k + 1)
            };
            self.pos = end.unwrap_or(len);
            return Some(Token::Other);
        }
        if c == b'?' {
            self.pos = find_from(bytes, start + 2, b">")
                .map(|k| k + 1)
                .unwrap_or(len);
            return Some(Token::Other);
        }
        if c == b'/' {
            let mut k = start + 2;
            while k < len && is_ws(bytes[k]) {
                k += 1;
            }
            let name_start = k;
            while k < len && is_name_char(bytes[k]) {
                k += 1;
            }
            let name_end = k;
            let after = find_from(bytes, k, b">").map(|g| g + 1).unwrap_or(len);
            self.pos = after;
            if name_end == name_start {
                return Some(Token::Other);
            }
            return Some(Token::End {
                name: src.get(name_start..name_end)?,
                lt: start,
            });
        }
        if !is_name_start(c) {
            // Stray '<' that does not open a tag: emit it as text.
            self.pos = start + 1;
            return Some(Token::Text(src.get(start..start + 1)?));
        }
        let mut k = start + 1;
        let name_start = k;
        while k < len && is_name_char(bytes[k]) {
            k += 1;
        }
        let name_end = k;
        let attrs_start = k;
        let mut quote: Option<u8> = None;
        let mut gt: Option<usize> = None;
        while k < len {
            let byte = bytes[k];
            match quote {
                Some(q) => {
                    if byte == q {
                        quote = None;
                    }
                }
                None => {
                    if byte == b'"' || byte == b'\'' {
                        quote = Some(byte);
                    } else if byte == b'>' {
                        gt = Some(k);
                        break;
                    }
                }
            }
            k += 1;
        }
        let gt_pos = gt.unwrap_or(len);
        let mut attrs_stop = gt_pos;
        let mut self_closing = false;
        if gt_pos > attrs_start && bytes[gt_pos - 1] == b'/' {
            self_closing = true;
            attrs_stop = gt_pos - 1;
        }
        let content_start = if gt.is_some() { gt_pos + 1 } else { len };
        self.pos = content_start;
        Some(Token::Start {
            name: src.get(name_start..name_end)?,
            attrs: src.get(attrs_start..attrs_stop)?,
            self_closing,
            lt: start,
            content_start,
        })
    }
}

fn append_text(frames: &mut Vec<Frame>, chunk: &str, loose: &mut String) {
    loose.push_str(chunk);
    if frames.is_empty() {
        return;
    }
    let top = frames.len() - 1;
    if let Some(frame) = frames.get_mut(top) {
        frame.buf.push_str(chunk);
    }
    for (idx, frame) in frames.iter_mut().enumerate() {
        if idx != top && frame.article {
            frame.buf.push_str(chunk);
        }
    }
}

fn append_sep(frames: &mut Vec<Frame>, loose: &mut String) {
    loose.push('\n');
    for frame in frames.iter_mut() {
        frame.buf.push(' ');
    }
}

/// Start tag of a readable element: opens `<p>` / `<article>` frames.
fn start_element(
    frames: &mut Vec<Frame>,
    name: &str,
    self_closing: bool,
    lt: usize,
    articles: &mut Vec<RangeText>,
    paragraphs: &mut Vec<RangeText>,
    loose: &mut String,
) {
    if name.eq_ignore_ascii_case("br") {
        append_sep(frames, loose);
    } else if name.eq_ignore_ascii_case("p") {
        loose.push('\n');
        if !self_closing {
            close_kind(frames, false, lt, articles, paragraphs);
            frames.push(Frame {
                article: false,
                buf: String::new(),
                start: lt,
            });
        }
    } else if name.eq_ignore_ascii_case("article") {
        loose.push('\n');
        if !self_closing {
            frames.push(Frame {
                article: true,
                buf: String::new(),
                start: lt,
            });
        }
    } else if is_block_tag(name) {
        append_sep(frames, loose);
    }
}

/// End tag: closes the innermost frame of the matching kind.
fn end_element(
    frames: &mut Vec<Frame>,
    name: &str,
    end: usize,
    articles: &mut Vec<RangeText>,
    paragraphs: &mut Vec<RangeText>,
    loose: &mut String,
) {
    if is_block_tag(name) {
        append_sep(frames, loose);
    }
    if name.eq_ignore_ascii_case("article") {
        close_kind(frames, true, end, articles, paragraphs);
    } else if name.eq_ignore_ascii_case("p") {
        close_kind(frames, false, end, articles, paragraphs);
    }
}

/// Removed elements whose content is script or stylesheet text, not markup:
/// inside them a `<article>` or `<p>` token is just a string in code.
fn skip_is_code(open: &[&str]) -> bool {
    const CODE: &[&str] = &["script", "style", "noscript", "template"];
    match open.last() {
        Some(name) => CODE.iter().any(|c| name.eq_ignore_ascii_case(*c)),
        None => false,
    }
}

/// Start tags that are never *inside* a removed block, so seeing one means
/// the removed element was never closed (common for iframe / header / form).
fn ends_skip(name: &str) -> bool {
    const OUTER: &[&str] = &["body", "html", "main", "article", "section", "h1"];
    OUTER.iter().any(|o| name.eq_ignore_ascii_case(*o))
}

fn is_page_end(name: &str) -> bool {
    name.eq_ignore_ascii_case("body") || name.eq_ignore_ascii_case("html")
}

/// Close the innermost frame of the wanted kind (and any frames above it).
fn close_kind(
    frames: &mut Vec<Frame>,
    want_article: bool,
    end: usize,
    articles: &mut Vec<RangeText>,
    paragraphs: &mut Vec<RangeText>,
) {
    let mut target: Option<usize> = None;
    for i in (0..frames.len()).rev() {
        if frames[i].article == want_article {
            target = Some(i);
            break;
        }
    }
    let target = match target {
        Some(i) => i,
        None => return,
    };
    while frames.len() > target {
        if let Some(frame) = frames.pop() {
            let record = RangeText {
                start: frame.start,
                end,
                text: text::normalize(&frame.buf),
            };
            if frame.article {
                articles.push(record);
            } else {
                paragraphs.push(record);
            }
        }
    }
}

/// One pass over the page: drop removed elements, collect every `<article>`
/// range and every `<p>` paragraph with its byte range.
pub fn scan(src: &str) -> Scan {
    let mut sc = Scanner::new(src);
    let mut frames: Vec<Frame> = Vec::new();
    let mut articles: Vec<RangeText> = Vec::new();
    let mut paragraphs: Vec<RangeText> = Vec::new();
    let mut loose = String::new();
    // Removed elements that are still open. A page that forgets one closing
    // tag must not lose the rest of the document (`ends_skip`), and a stray
    // `</nav>` with nothing open must not push the counter negative.
    let mut skip: Vec<&str> = Vec::new();

    while let Some(token) = sc.next_token() {
        let end_pos = sc.pos();
        match token {
            Token::Text(chunk) => {
                if skip.is_empty() {
                    append_text(&mut frames, chunk, &mut loose);
                }
            }
            Token::Start {
                name,
                self_closing,
                lt,
                ..
            } => {
                if !skip.is_empty() {
                    if is_removed(name) {
                        if !self_closing {
                            skip.push(name);
                        }
                        continue;
                    }
                    if skip_is_code(&skip) || !ends_skip(name) {
                        continue;
                    }
                    // Unclosed removed block: leave it and read this element.
                    skip.clear();
                } else if is_removed(name) {
                    if !self_closing {
                        skip.push(name);
                    }
                    continue;
                }
                start_element(
                    &mut frames,
                    name,
                    self_closing,
                    lt,
                    &mut articles,
                    &mut paragraphs,
                    &mut loose,
                );
            }
            Token::End { name, .. } => {
                if !skip.is_empty() {
                    if is_removed(name) {
                        if let Some(pos) = skip
                            .iter()
                            .rposition(|open| open.eq_ignore_ascii_case(name))
                        {
                            skip.truncate(pos);
                        }
                    } else if is_page_end(name) {
                        skip.clear();
                    }
                    continue;
                }
                end_element(
                    &mut frames,
                    name,
                    end_pos,
                    &mut articles,
                    &mut paragraphs,
                    &mut loose,
                );
            }
            Token::Other => {}
        }
    }

    while let Some(frame) = frames.pop() {
        let record = RangeText {
            start: frame.start,
            end: src.len(),
            text: text::normalize(&frame.buf),
        };
        if frame.article {
            articles.push(record);
        } else {
            paragraphs.push(record);
        }
    }

    Scan {
        articles,
        paragraphs,
        text: loose,
    }
}

/// Inner bodies of every `<script type="application/ld+json">` block.
pub fn jsonld_article_bodies(src: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut sc = Scanner::new(src);
    let mut body_start: Option<usize> = None;
    while let Some(token) = sc.next_token() {
        match token {
            Token::Start {
                name,
                attrs,
                content_start,
                ..
            } => {
                if name.eq_ignore_ascii_case("script") {
                    let lower = attrs.to_ascii_lowercase();
                    body_start = if lower.contains("ld+json") {
                        Some(content_start)
                    } else {
                        None
                    };
                }
            }
            Token::End { name, lt } => {
                if name.eq_ignore_ascii_case("script") {
                    if let Some(body_offset) = body_start {
                        if lt >= body_offset {
                            if let Some(body) = src.get(body_offset..lt) {
                                out.push(body.to_string());
                            }
                        }
                    }
                    body_start = None;
                }
            }
            _ => {}
        }
    }
    out
}

/// Plain text of a small HTML fragment (tags dropped, entities decoded).
pub fn plain(fragment: &str) -> String {
    let mut sc = Scanner::new(fragment);
    let mut raw = String::new();
    while let Some(token) = sc.next_token() {
        if let Token::Text(chunk) = token {
            raw.push_str(chunk);
            raw.push(' ');
        }
    }
    text::normalize(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_reads_tags_text_attributes_and_comments() {
        let src = "<p class=\"a>b\">Hi &amp; there</p><!-- a > b -->";
        let mut sc = Scanner::new(src);
        let mut names: Vec<String> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        let mut saw_class = false;
        while let Some(token) = sc.next_token() {
            match token {
                Token::Start { name, attrs, .. } => {
                    names.push(format!("s:{}", name));
                    saw_class = attrs.contains("class");
                }
                Token::End { name, .. } => names.push(format!("e:{}", name)),
                Token::Text(chunk) => texts.push(chunk.to_string()),
                Token::Other => {}
            }
        }
        assert_eq!(names, vec!["s:p".to_string(), "e:p".to_string()]);
        assert_eq!(texts.join("|"), "Hi &amp; there");
        assert!(saw_class, "attribute slice should contain class");
    }

    #[test]
    fn scanner_handles_unclosed_tags_and_stray_angle_brackets() {
        let src = "a < b <div>x";
        let mut sc = Scanner::new(src);
        let mut texts: Vec<String> = Vec::new();
        let mut starts = 0usize;
        while let Some(token) = sc.next_token() {
            match token {
                Token::Text(chunk) => texts.push(chunk.to_string()),
                Token::Start { .. } => starts += 1,
                _ => {}
            }
        }
        let joined = texts.join("");
        assert_eq!(starts, 1);
        assert!(joined.contains('a'));
        assert!(joined.contains('b'));
        assert!(joined.contains('x'));
    }

    #[test]
    fn removed_blocks_are_dropped() {
        let html = "<html><body><nav>menu menu</nav><article>\
            <p>This is a real article sentence that is long enough to keep.</p>\
            <script>var x = 1 < 2;</script></article>\
            <footer>footer junk</footer></body></html>";
        let scan = scan(html);
        assert_eq!(scan.paragraphs.len(), 1);
        assert!(scan.paragraphs[0].text.contains("real article"));
        assert_eq!(scan.articles.len(), 1);
        assert!(!scan.articles[0].text.contains("var x"));
        assert!(!scan.articles[0].text.contains("menu"));
        assert!(!scan.articles[0].text.contains("footer junk"));
    }

    #[test]
    fn unclosed_removed_block_keeps_the_rest_of_the_page() {
        let html = "<html><body><header><nav>menu</nav><article>\
            <p>This is a real article sentence that is long enough to keep.</p>\
            </article></body></html>";
        let scan = scan(html);
        assert_eq!(scan.paragraphs.len(), 1);
        assert!(scan.paragraphs[0].text.contains("real article"));
        assert!(!scan.text.contains("menu"));
    }

    #[test]
    fn stray_closing_tag_does_not_change_what_follows() {
        let html = "<html><body></nav><article>\
            <p>This is a real article sentence that is long enough to keep.</p>\
            </article></body></html>";
        let scan = scan(html);
        assert_eq!(scan.paragraphs.len(), 1);
        assert!(scan.paragraphs[0].text.contains("real article"));
    }
}
