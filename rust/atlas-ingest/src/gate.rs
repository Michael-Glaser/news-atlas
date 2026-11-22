//! The URL gate and the "that host is not an article" rule.
//!
//! Port of `server/ingest.py`'s `_clean_url`, `_url_error`, `_is_blocked_host`
//! and `_host_of`, with the decision itself left where it already lives:
//!
//! * [`atlas_net::gate`] — `collector.url_gate(url)` plus the service-port
//!   policy. It calls `atlas_guard::urlsafe::check`, so the loopback/metadata/
//!   punycode refusals are the guard's, not this crate's.
//! * [`atlas_guard::cleantext::clean`] — the control/bidi strip that
//!   `collector.clean_untrusted` does to a value before it is looked at.
//!
//! Nothing in this module opens a socket: it is the pure decision, which is why
//! `read_article` and `save_article` can both ask it and a test can call it
//! offline.
//!
//! There is **no host list**. ATLAS does not decide what a page is from the name
//! of the site it sits on: whether a page is reporting is the local model's
//! verdict (`atlas-ai`'s news prompt), and it is the only thing that can refuse
//! an article for being the wrong kind of text. What is left here is structural —
//! a URL must point at a document rather than at a search-results page or a site
//! root, which is a property of the URL, not of who publishes it.
//!
//! The shape rule is deliberately one-sided: it refuses what *cannot* be an
//! article (a front page, a results page, a PDF, an image) and lets everything
//! else through to the read path, where the page's own `Content-Type` and the
//! model's verdict decide. A file extension is a structural fact about the URL;
//! a host name is an editorial opinion, and no host name is looked at here.

/// The longest URL the gate will look at (`collector.MAX_URL_CHARS`).
pub const MAX_URL_CHARS: usize = 2048;

/// The host of a URL, lower-cased, without a userinfo or port part.
///
/// `ingest._host_of`, without `urllib.parse`: what is needed here is the
/// authority a reader would name, and nothing else.
pub fn host_of(url: &str) -> String {
    let raw = url.trim();
    let after_scheme = match raw.find("://") {
        Some(index) => &raw[index + 3..],
        None => raw,
    };
    let end = after_scheme
        .find(|ch| ch == '/' || ch == '?' || ch == '#')
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = match host.rfind(':') {
        // an IPv6 literal keeps its colons; a port is dropped
        Some(index) if !host[index..].contains(']') => &host[..index],
        _ => host,
    };
    host.trim_end_matches('.').to_lowercase()
}

/// The path of a URL, lower-cased (`""` when it has none).
pub fn path_of(url: &str) -> String {
    let raw = url.trim();
    let after_scheme = match raw.find("://") {
        Some(index) => &raw[index + 3..],
        None => raw,
    };
    let start = match after_scheme.find(|ch| ch == '/' || ch == '?' || ch == '#') {
        Some(index) => index,
        None => return String::new(),
    };
    let rest = &after_scheme[start..];
    let end = rest.find(|ch| ch == '?' || ch == '#').unwrap_or(rest.len());
    rest[..end].to_lowercase()
}

/// The control/bidi-cleaned form of an untrusted value, capped.
///
/// This **cuts**, and that is right for what it is for: a display clip or a list
/// item shown to a model (`handlers.rs`'s `LIST_ITEM_CHARS`). It is the wrong
/// function for a URL that is about to be judged — see [`url_too_long`].
pub fn clean_value(value: &str, cap: usize) -> String {
    let cleaned = atlas_guard::cleantext::clean(value, Some(cap.max(1)));
    cleaned.text.trim().to_string()
}

/// Would the guard never see this URL in full?
///
/// The gate is asked with `clean_value(raw, MAX_URL_CHARS)`, and `clean` *cuts* at
/// the cap. So a URL of 2049 characters arrived at `atlas_guard::urlsafe::check` as
/// 2048 — and the guard's own refusal (`url longer than 2048 characters`,
/// `urlsafe.rs:81`) could never fire from this crate. Worse, the approved value was
/// the cut string: `find_news` stored a 2048-character prefix as the hit's `url`, a
/// link nobody had seen and nobody had judged. A URL is refused now, not shortened:
/// no page has a 2 KiB address, and a caller that genuinely wants a clip has
/// [`clean_value`], which says so in its name.
///
/// Counted in `chars`, the unit the guard counts, so the two agree about what "long"
/// means — and measured on the raw text, before cleaning can shorten it.
pub fn url_too_long(raw: &str) -> bool {
    raw.trim().chars().count() > MAX_URL_CHARS
}

/// The sentence a too-long URL earns, for the callers that answer in prose.
pub fn too_long_message() -> String {
    format!(
        "url refused (longer than {MAX_URL_CHARS} characters) - pass the address of the article itself, not a wrapped or tracked link"
    )
}

/// `ingest._clean_url` — a usable http(s) article URL, or `""` when the gate
/// refuses the value.
///
/// The decision is the gate's, never a local prefix test: this function used to
/// accept anything that started with `http`, on a machine where `http://127.0.0.1`
/// is the app itself.
pub fn gate_url(raw: &str) -> String {
    if url_too_long(raw) {
        return String::new();
    }
    let candidate = clean_value(raw.trim().trim_matches(|ch| ch == '<' || ch == '>' || ch == '"' || ch == '\''), MAX_URL_CHARS);
    if candidate.is_empty() {
        return String::new();
    }
    let verdict = atlas_net::gate(&candidate);
    if verdict.ok {
        verdict.url().to_string()
    } else {
        String::new()
    }
}

/// The same verdict as [`gate_url`], as the token `atlas-net`'s transport takes.
///
/// A caller that holds this can fetch the page without passing a `String`
/// around; the string form above is kept because the tool handlers show a URL to
/// the model, and a display value is not a request.
pub fn gated_url(raw: &str) -> Option<atlas_guard::Gated> {
    if url_too_long(raw) {
        return None;
    }
    let candidate = clean_value(raw.trim(), MAX_URL_CHARS);
    if candidate.is_empty() {
        return None;
    }
    atlas_net::gate(&candidate).gated().cloned()
}

/// `ingest._url_error` — the gate's reason, phrased for the model.
///
/// Empty when the URL is fine, so a caller can use it as "why not".
pub fn url_error(raw: &str) -> String {
    if url_too_long(raw) {
        return too_long_message();
    }
    let candidate = clean_value(raw.trim(), MAX_URL_CHARS);
    if candidate.is_empty() {
        return String::new();
    }
    let verdict = atlas_net::gate(&candidate);
    if verdict.ok {
        return String::new();
    }
    format!(
        "url refused ({}) - pass the full http(s) link of the article itself, on a public site",
        scrub_reason(&verdict.reason)
    )
}

fn scrub_reason(reason: &str) -> String {
    let collapsed = reason.split_whitespace().collect::<Vec<&str>>().join(" ");
    if collapsed.is_empty() {
        "not a public http(s) url".to_string()
    } else {
        collapsed.chars().take(160).collect()
    }
}

/// File types that are never an article *page*: a document, an image, an audio
/// or video file, an archive, a spreadsheet.
///
/// This is a list of file **types**, which is a structural fact about a URL — it
/// classifies no story and names no publisher. It exists so that
/// `https://site.example/report.pdf` costs nothing: without it the transport
/// would download the file and the browser would start on a page that cannot be
/// one. The list is one-sided on purpose — a link whose suffix is not here is
/// *tried*, and `fetcher::read_failure` refuses it with a reason once the
/// `Content-Type` is known. That is why `…/news/2026/11/story.html` (a real
/// article, and a suffix this rule must never refuse) still passes.
const NON_PAGE_EXTENSIONS: &[&str] = &[
    // documents
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "epub", "djvu",
    // images
    "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "tif", "tiff", "svg", "ico", "heic",
    // audio and video
    "mp3", "m4a", "aac", "wav", "flac", "ogg", "oga", "mp4", "m4v", "mov", "avi", "mkv", "webm",
    "mpg", "mpeg", "wmv",
    // archives and binaries
    "zip", "gz", "tgz", "bz2", "xz", "7z", "rar", "tar", "dmg", "iso", "exe", "msi", "apk", "bin",
    // data and feeds a reader could open but not report on
    "csv", "tsv", "json", "xml", "rss", "atom", "txt",
];

/// Query parameters that carry a question: a results page is the one page whose
/// address is the search itself (`ingest._is_blocked_host`'s `?q=` rule, kept
/// structural — it looks at the parameter name, never at the site).
const QUESTION_PARAMS: &[&str] = &["q", "query", "search", "s"];

/// Why this URL is not an article page — `""` when it may be one.
///
/// The rule is structural on purpose: an article lives at a path, a search engine
/// answers on a query, and a homepage is neither. Nothing here looks at the host
/// name, so no publisher is privileged and none is banned — what the page *is*
/// gets decided by content type and by the model, once the text has been read.
///
/// The reason is a sentence, because a refusal reaches the model (and, through
/// it, the reader) as the answer to *why* nothing was stored.
pub fn article_url_refusal(url: &str) -> String {
    let host = host_of(url);
    if host.is_empty() {
        return "that value is not a full http(s) link to a page".to_string();
    }
    let path = path_of(url);
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let Some(last) = segments.last() else {
        return format!(
            "{} is a site front page, not an article - save the link of one story on it",
            host
        );
    };
    if is_non_page_file(last) {
        return format!(
            "{} is a file ({}), not an article page - a page is read, a file cannot be",
            host,
            last.chars().take(40).collect::<String>()
        );
    }
    if asks_a_question(url) {
        return format!(
            "{} is a search-results page, not an article - open one of its results and save that link",
            host
        );
    }
    String::new()
}

/// Does the last path segment name a file this app cannot read as a page?
fn is_non_page_file(last_segment: &str) -> bool {
    let lower = last_segment.to_lowercase();
    match lower.rsplit_once('.') {
        // A suffix, not the whole segment: `.well-known` is a path, not a file.
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            NON_PAGE_EXTENSIONS.contains(&extension)
        }
        _ => false,
    }
}

/// Does this URL carry a search query in its parameters?
fn asks_a_question(url: &str) -> bool {
    let query = url.split_once('?').map(|(_, rest)| rest).unwrap_or("");
    query.split('&').any(|pair| {
        let key = pair.split('=').next().unwrap_or("").to_ascii_lowercase();
        QUESTION_PARAMS.contains(&key.as_str())
    })
}

/// Whether this URL is a *document* and not a results page or a site root.
pub fn is_article_url(url: &str) -> bool {
    article_url_refusal(url).is_empty()
}

/// Deprecated alias kept for the call sites that still say "blocked host".
///
/// There is no blocked host: this is [`article_url_refusal`]'s verdict as a
/// boolean. New code should ask for the reason, so the model can read it.
pub fn is_blocked_host(url: &str) -> bool {
    !is_article_url(url)
}

/// Are these two URLs the same *page* by shape alone?
///
/// This is a **necessary** condition for `atlas-text`'s normalised form to agree,
/// computed without spawning an engine: the normalised key keeps the authority
/// (lower-cased) and the path (trailing slashes trimmed) and only ever drops
/// query parameters, so two URLs whose host and path differ here cannot normalise
/// to the same key. The converse does not hold — this answers "may be the same",
/// never "is the same".
///
/// [`crate::Pipeline::stored_id_for_url`] uses it as a filter: without it, the
/// fallback scan of the recent window asked `atlas-text` to normalise every
/// stored row — one engine process per row, on the path of every *new* article.
/// `www.` is ignored on both sides, so the filter is never tighter than the
/// engine that has the last word.
pub fn same_page(a: &str, b: &str) -> bool {
    shape(a) == shape(b)
}

/// The host-and-path shape [`same_page`] compares.
fn shape(url: &str) -> (String, String) {
    let host = host_of(url);
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let path = path_of(url);
    let path = path.trim_end_matches('/').to_string();
    let path = if path.is_empty() { "/".to_string() } else { path };
    (host, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_refuses_this_machine() {
        assert_eq!(gate_url("http://127.0.0.1:8080/x"), "");
        assert_eq!(gate_url("http://localhost/x"), "");
        assert_eq!(gate_url("file:///etc/passwd"), "");
        assert_eq!(gate_url("javascript:alert(1)"), "");
    }

    #[test]
    fn the_gate_accepts_a_public_article_and_keeps_its_verdict() {
        let url = gate_url("https://example.com/news/story?utm_source=x");
        assert!(url.starts_with("https://example.com/news/story"), "{}", url);
        assert_eq!(url_error("https://example.com/a"), "");
    }

    /// A URL of exactly `chars` characters at a public host.
    fn url_of_length(chars: usize) -> String {
        let base = "https://example.com/";
        format!("{}{}", base, "a".repeat(chars - base.len()))
    }

    #[test]
    fn a_url_past_the_cap_is_refused_rather_than_shortened() {
        // The defect this pins: `clean_value(raw, MAX_URL_CHARS)` *cuts*, so a URL of
        // 2049 characters reached `atlas_guard::urlsafe::check` as 2048 — the guard's
        // own "url longer than 2048 characters" refusal could never fire from this
        // crate, and the approved value was the prefix, which is a link nobody had
        // seen. Measured before the fix: `gate_url` answered the 2048-character cut.
        let at_cap = url_of_length(MAX_URL_CHARS);
        let one_over = format!("{at_cap}a");
        assert_eq!(at_cap.chars().count(), MAX_URL_CHARS);
        assert!(!url_too_long(&at_cap), "the cap itself is looked at");
        assert!(url_too_long(&one_over));

        // Refused, in all three spellings a caller can ask with.
        assert_eq!(gate_url(&one_over), "", "a cut URL was approved");
        assert!(gated_url(&one_over).is_none(), "a cut URL became a request token");
        let reason = url_error(&one_over);
        assert!(
            reason.contains("url refused") && reason.contains("longer than"),
            "the refusal does not name the length: {reason}"
        );

        // And the cap itself still passes, unchanged: this is a refusal of the
        // over-long, not a new ceiling under the old one.
        let approved = gate_url(&at_cap);
        assert_eq!(approved.chars().count(), MAX_URL_CHARS);
        assert_eq!(approved, at_cap);

        // A display clip is a different call and still cuts (`clean_value`).
        assert_eq!(clean_value(&at_cap, 10).chars().count(), 10);
    }

    #[test]
    fn the_length_is_counted_the_way_the_guard_counts_it() {
        // `chars`, not bytes: this URL is 919 characters and 3 619 bytes, so a byte
        // measurement would refuse a URL the guard looks at in full.
        let long_host = format!("https://example.com/{}", "🦀".repeat(900));
        assert!(long_host.len() > MAX_URL_CHARS, "the fixture is not wide enough");
        assert!(
            !url_too_long(&long_host),
            "the cap was measured in bytes: {} characters, {} bytes",
            long_host.chars().count(),
            long_host.len()
        );
    }

    #[test]
    fn a_refused_url_says_why() {
        let reason = url_error("http://169.254.169.254/latest/meta-data");
        assert!(reason.contains("url refused"), "{}", reason);
    }

    #[test]
    fn an_article_url_has_a_document_and_no_question() {
        // documents
        assert!(is_article_url("https://en.wikipedia.org/wiki/Chip"));
        assert!(is_article_url("https://www.reuters.com/business/chips"));
        assert!(is_article_url("https://news.google.com/rss/articles/CBMi"));
        // A page that carries a document *suffix* is still a page. The old rule
        // refused every dotted last segment, which dropped real articles filed as
        // `story.html` — this is that defect, pinned.
        assert!(is_article_url("https://example.com/news/2026/11/story.html"));
        assert!(is_article_url("https://example.com/news/story.php"));
        assert!(is_article_url("https://example.com/post.aspx?id=7"));
        // a dot inside a segment that is not a suffix is not a file
        assert!(is_article_url("https://example.com/.well-known/x"));
        // files, site roots and results pages
        assert!(!is_article_url("https://example.com"));
        assert!(!is_article_url("https://example.com/"));
        assert!(!is_article_url("https://example.com/report.pdf"));
        assert!(!is_article_url("https://example.com/photo.JPG"));
        assert!(!is_article_url("https://example.com/search?q=chips"));
        assert!(!is_article_url("https://example.com/?s=chips"));
        assert!(!is_article_url("https://example.com/a?q=chips"));
        assert!(!is_article_url(""));
    }

    #[test]
    fn every_shape_refusal_names_the_reason_and_the_fix() {
        let root = article_url_refusal("https://example.com");
        assert!(root.contains("front page"), "{}", root);
        assert!(root.contains("one story"), "{}", root);
        let file = article_url_refusal("https://example.com/report.pdf");
        assert!(file.contains("is a file"), "{}", file);
        let question = article_url_refusal("https://example.com/search?q=chips");
        assert!(question.contains("search-results page"), "{}", question);
        assert!(question.contains("open one of its results"), "{}", question);
        assert_eq!(article_url_refusal("https://example.com/news/story"), "");
    }

    #[test]
    fn no_host_is_banned_by_name() {
        // The point of the change: an encyclopaedia URL is a document that the
        // model may refuse as "not news", but the URL itself is not refused here.
        for host in ["wikipedia.org", "facebook.com", "x.com", "youtube.com", "tiktok.com"] {
            let url = format!("https://{}/some/story", host);
            assert!(is_article_url(&url), "{} must not be refused by name", url);
        }
    }

    #[test]
    fn host_of_reads_the_authority_only() {
        assert_eq!(host_of("https://user:pw@News.Example.COM:8443/a/b?x=1"), "news.example.com");
        assert_eq!(host_of("https://[::1]:8080/x"), "[::1]");
        assert_eq!(host_of(""), "");
    }

    #[test]
    fn the_cheap_page_shape_is_a_necessary_filter_never_a_verdict() {
        // same page: a tracking parameter and a trailing slash are noise
        assert!(same_page(
            "https://example.com/a/?utm_source=x",
            "https://example.com/a"
        ));
        assert!(same_page("https://www.example.com/a", "https://example.com/a"));
        assert!(same_page("https://EXAMPLE.com/a", "https://example.com/a"));
        // not the same page, so no engine has to be asked
        assert!(!same_page("https://example.com/a", "https://example.com/b"));
        assert!(!same_page("https://example.com/a", "https://other.example/a"));
        assert!(!same_page("https://example.com/a", "https://example.com/a/b"));
    }
}
