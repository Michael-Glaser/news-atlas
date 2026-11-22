//! The endpoint: one search engine, as a URL template per surface.
//!
//! Decides which URL a query becomes. Nothing else: it holds no key, no account,
//! no cookie and no host list of "good" publishers. The template is the user's
//! ([`ENV_WEB`] / [`ENV_NEWS`], or the `web` / `news` field of a tool call) and
//! the default is one plain HTML endpoint that needs no API key — so a fetch is
//! a page fetch, gated by `atlas-net` like any other.
//!
//! The **query is the model's**, kept whole: quotes, unicode and the words the
//! model chose are percent-encoded once (RFC 3986) and never rewritten,
//! trimmed to a keyword list or lowercased. Searching for `"chip export rules"`
//! and searching for `chip export rules` are different requests, and this crate
//! does not decide which one the model meant.
//!
//! Two surfaces over the same engine: [`Surface::Web`] for the general index and
//! [`Surface::News`] for the news vertical. A news surface that shows a date is
//! what [`crate::search::Search::news_for`] needs; a web surface usually does
//! not, which is why the operation that filters by date asks for `news`.

/// The default web surface: a plain HTML result page, no key, no account.
///
/// Measured on 2026-11-21 from this machine: `200`, 34.8 kB, ten results in the
/// shape [`crate::fixtures::DUCK_WEB`] models — and after a handful of requests in
/// a row, `202` with a 15 kB challenge page instead. That is why the status is
/// part of the evidence ([`crate::search::Search::web_search`] reports it) and why
/// the template is the user's to change: an engine that decides to refuse this
/// machine is a fact about the engine, not an empty search.
pub const DEFAULT_WEB: &str = "https://html.duckduckgo.com/html/?q={query}";

/// The default news surface: the same engine's news vertical, in HTML.
pub const DEFAULT_NEWS: &str = "https://html.duckduckgo.com/html/?q={query}&ia=news&iar=news";

/// The environment variable that overrides the web surface.
pub const ENV_WEB: &str = "ATLAS_SEARCH_WEB";

/// The environment variable that overrides the news surface.
pub const ENV_NEWS: &str = "ATLAS_SEARCH_NEWS";

/// The placeholder a template must contain.
pub const PLACEHOLDER: &str = "{query}";

/// Which surface of the engine a call asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Web,
    News,
}

impl Surface {
    pub fn name(self) -> &'static str {
        match self {
            Surface::Web => "web",
            Surface::News => "news",
        }
    }
}

/// One configured engine: a web template and a news template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engine {
    web: String,
    news: String,
}

impl Default for Engine {
    fn default() -> Engine {
        Engine {
            web: DEFAULT_WEB.to_string(),
            news: DEFAULT_NEWS.to_string(),
        }
    }
}

impl Engine {
    /// An engine from two templates. Either may be empty:
    ///
    /// * both empty → the defaults;
    /// * `news` empty → the news surface is the web surface (one surface);
    /// * `web` empty → the web surface is the news surface.
    ///
    /// A template without `{query}`, or whose probe URL is refused by the gate,
    /// is an error with a reason — never a silent fallback to a hardcoded host.
    pub fn new(web: &str, news: &str) -> Result<Engine, String> {
        let web = web.trim();
        let news = news.trim();
        let (web, news) = match (web.is_empty(), news.is_empty()) {
            (true, true) => (DEFAULT_WEB.to_string(), DEFAULT_NEWS.to_string()),
            (false, true) => (web.to_string(), web.to_string()),
            (true, false) => (news.to_string(), news.to_string()),
            (false, false) => (web.to_string(), news.to_string()),
        };
        Engine { web, news }.validated()
    }

    /// The environment's engine, falling back to the defaults.
    pub fn from_env() -> Result<Engine, String> {
        let web = std::env::var(ENV_WEB).unwrap_or_default();
        let news = std::env::var(ENV_NEWS).unwrap_or_default();
        Engine::new(&web, &news)
    }

    /// Check both templates: a placeholder, an http(s) URL, a public host.
    fn validated(self) -> Result<Engine, String> {
        for (surface, template) in [(Surface::Web, &self.web), (Surface::News, &self.news)] {
            if !template.contains(PLACEHOLDER) {
                return Err(format!(
                    "the {} search template has no {} in it, so the query would go nowhere",
                    surface.name(),
                    PLACEHOLDER
                ));
            }
            let probe = template.replace(PLACEHOLDER, "query");
            if !atlas_net::gate(&probe).ok {
                return Err(format!(
                    "the {} search template is not a URL this app may open ({})",
                    surface.name(),
                    crate::scrub(&atlas_net::gate(&probe).reason)
                ));
            }
        }
        Ok(self)
    }

    /// The template of one surface, as configured.
    pub fn template(&self, surface: Surface) -> &str {
        match surface {
            Surface::Web => &self.web,
            Surface::News => &self.news,
        }
    }

    /// The URL to ask for one query: the template with the query encoded into
    /// it, exactly once.
    pub fn url(&self, surface: Surface, query: &str) -> String {
        self.template(surface)
            .replace(PLACEHOLDER, &encode_component(query))
    }

    /// The host a surface asks (`""` when the template cannot be read).
    pub fn host(&self, surface: Surface) -> String {
        crate::link::host_of_url(&self.template(surface).replace(PLACEHOLDER, "query"))
    }
}

/// Percent-encode one query value: RFC 3986 unreserved characters kept, every
/// other byte as `%XX`.
///
/// A query is a value, not a path segment, so a space becomes `%20` (a `+` is a
/// form convention, and an engine that reads it as a literal `+` would search
/// for the wrong words).
pub fn encode_component(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        let ch = *byte;
        if ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.' | b'~') {
            out.push(ch as char);
        } else {
            out.push('%');
            out.push(HEX[(ch >> 4) as usize] as char);
            out.push(HEX[(ch & 0x0f) as usize] as char);
        }
    }
    out
}
