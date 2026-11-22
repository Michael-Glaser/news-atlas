//! Who the request says it is.
//!
//! One decision: the `User-Agent` every page request carries. It is a plain
//! desktop browser — no product token, no library version, nothing that names this
//! app — because publishers answered `atlas/1.0` with 403 while they answer a
//! browser with the page. A header that says "a person's browser" and then behaves
//! politely towards the site is neither a lie to the user nor a fingerprint of the
//! app.
//!
//! Two values are composed rather than frozen: the platform token is
//! `atlas-platform`'s answer about the machine this is running on (the same answer
//! a browser would give), and the Chrome major comes from `ATLAS_CHROME_MAJOR` when
//! the launcher knows the version of the local browser, else from
//! [`DEFAULT_CHROME`].
//!
//! It does not: send anything, keep state, or decide whether a page may be fetched.

/// `collector._DEFAULT_CHROME`, used only when no honest version is offered.
const DEFAULT_CHROME: &str = "140.0.0.0";

/// The `User-Agent` one request carries.
///
/// Built per call on purpose: a client holds one string (see
/// [`crate::Client::user_agent`]), and a caller that needs it twice asks the client.
pub fn user_agent() -> String {
    // The platform token is `atlas-platform`'s answer, taken from the build, not
    // from a runtime string a fork of the environment could rename.
    let platform = atlas_platform::host::navigator_platform();
    format!(
        "Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{} Safari/537.36",
        platform,
        chrome_version()
    )
}

/// The Chrome version token of [`user_agent`]: `"140.0.0.0"` style or a bare major.
///
/// A value that is not a version (empty, a sentence, a path) is ignored rather than
/// pasted into the header, so a broken environment cannot make every request
/// malformed.
fn chrome_version() -> String {
    std::env::var("ATLAS_CHROME_MAJOR")
        .ok()
        .and_then(|raw| version_token(raw.trim()))
        .unwrap_or_else(|| DEFAULT_CHROME.to_string())
}

/// A version-looking value as a full token, or `None` when it is not a version.
///
/// One rule, in one place, so "is this a version" is a test and not a comment.
fn version_token(value: &str) -> Option<String> {
    let plausible = !value.is_empty()
        && value.len() <= 16
        && value.chars().all(|c| c.is_ascii_digit() || c == '.')
        && value.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
    if !plausible {
        return None;
    }
    Some(if value.contains('.') {
        value.to_string()
    } else {
        format!("{}.0.0.0", value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_is_a_plain_desktop_browser_and_says_nothing_about_atlas() {
        let ua = user_agent();
        assert!(ua.starts_with("Mozilla/5.0 ("), "{}", ua);
        assert!(ua.contains("AppleWebKit/537.36"), "{}", ua);
        assert!(ua.contains("Chrome/"), "{}", ua);
        assert!(ua.contains("Safari/537.36"), "{}", ua);
        let low = ua.to_ascii_lowercase();
        assert!(!low.contains("atlas"), "product token in {}", ua);
        assert!(!low.contains("ureq"), "library token in {}", ua);
        assert!(!low.contains("rust"), "library token in {}", ua);
        // Composed from the real platform, not a frozen constant and not a string
        // read from the environment.
        assert!(
            ua.contains(atlas_platform::host::navigator_platform()),
            "{}",
            ua
        );
        // Reused, not rebuilt per call: the client holds one string.
        let client = crate::Client::new();
        assert_eq!(client.user_agent(), ua);
    }

    #[test]
    fn only_a_version_reaches_the_header() {
        // The environment is not trusted to shape a header: a path, a sentence or a
        // header injection never reaches the wire.
        for junk in ["", " ", "../etc", "a b", "12.0\nX-Injected: 1", "12.0.0.0.0.0.0.0.1"] {
            assert_eq!(version_token(junk), None, "{:?} became a version", junk);
        }
        assert_eq!(version_token("141").unwrap(), "141.0.0.0");
        assert_eq!(version_token("141.0.7390.55").unwrap(), "141.0.7390.55");
        // And the honest default is a real version, not a placeholder word.
        assert!(DEFAULT_CHROME.starts_with(char::is_numeric), "{}", DEFAULT_CHROME);
    }
}
