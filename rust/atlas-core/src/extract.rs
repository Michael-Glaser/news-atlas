//! `extract` — the body chooser: candidates in, the best body out.
//!
//! Decides the order the candidates are tried in (JSON-LD `articleBody`, then
//! the largest `<article>`, then the longest run of `<p>` paragraphs, then the
//! whole page), the minimum text worth calling an article (`MIN_TEXT_CHARS`), how
//! paragraph runs are split (`RUN_GAP`), when a title is recovered from the page,
//! and how sentences a page glued together with a tag get their break back
//! (`unglue_sentences`).
//!
//! It does not scan HTML itself (`htmlscan` does, once) and it does not count
//! entities (`entities` does): this module sees finished candidate strings and
//! returns the one the pipeline should trust.

use crate::htmlscan::{self, RangeText};
use crate::json::{self, Json};
use crate::text;

/// Below this many characters the chosen text is short enough to look again
/// at the paragraph run (and, when it is really thin, at the whole page).
const MIN_TEXT_CHARS: usize = 400;
/// Paragraph gaps (in page bytes) larger than this start a new run.
const RUN_GAP: usize = 4000;

pub fn run(root: &Json) -> Result<String, String> {
    let html = match root.get("html").and_then(Json::as_str) {
        Some(h) => h,
        None => return Err("missing \"html\" string".to_string()),
    };
    let _url = root.get("url").and_then(Json::as_str).unwrap_or("");
    let hint = root
        .get("title")
        .and_then(Json::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    let scan = htmlscan::scan(html);
    let paragraph_run = best_paragraph_run(&scan.paragraphs);
    let article_text = best_article(&scan.articles, &scan.paragraphs);
    let jsonld_text = best_jsonld(html);

    // Prefer JSON-LD articleBody, then the largest <article>, then <p> runs.
    let mut chosen = String::new();
    if let Some(found) = jsonld_text {
        if !found.is_empty() {
            chosen = found;
        }
    }
    if chosen.is_empty() {
        if let Some(found) = article_text {
            chosen = found;
        }
    }
    if chosen.is_empty() {
        chosen = paragraph_run.clone();
    }

    let mut chosen_len = chosen.chars().count();
    let run_len = paragraph_run.chars().count();
    // A thin pick is replaced by the paragraph run, but only when the run is
    // at least twice as long: a slightly longer run on an index page is the
    // teaser text of *other* articles, and gluing it on would turn one body
    // into several stories.
    if chosen_len < MIN_TEXT_CHARS && run_len >= chosen_len * 2 {
        chosen = paragraph_run;
        chosen_len = run_len;
    }
    // Last resort for pages that wrap their paragraphs in plain div blocks
    // and have no <p> at all: the page's visible text, line filtered like
    // the rest. Only when nothing at all was found - a short article is
    // still the article, and widening it would pull in page furniture.
    if chosen.is_empty() && chosen_len < MIN_TEXT_CHARS / 2 {
        let loose = text_from_plain(&scan.text);
        if loose.chars().count() > chosen_len {
            chosen = loose;
        }
    }
    let chosen = chosen.trim().to_string();
    let chosen = unglue_sentences(&chosen);

    let title = if hint.is_empty() {
        fallback_title(html)
    } else {
        hint
    };

    let mut out = String::new();
    out.push_str("{\"title\":");
    json::escape_str(&mut out, &title);
    out.push_str(",\"text\":");
    json::escape_str(&mut out, &chosen);
    out.push_str(",\"chars\":");
    out.push_str(&chosen.chars().count().to_string());
    out.push('}');
    Ok(out)
}

/// Paragraph text of a plain blob (JSON-LD body): split, normalise, filter.
pub fn text_from_plain(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for piece in raw.split(|c| c == '\n' || c == '\r') {
        let line = text::normalize(piece);
        if line.is_empty() {
            continue;
        }
        if text::keep_line(&line) {
            lines.push(line);
        }
    }
    lines.join("\n")
}

/// Longest run of kept `<p>` paragraphs (runs split on big page gaps).
fn best_paragraph_run(paragraphs: &[RangeText]) -> String {
    let mut best_text = String::new();
    let mut best_len = 0usize;
    let mut cur_text = String::new();
    let mut cur_len = 0usize;
    let mut prev_end: Option<usize> = None;

    for para in paragraphs {
        let line = para.text.trim();
        if line.is_empty() {
            continue;
        }
        let keep = text::keep_line(line);
        let gap = match prev_end {
            Some(pe) => para.start.saturating_sub(pe) > RUN_GAP,
            None => false,
        };
        if !keep || gap {
            if cur_len > best_len {
                best_len = cur_len;
                best_text = std::mem::take(&mut cur_text);
            } else {
                cur_text.clear();
            }
            cur_len = 0;
            if !keep {
                prev_end = Some(para.end);
                continue;
            }
        }
        if !cur_text.is_empty() {
            cur_text.push('\n');
            cur_len += 1;
        }
        cur_text.push_str(line);
        cur_len += line.chars().count();
        prev_end = Some(para.end);
    }
    if cur_len > best_len {
        best_text = cur_text;
    }
    best_text
}

/// Text of the largest `<article>` element.
fn best_article(articles: &[RangeText], paragraphs: &[RangeText]) -> Option<String> {
    let mut best: Option<String> = None;
    let mut best_len = 0usize;
    for article in articles {
        let mut body = paragraphs_in_range(paragraphs, article.start, article.end);
        if body.is_empty() {
            body = text_from_plain(&article.text);
        }
        let len = body.chars().count();
        if len == 0 {
            continue;
        }
        if best.is_none() || len > best_len {
            best_len = len;
            best = Some(body);
        }
    }
    best
}

fn paragraphs_in_range(paragraphs: &[RangeText], start: usize, end: usize) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for para in paragraphs {
        if para.start >= start && para.end <= end {
            let line = para.text.trim();
            if !line.is_empty() && text::keep_line(line) {
                lines.push(line);
            }
        }
    }
    lines.join("\n")
}

/// Break sentences the page glued together with a tag instead of a space.
///
/// `<p>… in Washington.</p><p>District Judge …` loses its paragraph break
/// somewhere between the scan and the text, and the result reads as one
/// endless sentence: sentence counting then rejects real articles as "no
/// prose", and the model sees two stories as one. A terminator that runs
/// straight into a capital letter is a boundary the page forgot to write.
pub fn unglue_sentences(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + text.len() / 24);
    for (i, &ch) in chars.iter().enumerate() {
        if i >= 2
            && i + 1 < chars.len()
            && matches!(chars[i - 1], '.' | '!' | '?')
            && ch.is_uppercase()
            && (chars[i - 2].is_lowercase() || matches!(chars[i - 2], '.' | '!' | '?' | '"' | '\'' | '”' | '’'))
        {
            // `3.5` and `U.S.` stay whole: a digit or an all-caps run before
            // the terminator is not the end of a sentence.
            let before = chars.get(i - 3).copied();
            let digit_run = chars[i - 2].is_ascii_digit()
                || before.map(|b| b.is_ascii_digit()).unwrap_or(false);
            let caps_run = before.map(|b| b.is_uppercase()).unwrap_or(false)
                && chars[i - 2].is_uppercase();
            if !digit_run && !caps_run {
                out.push('\n');
                out.push('\n');
            }
        }
        out.push(ch);
    }
    out
}

/// Longest JSON-LD `articleBody` in any ld+json block on the page.
fn best_jsonld(html: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut best_len = 0usize;
    for block in htmlscan::jsonld_article_bodies(html) {
        let parsed = match json::parse(&block) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let mut bodies: Vec<String> = Vec::new();
        collect_article_body(&parsed, &mut bodies, 0);
        for raw in bodies {
            let cleaned = text_from_plain(&raw);
            let len = cleaned.chars().count();
            if len == 0 {
                continue;
            }
            if best.is_none() || len > best_len {
                best_len = len;
                best = Some(cleaned);
            }
        }
    }
    best
}

fn collect_article_body(value: &Json, out: &mut Vec<String>, depth: usize) {
    if depth > 64 {
        return;
    }
    match value {
        Json::Obj(items) => {
            for (key, val) in items {
                if key.eq_ignore_ascii_case("articleBody") {
                    match val {
                        Json::Str(s) => out.push(s.clone()),
                        other => collect_article_body(other, out, depth + 1),
                    }
                } else {
                    collect_article_body(val, out, depth + 1);
                }
            }
        }
        Json::Arr(items) => {
            for item in items {
                collect_article_body(item, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn fallback_title(html: &str) -> String {
    for tag in ["h1", "title"] {
        if let Some(raw) = first_element_text(html, tag) {
            let cleaned = htmlscan::plain(&raw);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    String::new()
}

fn first_element_text(html: &str, tag: &str) -> Option<String> {
    let mut sc = htmlscan::Scanner::new(html);
    let mut inner_start: Option<usize> = None;
    while let Some(token) = sc.next_token() {
        match token {
            htmlscan::Token::Start {
                name,
                self_closing,
                content_start,
                ..
            } => {
                if inner_start.is_none() && !self_closing && name.eq_ignore_ascii_case(tag) {
                    inner_start = Some(content_start);
                }
            }
            htmlscan::Token::End { name, lt } => {
                if let Some(start) = inner_start {
                    if name.eq_ignore_ascii_case(tag) && lt >= start {
                        return html.get(start..lt).map(|s| s.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    #[test]
    fn extract_prefers_jsonld_article_body() {
        let body = "Long article body sentence that is definitely longer than twenty five characters. ".repeat(8);
        let body = body.trim().to_string();
        let html = format!(
            "<html><head><script type=\"application/ld+json\">\
             {{\"@type\":\"NewsArticle\",\"articleBody\":\"{}\"}}</script></head>\
             <body><article><p>Hi</p></article></body></html>",
            body
        );
        let input = json::Json::Obj(vec![
            ("html".to_string(), json::Json::Str(html)),
            (
                "url".to_string(),
                json::Json::Str("https://example.com/a".to_string()),
            ),
            ("title".to_string(), json::Json::Str("Chips".to_string())),
        ]);
        let encoded = json::to_string(&input);
        let root = json::parse(&encoded).expect("round trip");
        let out = run(&root).expect("extract ok");
        let parsed = json::parse(&out).expect("output is json");
        assert_eq!(
            parsed.get("title").and_then(json::Json::as_str),
            Some("Chips")
        );
        let chosen = parsed
            .get("text")
            .and_then(json::Json::as_str)
            .expect("text");
        assert!(chosen.contains("Long article body"));
        assert!(chosen.chars().count() > 400);
        let chars = parsed
            .get("chars")
            .and_then(json::Json::as_usize)
            .expect("chars");
        assert_eq!(chars, chosen.chars().count());
    }

    #[test]
    fn extract_falls_back_to_paragraph_run() {
        let first = "The first paragraph is long enough to be kept by the line filter for sure.";
        let second = "The second paragraph is also long enough to be kept by the line filter.";
        let html = format!(
            "<html><body><article><p>{}</p><p>{}</p></article></body></html>",
            first, second
        );
        let root = json::Json::Obj(vec![
            ("html".to_string(), json::Json::Str(html)),
            (
                "url".to_string(),
                json::Json::Str("https://example.com/b".to_string()),
            ),
            ("title".to_string(), json::Json::Str("Two".to_string())),
        ]);
        let out = run(&root).expect("extract ok");
        let parsed = json::parse(&out).expect("output is json");
        let chosen = parsed
            .get("text")
            .and_then(json::Json::as_str)
            .expect("text");
        assert!(chosen.contains(first));
        assert!(chosen.contains(second));
        assert_eq!(chosen.lines().count(), 2);
    }

    fn extract_text(html: &str) -> String {
        let root = json::Json::Obj(vec![("html".to_string(), json::Json::Str(html.to_string()))]);
        let out = run(&root).expect("extract ok");
        let parsed = json::parse(&out).expect("output is json");
        parsed
            .get("text")
            .and_then(json::Json::as_str)
            .expect("text")
            .to_string()
    }

    #[test]
    fn extract_uses_visible_text_when_the_page_has_no_paragraphs() {
        let html = "<html><body><div>Chip demand keeps rising across the whole supply \
            chain and the quarter looks strong for the whole sector, analysts said.</div>\
            </body></html>";
        let text = extract_text(html);
        assert!(text.contains("Chip demand keeps rising"), "got {:?}", text);
    }

    #[test]
    fn extract_does_not_glue_teaser_articles_together() {
        let first = "The first teaser line for another story.";
        let second = "The second story body is long enough to be kept by the line filter.";
        let html = format!(
            "<html><body><article><p>{}</p></article><article><p>{}</p></article></body></html>",
            first, second
        );
        let text = extract_text(&html);
        assert!(text.contains(second), "got {:?}", text);
        assert!(!text.contains(first), "got {:?}", text);
    }

    #[test]
    fn glued_sentences_get_their_break_back() {
        let glued = "A judge in Washington.District Judge Leonie Brinkema said no.";
        let fixed = unglue_sentences(glued);
        assert!(fixed.contains("Washington.\n\nDistrict"), "got {:?}", fixed);
        // a decimal and an initialism are not sentence boundaries
        assert_eq!(unglue_sentences("grew 3.5 percent"), "grew 3.5 percent");
        assert_eq!(unglue_sentences("the U.S.Department said"),
                   "the U.S.Department said");
        // already broken text is left exactly as it is (idempotent)
        let fine = "One sentence.\n\nAnother one.";
        assert_eq!(unglue_sentences(fine), fine);
    }
}
