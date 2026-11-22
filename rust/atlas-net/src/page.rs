//! The result of one fetch — the shape the bridge and the collector read.
//!
//! The only decision here is which of the three outcomes a caller gets: a
//! [`Page::refused`] verdict (a security decision: the URL must never be fetched
//! again), a [`Page::failed`] transport error (try again, or fall back to the
//! rendered browser), or a page with a status, a final URL and decoded text.
//!
//! It does not: gate, fetch, decode or log. It is data, and it carries no page
//! text in `refused`/`error` — those fields hold reasons, never content.

use serde::Serialize;

/// One page as the bridge and the collector see it.
///
/// `refused` and `error` are separate on purpose: a refusal is a security decision
/// (the URL must never be fetched again), an error is a transport failure (try
/// again, or fall back to the rendered browser). Neither field ever carries page
/// text, and `text` is empty unless the final status was 2xx with a document type.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Page {
    /// Status of the response that was read; 0 when nothing came back.
    pub status: u16,
    /// The URL the request ended on (already gated).
    pub final_url: String,
    /// The `Content-Type` header, trimmed, as sent.
    pub content_type: String,
    /// Detected charset (header, BOM or `<meta charset>`), lower-cased.
    pub charset: String,
    /// Bytes kept, after the cap.
    pub bytes: usize,
    /// The decoded body (empty unless the response was a 2xx document).
    pub text: String,
    /// The body hit the cap; the page is cut off.
    pub truncated: bool,
    /// Redirect hops that were followed (each one gated).
    pub redirects: usize,
    /// Why the gate refused — no request was made for a refused hop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// Why the transport failed (timeout, DNS, TLS, protocol, body over cap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Page {
    pub(crate) fn refused(reason: impl Into<String>) -> Page {
        Page {
            refused: Some(reason.into()),
            ..Page::default()
        }
    }

    pub(crate) fn failed(reason: impl Into<String>, redirects: usize) -> Page {
        Page {
            error: Some(reason.into()),
            redirects,
            ..Page::default()
        }
    }
}
