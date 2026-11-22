//! The one place a stored address may become an action.
//!
//! A story's `url` is scraped text that came off the open web and was written by a
//! collector. The window may *show* it, but showing is not the dangerous half: egui's
//! hyperlink sends `OpenUrl`, which the platform backend hands to the OS opener
//! (`NSWorkspace` on macOS, `xdg-open` on Linux, `ShellExecute` on Windows) with no
//! filtering at all — `webbrowser` builds the request straight from the string. So a row
//! in a local database file could ask the system to launch `file:///…` or a custom
//! scheme registered by some other program, and the reader would never see where the
//! click was going, because the visible text is a fixed label.
//!
//! Two rules follow, and they live here rather than at the call site so there is exactly
//! one implementation to audit:
//!
//! 1. **Only `http` and `https` are openable**, with a host that cannot be mistaken for a
//!    destination (`user@host` tricks, backslashes, control characters, absurd length).
//! 2. **The destination is shown.** [`label`] puts the host in the text of the link, so a
//!    click that leaves the app says where it goes. A link that hides its target is a
//!    link that lies.
//!
//! A stored address that fails rule 1 is not hidden — [`openable`] returns `None` and the
//! caller shows the address as plain text, so the reader can still copy it deliberately.

/// The longest address this app will act on. Real article URLs are far shorter; the
/// cap exists so a megabyte of "url" cannot become a layout problem or a launcher
/// argument.
///
/// `pub(crate)` because there is a second gate on the same kind of string: the one that
/// decides what the embedded page engine may load, in the sibling module's `allowed`. That
/// function must not be **looser** than this one — an address this crate refuses to offer as
/// a link must not be loadable just because the archive row spelled it out another way — and
/// one number with two readers is how the two stay in step. Do not spell `512` again at the
/// other site: a second literal is a cap that silently drifts.
pub(crate) const MAX_LENGTH: usize = 512;

/// The address, if it is one this window is willing to open.
///
/// Returned borrowed, so the caller cannot accidentally keep a modified copy.
pub fn openable(url: &str) -> Option<&str> {
    let url = url.trim();
    if url.len() > MAX_LENGTH {
        return None;
    }
    // A scheme is only acceptable in lower case: `HTTP://` is not what a browser would
    // treat as the same thing, and normalising it here would be inventing a URL.
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;

    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    // No credentials, no backslashes (Windows treats them as separators), no port
    // ranges that a naive parser and the OS could read differently.
    if host.contains('@') || host.contains('\\') {
        return None;
    }
    // Anything a terminal, a shell, a file system or another URL parser could read
    // differently, plus the characters that let stored text differ from what it looks
    // like. `\u{200B}` and `\u{202E}` are the interesting ones: they are not control
    // characters (`char::is_control` is Cc only), they render as nothing or as a
    // reversal, and they are exactly what a hostile row would use to make two addresses
    // look identical. The rule is therefore "printable ASCII only": a real article URL is
    // already ASCII — an international host arrives as `xn--` and a space arrives as
    // `%20` — so a non-ASCII byte in an address is a reason to show it as text instead of
    // handing it to the system opener.
    if !url.chars().all(|ch| {
        ch.is_ascii_graphic() && !matches!(ch, '"' | '\'' | '`' | '<' | '>' | '|' | '\\')
    }) {
        return None;
    }
    Some(url)
}

/// The host part of an openable address, for the link's visible label.
///
/// The authority minus any **user info**. Everything up to the last `@` in
/// `https://user:pass@host/path` is a credential form, and a label built from the authority as
/// written would paint the password: measured by `tests/reader_privacy.rs`
/// (`a_credentialed_url_paints_the_host_and_not_the_credential`) as
/// `Copy reader:hunter2@news.example.com`. For an *openable* address this strips nothing —
/// [`openable`] already refuses an `@` in the authority — so the rule costs the caller nothing
/// and closes the one path where an ungated caller asks for a label.
///
/// Falls back to the whole address when there is nothing better, so the label never renders as
/// an empty link; an address whose authority is *only* a credential form has nothing left to
/// name, and an empty host is still better than a password.
pub fn host(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // The last `@` is the one a URL parser splits on, so this is the same boundary the engine's
    // loader uses rather than a second opinion about where the host starts.
    let (had_user_info, host) = match authority.rsplit_once('@') {
        Some((_, after)) => (true, after),
        None => (false, authority),
    };
    if host.is_empty() {
        // An authority that was *only* a credential form has nothing left to name; one that
        // never had a host at all (`file:///tmp/x` asks for the whole string) keeps the old
        // fallback, so a label is never empty.
        if had_user_info {
            ""
        } else {
            url
        }
    } else {
        host
    }
}

/// The text a button for `url` should carry: what the click does, and where it goes.
///
/// "Copy <host>" rather than "Copy the source": the sentence-case verb says the action,
/// and the host says the destination, which is the half a stored URL cannot be trusted to
/// describe. The verb is *copy* and not *open* because this window hands nothing to the
/// operating system — the address goes to the clipboard and the reader decides.
pub fn label(url: &str) -> String {
    format!("Copy {}", host(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_article_address_is_openable() {
        for url in [
            "https://www.example.com/politics/story-1",
            "http://127.0.0.1:9/x",
            "https://news.example.org",
            "https://example.com/a?b=c#d",
        ] {
            assert_eq!(openable(url), Some(url), "{url} should be openable");
        }
    }

    #[test]
    fn every_other_scheme_is_refused() {
        for url in [
            "file:///Applications/Calculator.app",
            "file:///Users/someone/.ssh/id_ed25519",
            "javascript:alert(1)",
            "mailto:someone@example.com",
            "smb://host/share",
            "data:text/html,<script>alert(1)</script>",
            "ftp://example.com/x",
            "vbscript:msgbox",
            "about:blank",
            "HTTP://example.com/x",
            "//example.com/x",
            "/etc/passwd",
            "",
        ] {
            assert_eq!(openable(url), None, "{url} must not be openable");
        }
    }

    #[test]
    fn an_address_that_hides_its_destination_is_refused() {
        for url in [
            "https://evil.example@good.example/x",
            "https://good.example\\@evil.example/x",
            "https://exa mple.com/x",
            "https://example.com/\"onclick",
            "https://example.com/<script>",
            "https://example.com/\u{200b}x",
            "https://example.com/\u{202e}x",
            "https://example.com/x\nhttps://elsewhere.example",
        ] {
            assert_eq!(openable(url), None, "{url} must not be openable");
        }
    }

    #[test]
    fn a_megabyte_of_address_is_not_an_address() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_LENGTH));
        assert_eq!(openable(&long), None);
        let long_host = format!("https://{}.com/x", "a".repeat(300));
        assert_eq!(openable(&long_host), None);
    }

    #[test]
    fn the_label_says_where_the_click_goes() {
        assert_eq!(
            label("https://www.example.com/a/b?c"),
            "Copy www.example.com",
            "the label must name the destination"
        );
        assert_eq!(label("http://127.0.0.1:9/x"), "Copy 127.0.0.1:9");
        // An address with no host at all still gets a label; it is shown as text, never
        // as a link, so the label only has to be non-empty.
        assert!(!label("file:///tmp/x").is_empty());
    }

    #[test]
    fn the_host_is_the_part_a_reader_recognises() {
        assert_eq!(host("https://www.example.com/a"), "www.example.com");
        assert_eq!(host("https://example.com"), "example.com");
        assert_eq!(host("https://example.com?q=1"), "example.com");
    }
}
