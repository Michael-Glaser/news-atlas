//! Getting a page: `atlas-net`, and nothing that is not Rust.
//!
//! Port of `collector.fetch_page_http`, `collector.read_full_article` and
//! `ingest._read_failure`'s diagnosis:
//!
//! 1. the URL is gated ([`crate::gate`]) — every hop of a redirect chain is gated
//!    again by `atlas-net`, so a public link cannot land this app on a metadata
//!    address;
//! 2. `atlas-net` fetches it (byte cap, charset, no compression);
//! 3. `atlas-core` extracts the article text ([`crate::rules::Rules`]) — HTML
//!    never becomes text in this crate;
//! 4. if that text is not article text, the read is a *failure with a reason*, not a
//!    second transport.
//!
//! Step 4 used to ask a local Chrome over the DevTools protocol for a JS-rendered
//! body. That was a non-Rust binary driven through its remote-control flag on an
//! ephemeral **listening** port, and this product opens no port in the agent: the seam
//! below is [`NullBrowser`] in every shipped pipeline, so a page that needs JavaScript
//! gets a sentence the model can act on ([`crate::read`]'s JS-only case). The flag is
//! not spelled here on purpose — the reachability audit counts a line that names it,
//! comments included.
//!
//! Both paths are still seams ([`Fetcher`], [`BrowserReader`]) so a test can run the
//! whole `read_article` flow without a socket, and so a caller who genuinely needs a
//! rendering engine can supply one from outside this crate. Neither keeps cookies,
//! writes a profile or downloads anything.
//!
//! Nothing here decides *whether* a page is an article — that is
//! [`crate::quality`] and the model.

use std::sync::Arc;

use serde_json::json;

/// One HTTP answer, already gated and capped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fetched {
    /// The status of the response that was read; 0 when nothing came back.
    pub status: u16,
    /// The URL the request ended on (a redirect chain is followed and gated).
    pub final_url: String,
    pub content_type: String,
    /// The decoded body, empty unless the response was a 2xx document.
    pub body: String,
    /// The gate or the transport refused; the reason is a log line, not content.
    pub refused: String,
    pub error: String,
    pub truncated: bool,
}

impl Fetched {
    /// A response this pipeline can extract text from.
    pub fn usable(&self) -> bool {
        self.refused.is_empty()
            && self.error.is_empty()
            && (200..300).contains(&self.status)
            && !self.body.trim().is_empty()
            && self.content_type.to_lowercase().contains("html")
    }

    pub fn as_json(&self, include_body: bool) -> serde_json::Value {
        let mut value = json!({
            "status": self.status,
            "final_url": self.final_url,
            "content_type": self.content_type,
            "bytes": self.body.len(),
            "truncated": self.truncated,
        });
        if !self.refused.is_empty() {
            value["refused"] = json!(self.refused);
        }
        if !self.error.is_empty() {
            value["error"] = json!(self.error);
        }
        if include_body {
            value["html"] = json!(self.body);
        }
        value
    }
}

/// One page, as the browser read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserPage {
    pub url: String,
    pub title: String,
    pub text: String,
    /// Why the browser could not answer (not installed, timed out, refused).
    pub error: String,
}

/// The HTTP transport.
pub trait Fetcher: Send + Sync {
    fn fetch(&self, url: &str, max_bytes: usize) -> Fetched;
}

/// The local browser.
pub trait BrowserReader: Send + Sync {
    fn read(&self, url: &str, max_chars: usize) -> BrowserPage;
}

/// `atlas-net` — gated redirects, byte caps, charset decoding.
pub struct NetFetcher {
    client: Arc<atlas_net::Client>,
}

impl NetFetcher {
    pub fn new() -> NetFetcher {
        NetFetcher {
            client: Arc::new(atlas_net::Client::new()),
        }
    }

    /// A client with a smaller hard cap (tests, a caller on a thin connection).
    pub fn with_cap(hard_cap: usize) -> NetFetcher {
        NetFetcher {
            client: Arc::new(atlas_net::Client::with_cap(hard_cap)),
        }
    }
}

impl Default for NetFetcher {
    fn default() -> NetFetcher {
        NetFetcher::new()
    }
}

impl Fetcher for NetFetcher {
    fn fetch(&self, url: &str, max_bytes: usize) -> Fetched {
        let page = self.client.fetch(url, max_bytes);
        Fetched {
            status: page.status,
            final_url: page.final_url,
            content_type: page.content_type,
            body: page.text,
            refused: page.refused.unwrap_or_default(),
            error: page.error.unwrap_or_default(),
            truncated: page.truncated,
        }
    }
}

/// No browser at all: the HTTP path is the only transport.
///
/// Used by the tests and by a headless deployment where Chrome is absent. It
/// never pretends to have read a page.
pub struct NullBrowser;

impl BrowserReader for NullBrowser {
    fn read(&self, _url: &str, _max_chars: usize) -> BrowserPage {
        BrowserPage {
            error: "no local browser in this pipeline".to_string(),
            ..BrowserPage::default()
        }
    }
}

/// `ingest._read_failure` — why a page could not be read, in words the model can
/// act on, and a stable `reason` the caller branches on.
///
/// `chars` is how much text the extraction produced; `status` is the HTTP status,
/// 0 when nothing came back.
pub fn read_failure(status: u16, chars: usize, min_chars: usize, browser_error: &str) -> (String, String) {
    if matches!(status, 401 | 402 | 403 | 429 | 451) {
        return (
            "blocked".to_string(),
            format!(
                "the site refused the request (HTTP {}) - pick the same story from another outlet",
                status
            ),
        );
    }
    if matches!(status, 404 | 410) {
        return (
            "gone".to_string(),
            format!(
                "the page is gone (HTTP {}) - the link is dead, search again",
                status
            ),
        );
    }
    if status == 0 && !browser_error.is_empty() {
        return (
            "unreachable".to_string(),
            "the page could not be fetched at all (network, timeout or a hard block) - try another source"
                .to_string(),
        );
    }
    if chars == 0 {
        return (
            "not_article".to_string(),
            "the page did not contain article text (homepage, redirect, video or a bot check) - try another source"
                .to_string(),
        );
    }
    (
        "too_short".to_string(),
        format!(
            "only {} characters of article text (need {}) - the page was a teaser, a live ticker or a redirect. Try another source.",
            chars, min_chars
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_and_a_dead_link_read_differently() {
        let (reason, message) = read_failure(403, 0, 700, "");
        assert_eq!(reason, "blocked");
        assert!(message.contains("another outlet"));
        let (reason, _) = read_failure(404, 0, 700, "");
        assert_eq!(reason, "gone");
        let (reason, _) = read_failure(0, 0, 700, "no local browser");
        assert_eq!(reason, "unreachable");
        let (reason, message) = read_failure(200, 300, 700, "");
        assert_eq!(reason, "too_short");
        assert!(message.contains("300"));
        let (reason, _) = read_failure(200, 0, 700, "");
        assert_eq!(reason, "not_article");
    }

    #[test]
    fn a_null_browser_never_pretends() {
        let page = NullBrowser.read("https://example.com/a", 1000);
        assert!(page.text.is_empty());
        assert!(!page.error.is_empty());
    }

    #[test]
    fn only_an_html_2xx_with_a_body_is_usable() {
        let mut page = Fetched {
            status: 200,
            content_type: "text/html; charset=utf-8".to_string(),
            body: "<html>x</html>".to_string(),
            ..Fetched::default()
        };
        assert!(page.usable());
        page.content_type = "application/pdf".to_string();
        assert!(!page.usable());
        page.content_type = "text/html".to_string();
        page.refused = "port 22".to_string();
        assert!(!page.usable());
    }
}
