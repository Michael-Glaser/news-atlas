//! `sanitize` — one HTML page to plain paragraphs, before any model sees it.
//!
//! Decides which markup is text at all: script, style, template, svg, iframe and
//! friends are dropped whole, comments and tags are dropped, block elements
//! become paragraph breaks, attributes never reach the output, entities are
//! decoded once, every paragraph goes through `cleantext`, what is left of a tag
//! opener is removed by [`inert`], and the result is capped at `MAX_OUT_CHARS`.
//!
//! # The guarantee
//!
//! **The answer is inert text: after one HTML decode — the number of decodes a
//! parser performs — no character sequence in it can begin a tag, an end tag, a
//! comment or a markup declaration. If some future caller renders this string as
//! markup, the page gains no element, no attribute, no comment and no script; the
//! worst outcome is that characters appear as themselves.**
//!
//! That is a property of *this* output, not a promise about the caller, which is
//! why it is enforced here, at the one point where a paragraph is finished, and
//! re-checked by tests that parse the output the way a renderer would, with a
//! tokenizer written in the test itself so the oracle is not the code under test
//! (`sanitize_starts_no_tag_so_rendering_it_cannot_execute`, and the same property
//! over ten thousand mutated pages).
//!
//! Two routes used to break it, both measured against a browser before [`inert`]
//! existed:
//!
//! * **entities**: `<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>` came out as
//!   `<script>alert(1)</script>`, and so did `&#60script&#62…` with no `;`. The
//!   tags were parsed away and then rebuilt from their escapes *after* the scan.
//! * **removed characters**: `<p>a <\u{0}script>b</p>` came out as `a <script>b`,
//!   because a C0 control between `<` and the name is not part of a tag (so the
//!   `<` was kept as text) and was then dropped by `cleantext`, gluing the two
//!   halves into a tag nobody had scanned. A zero-width space does the same.
//!
//! [`inert`] therefore runs *after* decoding and after character removal, where
//! the text is final, and drops the `<` of anything that could open markup. It
//! does not escape it to `&lt;`: an escape is decodable, so it would leave the
//! attacker a second chance in whichever caller decodes next. What cannot be
//! decoded cannot be rebuilt.
//!
//! What survives is prose: `<` in `5 < 6` is not a tag opener to any HTML parser,
//! so it stays and the sentence keeps its meaning.
//!
//! It does not decide whether a page is an article — choosing the body is
//! `atlas-core`'s `extract`. It fetches nothing, and it keeps no attribute
//! value, not even an `alt` or a link, because an attribute is where a hostile
//! page puts its instructions.

use crate::cleantext;
use crate::json::{self, jobj, jstr, Json};

/// Cap on the text `sanitize_html` returns, so a hostile page cannot push
/// megabytes of prose into a model prompt.
const MAX_OUT_CHARS: usize = 200_000;

/// Elements whose content is never text: dropped whole, never summarised.
const SKIP: &[&str] = &[
    "script", "style", "template", "svg", "head", "iframe", "object", "embed", "noscript",
    "noframes", "canvas", "audio", "video", "form", "select", "textarea",
];

/// Elements that start a new paragraph, so the model sees blocks.
const BREAK: &[&str] = &[
    "p", "div", "br", "li", "ul", "ol", "tr", "td", "th", "table", "section", "article",
    "header", "footer", "nav", "aside", "figure", "figcaption", "blockquote", "pre", "h1",
    "h2", "h3", "h4", "h5", "h6", "main", "dl", "dt", "dd", "hr", "caption", "details",
    "summary",
];

enum Kind {
    Open,
    Close,
    Comment,
    Decl,
}

struct Tag {
    name: String,
    kind: Kind,
    end: usize,
    self_closing: bool,
}

fn is_skip(name: &str) -> bool {
    SKIP.contains(&name)
}

fn is_break(name: &str) -> bool {
    BREAK.contains(&name)
}

/// Read one tag starting at `start` (which must hold `<`). `None` means
/// "this is not a tag", so the caller keeps the `<` as text.
fn read_tag(s: &str, start: usize) -> Option<Tag> {
    let b = s.as_bytes();
    if start >= b.len() || b[start] != b'<' {
        return None;
    }
    let rest = s.get(start..)?;
    if rest.starts_with("<!--") {
        let end = match s.get(start + 4..).and_then(|tail| tail.find("-->")) {
            Some(rel) => start + 4 + rel + 3,
            None => s.len(),
        };
        return Some(Tag {
            name: String::new(),
            kind: Kind::Comment,
            end,
            self_closing: false,
        });
    }
    if rest.starts_with("<!") || rest.starts_with("<?") {
        let end = match s.get(start + 2..).and_then(|tail| tail.find('>')) {
            Some(rel) => start + 2 + rel + 1,
            None => s.len(),
        };
        return Some(Tag {
            name: String::new(),
            kind: Kind::Decl,
            end,
            self_closing: false,
        });
    }
    if start + 1 >= b.len() {
        return None;
    }
    let close = b[start + 1] == b'/';
    let mut i = start + 1 + if close { 1 } else { 0 };
    let name_start = i;
    while i < b.len()
        && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b':' || b[i] == b'_')
    {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = match s.get(name_start..i) {
        Some(raw) => raw.to_ascii_lowercase(),
        None => return None,
    };
    // Walk attributes to the closing '>', honouring quoted values so a
    // `title="a>b"` cannot end the tag early.
    let mut quote: Option<u8> = None;
    let mut end = b.len();
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == b'"' || c == b'\'' {
                    quote = Some(c);
                } else if c == b'>' {
                    end = i + 1;
                    break;
                }
            }
        }
        i += 1;
    }
    let mut self_closing = false;
    if end > name_start + 1 {
        let mut k = end - 1;
        while k > name_start {
            k -= 1;
            match b[k] {
                b' ' | b'\t' | b'\r' | b'\n' => continue,
                other => {
                    self_closing = other == b'/';
                    break;
                }
            }
        }
    }
    Some(Tag {
        name,
        kind: if close { Kind::Close } else { Kind::Open },
        end,
        self_closing,
    })
}

/// Does an HTML parser begin a *token* at a `<` whose kept character after it is
/// `next1`?
///
/// The tokenizer's own rule, and no wider than it. Four shapes make it act:
///
/// * an ASCII letter — a start tag (`<b>`,
///   `<img src=x onerror=…>`);
/// * `/` — an end tag or a bogus comment, in both cases a token;
/// * `!` — a markup declaration (`<!--`, `<!DOCTYPE`, `<![CDATA[`);
/// * `?` — a bogus comment, which is what a browser makes of `<?php`.
///
/// Everything else is character data to a browser: `< b`, `5<6`, `a<-b`,
/// `<:x>`, `a<3` (the broken heart, when no entity is involved), `<` at the end of
/// a page. Those stay, because prose in this app is number and punctuation and
/// deleting its punctuation would be a product change made in the name of safety.
///
/// Be careful comparing this with [`read_tag`]: that also accepts digits, `-`, `:`
/// and `_` as **name** characters, so it will drop `<a:b>` and `<3` as chips off a
/// tag, and its answer to "is this a tag" is deliberately wider than a browser's.
/// The two questions differ — "is this a tag I should drop" versus "would a parser
/// make a token of this" — and one function answering both would have to be wrong
/// about one of them.
fn opens_markup(next1: Option<u8>) -> bool {
    match next1 {
        None => false,
        Some(c) if c.is_ascii_alphabetic() => true,
        Some(b'/') | Some(b'!') | Some(b'?') => true,
        _ => false,
    }
}

/// Remove every `<` that could begin markup, so the answer is inert text.
///
/// This is the guarantee the module documents, and it runs on the *final*
/// paragraph — after entity decoding and after `cleantext` dropped the characters
/// it drops — because both of those can create a tag out of text that had none:
/// `&lt;script&gt;` decodes into one, and `<` + a NUL + `script` becomes one when
/// the NUL is removed. A pass that ran earlier would inspect a string nobody ever
/// sees.
///
/// Dropping rather than escaping is the decision that matters: `&lt;` is
/// decodable, so an escaped `<` is a tag waiting for the next decoder, and this
/// app has more than one (`atlas-web`'s input cleaner and the search-snippet
/// reader both decode). A character that is not there cannot be decoded.
///
/// **The pass runs right to left**, because whether a `<` opens markup depends on
/// what is *kept* after it, and removing a later character can make an earlier
/// `<` dangerous: in `<<script` the outer `<` looks harmless (it is followed by
/// `<`) until the inner one goes, and then it has become a tag. Walking from the
/// right and remembering the next character that survives answers that in one
/// linear pass with no iteration, and the result is an exact fixed point — running
/// it twice changes nothing, which `inert_is_its_own_fixed_point` asserts.
pub fn inert(text: &str) -> String {
    let b = text.as_bytes();
    if !b.contains(&b'<') {
        return text.to_string();
    }
    let mut next: Option<u8> = None;
    let mut dropped: Vec<usize> = Vec::new();
    for i in (0..b.len()).rev() {
        let c = b[i];
        if c == b'<' && opens_markup(next) {
            dropped.push(i);
            continue;
        }
        next = Some(c);
    }
    if dropped.is_empty() {
        return text.to_string();
    }
    // Pushed right to left, so the smallest index comes out first.
    let mut pending = dropped.pop();
    let mut out = String::with_capacity(text.len() - 1 - dropped.len());
    for (i, ch) in text.char_indices() {
        if pending == Some(i) {
            pending = dropped.pop();
            continue;
        }
        out.push(ch);
    }
    out
}

struct Builder {
    paras: Vec<String>,
    cur: String,
    /// Characters already committed to `paras`, plus the separator between them.
    total: usize,
    /// Characters in `cur`, so the cap also holds inside one paragraph.
    cur_chars: usize,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            paras: Vec::new(),
            cur: String::new(),
            total: 0,
            cur_chars: 0,
        }
    }

    fn text(&mut self, raw: &str) {
        let room = match MAX_OUT_CHARS.checked_sub(self.total + self.cur_chars) {
            Some(room) => room,
            None => return,
        };
        if room == 0 {
            return;
        }
        // A page whose whole body is one unbroken run of text used to arrive in
        // full: the cap was only checked when a paragraph ended, so a hostile
        // page could hand a model `field::MAX_FIELD` characters instead of
        // `MAX_OUT_CHARS`. Cut at the budget instead.
        let piece = if raw.chars().count() > room {
            crate::field::char_prefix(raw, room)
        } else {
            raw
        };
        self.cur.push_str(piece);
        self.cur_chars += piece.chars().count();
    }

    fn brk(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        // Decode once, drop what may never be stored, then make what is left
        // inert. In that order: the last step has to see the final characters or
        // it is inspecting a string nobody will ever look at.
        let line = inert(&cleantext::inline(&self.cur));
        self.cur.clear();
        self.cur_chars = 0;
        if line.is_empty() {
            return;
        }
        self.total += line.chars().count() + 2;
        self.paras.push(line);
    }

    fn too_big(&self) -> bool {
        self.total >= MAX_OUT_CHARS
    }

    fn joined(&self) -> String {
        self.paras.join("\n\n")
    }
}

/// HTML to paragraphs of inert text. Tags, comments and the content of
/// script/style/iframe and friends never reach the result; entities are decoded
/// once; every paragraph is cleaned with `clean_text`'s rules; and any `<` left
/// that could begin markup is removed, so the answer is safe as text and would
/// stay safe if a caller rendered it (see the guarantee at the top of this file).
pub fn to_text(html: &str) -> String {
    let src = crate::field::char_prefix(html, crate::field::MAX_FIELD);
    let b = src.as_bytes();
    let mut builder = Builder::new();
    let mut skip: Option<String> = None;
    let mut i = 0usize;
    let mut text_start = 0usize;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if text_start < i && skip.is_none() {
            if let Some(chunk) = src.get(text_start..i) {
                builder.text(chunk);
            }
        }
        match read_tag(src, i) {
            Some(tag) => {
                match tag.kind {
                    Kind::Open => {
                        if skip.is_none() && is_skip(&tag.name) && !tag.self_closing {
                            skip = Some(tag.name.clone());
                        } else if skip.is_none() && is_break(&tag.name) {
                            builder.brk();
                        }
                    }
                    Kind::Close => {
                        let mut closed_skip = false;
                        if let Some(open) = skip.clone() {
                            if open == tag.name {
                                skip = None;
                                closed_skip = true;
                            }
                        }
                        if !closed_skip && skip.is_none() && is_break(&tag.name) {
                            builder.brk();
                        }
                    }
                    Kind::Comment | Kind::Decl => {}
                }
                i = tag.end;
            }
            None => {
                if skip.is_none() {
                    builder.text("<");
                }
                i += 1;
            }
        }
        text_start = i;
        if builder.too_big() {
            break;
        }
    }
    if skip.is_none() && text_start < b.len() {
        if let Some(chunk) = src.get(text_start..) {
            builder.text(chunk);
        }
    }
    builder.brk();
    builder.joined()
}

pub fn run(root: &Json) -> Result<String, String> {
    let html = crate::field::required(root, "html")?;
    Ok(json::to_string(&jobj(vec![("text", jstr(&to_text(html)))])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::json_of;

    /// Every place in `text` where an HTML parser would begin a token: the
    /// witness a renderer needs.
    ///
    /// This is written from the tokenizer's rules and deliberately **does not**
    /// call [`opens_markup`], so the test cannot agree with the code by
    /// construction — the oracle is independent of the implementation it judges.
    /// A `<` followed by a letter starts a tag, `/` starts an end tag or a bogus
    /// comment, `!` a declaration, `?` a bogus comment; anything else is
    /// character data.
    fn tokens_in(text: &str) -> Vec<String> {
        let b = text.as_bytes();
        let mut found = Vec::new();
        for i in 0..b.len() {
            if b[i] != b'<' {
                continue;
            }
            let what = match b.get(i + 1) {
                Some(&c) if c.is_ascii_alphabetic() => "start tag",
                Some(&b'/') => "end tag or bogus comment",
                Some(&b'!') => "markup declaration",
                Some(&b'?') => "bogus comment",
                _ => continue,
            };
            let rest: String = text
                .get(i..(i + 12).min(b.len()))
                .unwrap_or("<cut>")
                .to_string();
            found.push(format!("{} at {} ({:?})", what, i, rest));
        }
        found
    }

    /// The pages a hostile site would serve at a sanitiser, each with the trick it
    /// uses. They are all inert at the end: this list is the attack list, and the
    /// tests below assert over all of it.
    const ATTACKS: &[(&str, &str)] = &[
        (
            "entity-encoded script",
            "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>",
        ),
        (
            "numeric entity, no terminator",
            "<p>&#60script&#62alert(1)&#60/script&#62</p>",
        ),
        (
            "hex entity, no terminator",
            "<p>&#x3Cscript&#X3Ealert(1)&#x3C/script&#X3E</p>",
        ),
        (
            "entity-encoded image with a handler",
            "<p>&lt;img src=x onerror=alert(1)&gt;</p>",
        ),
        (
            "double-encoded (the second decode is a caller's bug)",
            "<p>&amp;lt;script&amp;gt;alert(1)&amp;lt;/script&amp;gt;</p>",
        ),
        ("NUL between < and the name", "<p>a <\u{0}script>b</p>"),
        ("zero-width space between < and the name", "<p>a <\u{200b}script>b</p>"),
        ("bidi override between < and the name", "<p>a <\u{202e}script>b</p>"),
        ("variation selector between < and the name", "<p>a <\u{fe0f}script>b</p>"),
        ("tag characters between < and the name", "<p>a <\u{e0001}script>b</p>"),
        ("svg wrapping its own style", "<svg><style>p{}</style>t</svg>x"),
        ("noscript holding markup", "<noscript><img src=x onerror=alert(1)></noscript>x"),
        ("template holding markup", "<template><img src=x onerror=alert(1)></template>x"),
        ("unquoted attribute injection", "<img src=x onerror=alert(1)>x"),
        (
            "srcset, form action, base href and a meta refresh at once",
            "<img srcset=\"x 1x, javascript:alert(1) 2x\">\
             <form action=\"javascript:alert(1)\"><input></form>\
             <base href=\"http://evil/\">\
             <meta http-equiv=refresh content=\"0;url=http://evil\">x",
        ),
        ("style attribute with a javascript: url", "<p style=\"background:url(javascript:alert(1))\">x</p>"),
        ("data: URI in a link", "<a href=\"data:text/html;base64,PHNjcmlwdD4=\">x</a>"),
        ("comment opener inside a comment", "<!--><script>alert(1)</script>-->x"),
        ("CDATA section", "<![CDATA[<script>alert(1)</script>]]>x"),
        ("mis-nested same-name skip", "<svg><svg></svg>LEAK</svg>"),
        ("unclosed skip element", "<p>before</p><svg><p>after</p>"),
        ("bogus end tags left as text", "<p>a</3>b</ p>c</>d</ x>e</p>"),
        ("an unterminated tag at the end", "<p>text</scr"),
        ("an unterminated tag only", "</scr"),
        ("entity that opens a tag then a comment", "&lt;!--x--&gt;"),
        ("a tag that only becomes one when an earlier < goes", "<<script>alert(1)</script>"),
        ("an end tag rebuilt out of a kept </ pair", "</<script>alert(1)"),
        ("a wall of < with a name behind it", "&&&lt;<&lt;&lt;script&gt;"),
    ];

    #[test]
    fn sanitize_starts_no_tag_so_rendering_it_cannot_execute() {
        // The property, not an example: for every attack, the sanitiser's own
        // scanner must find nothing in what the sanitiser returned. A renderer
        // reading this string as markup has no tag to read, so no attribute, no
        // handler and no script. This fails on the code before `inert` existed —
        // the first attack alone came back as `<script>alert(1)</script>`.
        for (name, html) in ATTACKS {
            let out = to_text(html);
            let found = tokens_in(&out);
            assert!(
                found.is_empty(),
                "{}: rendering the output as HTML would find {:?} in {:?}",
                name,
                found,
                out
            );
        }
    }

    #[test]
    fn sanitize_starts_no_tag_over_ten_thousand_mutated_pages() {
        // The same property against inputs nobody wrote by hand: the corpus is
        // the attack list plus the guard's own hostile fragments, mutated by the
        // crate's deterministic generator, so this is re-runnable to the byte.
        let mut corpus: Vec<&str> = ATTACKS.iter().map(|(_, html)| *html).collect();
        corpus.push("<p>&lt;script&gt;</p>");
        corpus.push("<p>&#60img src=x onerror=alert(1)&#62</p>");
        let checked = std::cell::Cell::new(0usize);
        let stats = crate::mutate::run(&corpus, 10_000, 0x5A17_1E57, |page| {
            let out = to_text(page);
            let found = tokens_in(&out);
            assert!(
                found.is_empty(),
                "mutated page {:?} came back as {:?} and a renderer finds {:?}",
                page,
                out,
                found
            );
            checked.set(checked.get() + 1);
        });
        assert_eq!(stats.runs, 10_000);
        assert_eq!(checked.get(), 10_000);
    }

    #[test]
    fn sanitize_drops_a_tag_that_an_entity_was_hiding() {
        // Measured before the fix, through the binary:
        // `{"html":"<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>"}` answered
        // `<script>alert(1)</script>` — a tag built after the scan that removed
        // tags. The `<` goes; the words of the attempted tag stay as words,
        // because deleting them would be deleting somebody's text.
        assert_eq!(
            to_text("<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>"),
            "script>alert(1)/script>"
        );
        assert_eq!(
            to_text("<p>&#60img src=x onerror=alert(1)&#62</p>"),
            "img src=x onerror=alert(1)>"
        );
        assert_eq!(to_text("<p>&lt;!--x--&gt;</p>"), "!--x-->");
        // The numeric spelling a browser decodes without a `;` decodes here too,
        // so it cannot survive as an escape the sanitiser mistook for text.
        assert_eq!(to_text("<p>&#60/script&#62</p>"), "/script>");
    }

    #[test]
    fn sanitize_drops_a_tag_that_a_removed_character_rebuilt() {
        // A C0 control or an invisible character between `<` and the name is not
        // part of a tag, so the `<` was kept as text; `cleantext` then dropped the
        // character and the two halves became a tag nobody had scanned. Measured
        // before the fix: `<p>a <\u{0}script>b</p>` answered `a <script>b`.
        for hidden in ['\u{0}', '\u{200b}', '\u{202e}', '\u{fe0f}', '\u{e0001}', '\u{ad}'] {
            let html = format!("<p>a <{}script>b</p>", hidden);
            let out = to_text(&html);
            assert_eq!(out, "a script>b", "hidden character {:?}", hidden);
            assert!(tokens_in(&out).is_empty());
        }
    }

    #[test]
    fn sanitize_keeps_an_angle_bracket_that_no_parser_would_act_on() {
        // Inert is not the same as escaped: `<` that cannot begin markup stays,
        // so prose keeps its meaning. This is the half a blunt "strip every <"
        // would get wrong.
        assert_eq!(to_text("<p>5 &lt; 6 and 7 &gt; 6</p>"), "5 < 6 and 7 > 6");
        assert_eq!(to_text("<p>the &lt;3 was mutual</p>"), "the <3 was mutual");
        assert_eq!(to_text("<p>a &lt;&lt; b</p>"), "a << b");
        // A `\u{3c}/` pair is the one shape a browser turns into a token whatever
        // follows it (an end tag, or a bogus comment), so it goes. The emoticon
        // `</3` already could not survive the tag scan below in its literal form.
        assert_eq!(to_text("<p>close &lt;/3</p>"), "close /3");
        assert_eq!(inert("x < y"), "x < y");
        assert_eq!(inert("x<"), "x<");
        assert_eq!(inert("</ x>"), "/ x>");
    }

    #[test]
    fn inert_is_its_own_fixed_point() {
        // Whatever `inert` returns must survive `inert` again unchanged, or the
        // guarantee would depend on how many times the pipeline runs.
        for (name, html) in ATTACKS {
            let once = to_text(html);
            assert_eq!(inert(&once), once, "{} is not a fixed point", name);
            assert!(tokens_in(&once).is_empty(), "{}", name);
        }
        // The cases where removing one `<` is what makes the next one dangerous.
        assert_eq!(inert("<<script>x"), "script>x");
        assert_eq!(inert("</<script>x"), "/script>x");
        assert_eq!(inert("a << b"), "a << b");
        assert_eq!(inert("<<<"), "<<<");
        assert_eq!(inert("</"), "/");
    }

    #[test]
    fn sanitize_keeps_paragraphs_and_drops_inline_tags() {
        let text = to_text("<p>Hi <b>there</b></p><p>Second</p>");
        assert_eq!(text, "Hi there\n\nSecond");
    }

    #[test]
    fn sanitize_drops_script_style_and_iframe_content() {
        let html = "<style>p{color:red}</style><script>var x=1;</script>\
                    <p>Real</p><iframe src=\"//evil\">fallback</iframe>";
        assert_eq!(to_text(html), "Real");
    }

    #[test]
    fn sanitize_decodes_entities() {
        let text = to_text("<p>AT&amp;T &lt;ok&gt; &#8212; caf&eacute;</p>");
        // This test used to expect `AT&T <ok> — café`. The `<ok>` was the bug the
        // module's guarantee now names: a tag spelled in entities was decoded
        // *after* the tag scan and handed back as markup, so the sanitiser's own
        // output could be rendered as HTML. One decode is still made — `&amp;`,
        // `&#8212;` and `&eacute;` prove it — but a `<` a parser would act on does
        // not survive it, while a bare `>` is harmless and stays.
        assert_eq!(text, "AT&T ok> \u{2014} café");
        assert!(tokens_in(&text).is_empty());
    }

    #[test]
    fn sanitize_ignores_comments_and_angle_brackets_in_attributes() {
        assert_eq!(to_text("<!-- hidden --><p>shown</p>"), "shown");
        assert_eq!(to_text("<p title=\"a>b\">kept</p>"), "kept");
    }

    #[test]
    fn sanitize_keeps_text_around_unclosed_markup() {
        let text = to_text("<p>open <b>bold text");
        assert_eq!(text, "open bold text");
        let stray = to_text("a < b");
        assert_eq!(stray, "a < b");
    }

    #[test]
    fn sanitize_strips_control_and_invisible_characters_too() {
        let text = to_text("<p>a\u{0}b\u{200b}c</p>");
        assert_eq!(text, "abc");
    }

    #[test]
    fn sanitize_caps_the_output_inside_one_paragraph_too() {
        // The cap is on the text a model is asked to read, so it cannot depend on
        // the page having paragraphs: one unbroken run used to pass in full.
        let huge = format!("<p>{}</p>", "word ".repeat(MAX_OUT_CHARS / 4));
        let text = to_text(&huge);
        assert!(
            text.chars().count() <= MAX_OUT_CHARS,
            "{} characters came back for a {}-character cap",
            text.chars().count(),
            MAX_OUT_CHARS
        );
        // Many short paragraphs are capped as before.
        let many = "<p>one</p>".repeat(MAX_OUT_CHARS / 2);
        assert!(to_text(&many).chars().count() <= MAX_OUT_CHARS + 8);
        // A page the cap cannot reach is unchanged.
        assert_eq!(to_text("<p>short</p>"), "short");
    }

    #[test]
    fn sanitize_run_returns_the_text_field() {
        let out = run(&json_of("{\"html\":\"<p>Hi <b>there</b></p>\"}")).expect("ok");
        assert_eq!(out, "{\"text\":\"Hi there\"}");
    }
}
