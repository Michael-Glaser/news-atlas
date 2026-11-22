//! One page over `atlas-net`, behind a seam.
//!
//! Decides nothing about HTTP: [`Http`] calls `atlas_net::Client::fetch`, which
//! gates the URL, gates every redirect hop before following it, caps the bytes
//! and decodes the charset. What is decided here is only the shape this crate
//! reads — status, final URL, content type, body, and the two failure reasons
//! kept apart, because a refusal (never try that URL again) is not a transport
//! error (try again, later).
//!
//! The [`Fetcher`] seam exists so the parser, the ranking and the whole tool can
//! be tested against pasted result pages without a socket.

use atlas_net::Client;

/// One search page, as this crate reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    /// The status of the response that was read; 0 when nothing came back.
    pub status: u16,
    /// Where the request ended (every hop already gated).
    pub final_url: String,
    pub content_type: String,
    /// The decoded body.
    pub body: String,
    /// The gate refused the URL: a security decision, no request was made.
    pub refused: String,
    /// The transport failed: timeout, DNS, TLS, protocol, body over cap.
    pub error: String,
    pub truncated: bool,
}

impl Answer {
    /// A 2xx body that came through the gate — the only thing worth parsing.
    pub fn usable(&self) -> bool {
        self.refused.is_empty()
            && self.error.is_empty()
            && (200..300).contains(&self.status)
            && !self.body.trim().is_empty()
    }
}

/// The transport. `Send + Sync` so the server can hold one in an `Arc`.
pub trait Fetcher: Send + Sync {
    /// Read one URL, at most `max_bytes` of it.
    fn fetch(&self, url: &str, max_bytes: usize) -> Answer;
}

/// The real transport: `atlas-net`'s client, one per process.
pub struct Http {
    client: Client,
}

impl Default for Http {
    fn default() -> Http {
        Http::new()
    }
}

impl Http {
    /// The workspace's client: 20 s overall, 15 s connect, `identity` bodies, a
    /// desktop User-Agent, no redirect followed by the transport.
    pub fn new() -> Http {
        Http {
            client: Client::new(),
        }
    }

    /// The User-Agent this process sends, for `/api/status`.
    pub fn user_agent(&self) -> &str {
        self.client.user_agent()
    }
}

impl Fetcher for Http {
    fn fetch(&self, url: &str, max_bytes: usize) -> Answer {
        let page = self.client.fetch(url, max_bytes);
        Answer {
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
