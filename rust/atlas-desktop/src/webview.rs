//! The reader's page view: the *system* web engine, in this process, as a child view of the
//! window — WKWebView on macOS, WebView2 on Windows, and on Linux no engine at all, where the
//! page is read as text by [`crate::views::reader`]. The engine is the operating system's, so
//! nothing is downloaded to make a page render and nothing listens: a view is a rectangle of
//! this window, not a server. What it may load is [`allowed`] and nothing else — `http`/`https`,
//! with a host and no credentials — and that rule is applied three times: to the address the
//! reader asks for, to every navigation the page itself attempts (a link, a redirect, a
//! `location.href`, a meta refresh all arrive at the navigation handler), and to `window.open`,
//! which is refused outright rather than opened in a second surface nobody governs.

// Only an engine build has anything to put in a `Cell`: the handle below. On Linux this import
// would be an unused one, and this module is compiled there too.
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::cell::Cell;

/// Is a system web engine compiled into this build?
///
/// True where the platform ships one: macOS (WKWebView) and Windows (WebView2). On Linux the
/// `wry` crate is not even compiled — it would need WebKitGTK installed before the app could
/// build — so the answer is `false` and the reader draws the stored text.
pub fn engine() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// May the engine load this address?
///
/// `http` and `https` only, parsed here by hand because the rule is a short one and this crate
/// has no URL library. The refusals, each for its own reason:
///
/// * **any other scheme.** `file:` reads this machine's disk into a page's script;
///   `javascript:` runs in the context of whatever is already loaded; `data:` is a document
///   with no origin and no host to check; `about:` and `chrome:` are the browser's own
///   surfaces; `ftp:` has no reason to be here at all. The scheme is compared without case, so
///   `HtTpS:` is `https:`.
/// * **an address with `user:pass@` or any userinfo.** The credentials would be sent to a host
///   chosen by whoever wrote the stored address, and nothing on screen would say so; the
///   `host@` form without a password is worse, because it makes the reader believe the host is
///   the part after the `@`.
/// * **no host** (`https://`, `http:///x`). There is nothing to connect to, and an empty
///   authority is the shape a hand-built string takes when it is wrong.
/// * **a control character, a space, or surrounding whitespace.** A URL is one line of ASCII;
///   a newline inside it is how a check and a parser are made to disagree about where the
///   string ends. Whitespace is **refused, not trimmed**: `" https://example.com/ "` is not
///   the address, and this function will not guess which of the two the caller meant. (The
///   widget's gate, [`crate::link::openable`], does trim — it is deciding what to put on a
///   button, and a button can show the trimmed spelling. Here the string is about to become a
///   navigation, so a byte that is not part of the address is a byte the engine would have to
///   interpret.)
/// * **a backslash.** `https://example.com\path` is refused. A backslash is not a URL
///   character; browsers read it as `/`, so the engine would see host `example.com` and path
///   `/path` while this parse — which does not split on it — would see one authority. That
///   difference is the whole reason for the rule above, and
///   `https://evil.test\.example.com/` is the same disagreement with a host that lies about
///   itself. [`crate::link::openable`] refuses it too, for the same family of reasons.
/// * **bracketed IPv6.** `http://[::1]/` is a legitimate address, but this parse splits the
///   port on the last colon and would have to grow a bracket rule to keep the literal whole;
///   refusing is honest here, and the engine's own parser is not the authority on what this
///   window may load.
///
/// Loopback is allowed on purpose: the model server is at `127.0.0.1`, and a page the reader
/// opens may well be that server's own status surface.
pub fn allowed(url: &str) -> bool {
    // Length first, before a single byte is parsed, in **bytes** rather than characters: the
    // cap is [`crate::link::MAX_LENGTH`], the same 512 bytes the archive's own link gate
    // refuses above ([`crate::link::openable`]). Two reasons it belongs here and at the top.
    //
    // The two gates see the same string. `allowed` is asked about addresses that came out of
    // `data/atlas.db` — a `url` column some page or model wrote — and so does `openable`; if
    // this one had no cap, an address the window will not offer as a link would still be
    // loadable by the engine, and the tighter gate would be decorative. A gate is only as
    // strong as its loosest reader.
    //
    // And the work below is linear in the input with no bound of its own: the character scan,
    // the scheme split, the authority split. A multi-megabyte "url" is not an address, it is a
    // stored string shaped like one, and it should cost the engine nothing to refuse it. The
    // check is cheap, so it goes first, where it also protects the parse from having to be
    // correct about input no address can have.
    if url.len() > crate::link::MAX_LENGTH {
        return false;
    }
    // One line of printable ASCII, and no backslash. Nothing this window constructs has a space
    // in it, and a string that does is either a mistake or an attempt; a backslash is the
    // character a browser rewrites, so accepting one would hand the engine a different URL from
    // the one checked here.
    if url.is_empty()
        || !url
            .chars()
            .all(|c| c.is_ascii() && !c.is_ascii_control() && c != ' ' && c != '\\')
    {
        return false;
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return false;
    }
    // `//host` is what makes the next part an authority. `http:example.com` is the relative
    // form a browser would resolve against a base, and there is no base in this window.
    let Some(authority_and_path) = rest.strip_prefix("//") else {
        return false;
    };
    let authority = authority_and_path
        .split(|c| c == '/' || c == '?' || c == '#')
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return false;
    }
    if authority.contains('@') {
        return false;
    }
    if authority.contains('[') || authority.contains(']') {
        return false;
    }
    // A port, when there is one, is digits after one colon. `host:` and `host:x` are refused
    // rather than handed to the engine to guess at.
    let host = match authority.rsplit_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            host
        }
        None => authority,
    };
    !host.is_empty()
}

/// Open `url` in a new child view of the window named by the last [`attach`].
///
/// `None` means "no engine for this" — a refused address, a build without an engine (Linux), a
/// window no frame has named yet, or an engine that would not build (no WebView2 runtime on
/// this Windows). All four are one answer to the caller, which draws the text reader instead;
/// none of them is a panic and none leaves a half-built view behind.
///
/// `rect` is where the view sits inside the window, in logical pixels.
pub fn open(url: &str, rect: egui::Rect) -> Option<Page> {
    if !allowed(url) {
        return None;
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let parent = parent()?;
        let built = wry::WebViewBuilder::new()
            .with_url(url)
            .with_bounds(bounds(rect))
            // Off: devtools in a reading window is a console for a page the reader did not
            // write, and it arrives with a browser the product is not.
            .with_devtools(false)
            // Every navigation the page attempts — a link, a redirect, a meta refresh, a
            // `location.href` in a script — passes through here. A page may move within the
            // web it is allowed to see, and nowhere else.
            .with_navigation_handler(|next: String| allowed(&next))
            // `window.open` is refused rather than allowed: a second surface would have no
            // bounds in this window, no reader contract and no navigation rule of its own.
            .with_new_window_req_handler(|_url: String, _features| wry::NewWindowResponse::Deny)
            // A download is refused, and the caller has to say so: wry's built-in handler
            // (`lib.rs:868` of the pinned crate) answers *yes* to every request, so a page the
            // reader opens could write a file into their Downloads folder. The handler is a
            // named function ([`deny_download`]) rather than a closure so the test below can
            // call the same code the builder installs.
            .with_download_started_handler(deny_download)
            // Every permission is denied ([`deny_permission`]). Without a handler macOS grants
            // by default (`wkwebview/class/wry_web_view_ui_delegate.rs:173` falls through to
            // `WKPermissionDecision::Grant`), so a page could ask for the camera, the
            // microphone or a location and get it. `Deny` and not `Default`, because `Default`
            // is the grant on that platform (`:151-153` maps the three).
            .with_permission_handler(deny_permission)
            .build_as_child(&parent);
        return built
            .ok()
            .map(|view| Page {
                url: url.to_string(),
                view,
            });
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = rect;
        None
    }
}

/// One page, owned by the reader view: the engine's view, and the address it is showing.
///
/// **A page can navigate on its own**, and the reader has to be able to show where it went. A
/// link inside the page, a redirect, a meta refresh or a `location.href` in a script all reach
/// the navigation handler, which applies [`allowed`] and lets the page move anywhere inside that
/// rule — `https://example.com/a` can become `https://example.com/b`, and a redirect can leave
/// the host entirely. So the address here is refreshed from the engine on every
/// [`Page::set_bounds`] (the view calls it each frame) and [`Page::url`] answers with it; a view
/// that showed the *story's* host instead would be naming a page the engine is no longer on.
pub struct Page {
    url: String,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    view: wry::WebView,
}

impl Page {
    /// Move the view inside the window, in logical pixels — and take the engine's current
    /// address while it is here.
    ///
    /// Called every frame the view is visible, which is what makes it the right place to ask:
    /// the window can be resized, the rail can fold, the status row can change height under it,
    /// and the page may have navigated since the last frame. The address is read *after* the
    /// bounds so a view that has gone away still answers with the last one it had.
    ///
    /// One frame of lag, and it is deliberate: the reader draws its toolbar before it hands the
    /// page area over, so an address that changed this frame is shown next frame. Reading it
    /// here, once, keeps [`Page::url`] a `&str` instead of a `Result` every caller must handle.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn set_bounds(&mut self, rect: egui::Rect) {
        // A bounds call the engine refuses is not worth a sentence: the view stays where it
        // was, and the next frame asks again.
        let _ = self.view.set_bounds(bounds(rect));
        // `about:blank` and the empty string are what a view reports before its first real
        // navigation; they are not somewhere the page has been, so the address that was asked
        // for stays.
        if let Ok(current) = self.view.url() {
            if !current.is_empty() && current != "about:blank" {
                self.url = current;
            }
        }
    }

    /// Non-engine builds: the type is still `Page`, because [`crate::views::reader`] names it,
    /// but nothing here has a view to move. [`open`] never returns one on this platform.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn set_bounds(&mut self, rect: egui::Rect) {
        let _ = rect;
    }

    /// Load another address in this view. `false` when the address is refused or the engine
    /// will not take it — and then the address this page reports does not change.
    pub fn go(&mut self, url: &str) -> bool {
        if !allowed(url) {
            return false;
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            if self.view.load_url(url).is_err() {
                return false;
            }
            self.url = url.to_string();
            return true;
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = url;
            false
        }
    }

    /// Back one entry in the view's own history.
    pub fn back(&mut self) {
        self.script("history.back()");
    }

    /// Forward one entry in the view's own history.
    pub fn forward(&mut self) {
        self.script("history.forward()");
    }

    /// The address the page is showing, for a caller that wants to draw it.
    ///
    /// It is the address [`open`] or [`go`] asked for, refreshed with the engine's own answer
    /// each [`Page::set_bounds`], so it follows a page that navigated by itself — a link, a
    /// redirect, a meta refresh — anywhere inside [`allowed`]'s rule. That is why a reader's
    /// toolbar should show *this* rather than the stored story's host: past the first click the
    /// two are different pages, and showing the story's host would name the one the engine
    /// left. Borrowed, and cheap: no allocation per frame.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Run one line of script in the view, ignoring what the engine says about it.
    ///
    /// Two callers only ([`Page::back`], [`Page::forward`]), both history moves the page could
    /// make itself: a script the engine refuses leaves the page where it is, which is where the
    /// reader already was.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn script(&mut self, js: &str) {
        let _ = self.view.evaluate_script(js);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn script(&mut self, js: &str) {
        let _ = js;
    }
}

/// Remember the window a page view is a child of.
///
/// Called with the frame's window (`eframe::Frame` is a `HasWindowHandle`), because a borrowed
/// window cannot outlive the frame that named it: this keeps the *handle*, a small `Copy` value
/// naming the platform window, and that window outlives every view drawn inside it.
///
/// A window this platform cannot describe is simply not remembered, and the next frame offers
/// its own.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn attach<W: HasWindowHandle + ?Sized>(window: &W) {
    if let Ok(handle) = window.window_handle() {
        let raw = handle.as_raw();
        PARENT.with(|slot| slot.set(Some(raw)));
    }
}

/// Non-engine builds: the same call compiles, and there is nothing to remember — no view will
/// ever be made from it. The signature is deliberately unconstrained so that a caller can pass
/// its window without this module having an opinion about it.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn attach<W: ?Sized>(_window: &W) {}

// The one handle, for the one window.
//
// A thread-local and not a global: a view is a child of a window on the thread that owns that
// window, and a second thread asking for one gets `None` (and draws text) instead of reaching
// across threads to a window it does not own. (`//` and not `///`: a doc comment on a macro
// invocation is not attached to anything, and rustc says so.)
#[cfg(any(target_os = "macos", target_os = "windows"))]
thread_local! {
    static PARENT: Cell<Option<RawWindowHandle>> = const { Cell::new(None) };
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
use wry::raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};

/// The remembered handle, in the shape the builder takes.
///
/// A `WindowHandle` borrows the `HasWindowHandle` it came from, and that is the frame, which
/// exists for one frame. Keeping the raw handle — a `Copy` value naming a window that outlives
/// the app — and rebuilding the borrowing wrapper here is the only shape that neither leaks a
/// frame nor asks the reader view to hold a borrow across frames.
#[cfg(any(target_os = "macos", target_os = "windows"))]
struct Parent(RawWindowHandle);

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // Safety: `self.0` was copied from a live window this session, and that window outlives
        // every view drawn as its child — the `Page` is dropped with the app, not with a frame.
        unsafe { Ok(WindowHandle::borrow_raw(self.0)) }
    }
}

/// The download handler: nothing is written, ever.
///
/// A page that tries to download — a `Content-Disposition: attachment`, a `<a download>`, a
/// blob saved by script — reaches this. `false` makes the engine answer "no destination", which
/// cancels the download rather than choosing another folder: on macOS `wry`'s download delegate
/// calls the handler with the URL and the suggested path and completes with a null destination
/// on `false` (`wkwebview/download.rs:74-89`), and on Windows the same `false` denies the
/// request (`webview2/mod.rs:955-963`). The suggested path is ignored rather than inspected —
/// there is no folder a page may name that this window would accept, so the check is the
/// answer, not the path.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn deny_download(_url: String, _suggested: &mut std::path::PathBuf) -> bool {
    false
}

/// The permission handler: no camera, no microphone, no location, nothing.
///
/// `Deny` and not `Default`: on macOS `Default` maps to `WKPermissionDecision::Prompt` and the
/// absent-handler case falls through to `Grant` (`wkwebview/class/wry_web_view_ui_delegate.rs:151-173`),
/// so a page that asked for the camera would either get a system dialog this window cannot
/// explain or get the camera outright. Windows maps the same answer to
/// `COREWEBVIEW2_PERMISSION_STATE_DENY`.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn deny_permission(_kind: wry::PermissionKind) -> wry::PermissionResponse {
    wry::PermissionResponse::Deny
}

/// The window named by the last [`attach`], if a frame has named one yet.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn parent() -> Option<Parent> {
    PARENT.with(Cell::get).map(Parent)
}

/// An `egui` rectangle as the engine's logical-pixel rectangle.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn bounds(rect: egui::Rect) -> wry::Rect {
    use wry::dpi::{LogicalPosition, LogicalSize, Position, Size};
    wry::Rect {
        position: Position::Logical(LogicalPosition::new(
            f64::from(rect.min.x),
            f64::from(rect.min.y),
        )),
        size: Size::Logical(LogicalSize::new(
            f64::from(rect.width().max(0.0)),
            f64::from(rect.height().max(0.0)),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The addresses the engine may load.
    #[test]
    fn http_and_https_with_a_host_are_what_the_engine_may_load() {
        for url in [
            "https://example.com/a?b#c",
            "http://127.0.0.1:8081/props",
            "HTTPS://EXAMPLE.COM",
            // The model server's own status surface, by name.
            "http://localhost/status",
            // A query with no path, and a port that is not one of ours.
            "https://example.com?x=1",
            "https://example.com:8443/",
        ] {
            assert!(allowed(url), "{url} must be loadable");
        }
    }

    /// Everything else, and the reason is in [`allowed`]'s doc: a scheme that reads the disk, a
    /// scheme that runs script, a document with no origin, another protocol, credentials, the
    /// browser's own surfaces, an address with no host, and nothing at all.
    #[test]
    fn every_other_scheme_shape_and_credential_is_refused() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "ftp://x",
            "https://user:pw@example.com",
            "https://example.com@evil.test/",
            "about:blank",
            "chrome://settings",
            "",
            "https://",
            "http:///path",
            "http://host:notaport/",
            "http://[::1]/",
            // A newline is how a check and a parser are made to disagree.
            "https://example.com/\nfile:///etc/passwd",
            // Whitespace is refused, not trimmed: a leading space is not the address.
            " https://example.com/",
            "https://example.com/ ",
            "\thttps://example.com/",
            // A backslash: browsers read it as `/`, so the engine would see a different URL
            // from the one checked here (`https://evil.test\.example.com/` is the case that
            // matters — the host it shows is not the host that is loaded).
            "https://example.com\\path",
            "https://evil.test\\.example.com/",
            "https://example.com\\",
            // No scheme: a relative address has no base in this window.
            "//example.com/",
            "example.com",
        ] {
            assert!(!allowed(url), "{url:?} must be refused");
        }
    }

    /// A build with a system engine says so; a build without one says that instead. This is the
    /// flag a caller asks before it decides between a rendered page and text.
    #[test]
    fn the_engine_is_the_platforms_and_is_reported_honestly() {
        assert_eq!(
            engine(),
            cfg!(any(target_os = "macos", target_os = "windows")),
            "engine() must be the cfg it is compiled under"
        );
        #[cfg(target_os = "macos")]
        assert!(engine(), "macOS ships WKWebView, so the engine is there");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert!(!engine(), "no engine crate is compiled on this platform");
    }

    /// Opening a refused address answers `None`: no view is built, nothing panics, and no window
    /// has to exist for the answer to be known.
    #[test]
    fn opening_a_refused_address_answers_none() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "about:blank",
            "",
        ] {
            assert!(open(url, rect).is_none(), "{url:?} opened a view");
        }
    }

    /// A page may neither download a file nor ask for the machine — **and the two pieces of
    /// code the engine calls are tested by calling them**.
    ///
    /// Two halves, because they prove different things:
    ///
    /// * the *behaviour* half calls [`deny_download`] and [`deny_permission`] directly, which is
    ///   the same code object the builder installs. That is the closest a test can come to "the
    ///   engine reached the handler" without a live view: `wry` dispatches a download to the
    ///   stored closure (`wkwebview/download.rs:74-89`, `webview2/mod.rs:955-963`) and a
    ///   permission request likewise (`wry_web_view_ui_delegate.rs:151-173`), so the remaining
    ///   question is whether *this* code answers deny — and here it is, answering.
    /// * the *wiring* half reads this file's own source and checks the builder chain installs
    ///   those two functions and no permissive spelling. A builder cannot be constructed in a
    ///   test: it needs a live window handle, and making one opens a window, which this project
    ///   forbids in a suite.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_page_may_neither_download_a_file_nor_ask_for_the_machine() {
        // --- behaviour: the handlers themselves ------------------------------------------
        let mut suggested = std::path::PathBuf::from("/Users/someone/Downloads/report.pdf");
        for url in [
            "https://example.com/report.pdf",
            "http://127.0.0.1:8081/export.csv",
            "",
        ] {
            assert!(
                !deny_download(url.to_string(), &mut suggested),
                "a download from {url:?} was allowed"
            );
        }
        assert_eq!(
            suggested,
            std::path::PathBuf::from("/Users/someone/Downloads/report.pdf"),
            "the suggested path must be left alone: the answer is the refusal, not a rewrite"
        );
        for kind in [
            wry::PermissionKind::Microphone,
            wry::PermissionKind::Camera,
            wry::PermissionKind::Geolocation,
            wry::PermissionKind::Notifications,
            wry::PermissionKind::ClipboardRead,
            wry::PermissionKind::DisplayCapture,
            wry::PermissionKind::Midi,
            wry::PermissionKind::Sensors,
            wry::PermissionKind::Other,
        ] {
            assert_eq!(
                deny_permission(kind),
                wry::PermissionResponse::Deny,
                "{kind:?} was not denied"
            );
        }

        // --- wiring: the chain installs those two, and nothing permissive ---------------
        let source = include_str!("webview.rs");
        let from = source
            .find("let built = wry::WebViewBuilder::new()")
            .expect("the builder chain is in open()");
        let to = source
            .find(".build_as_child(&parent)")
            .expect("the chain ends at build_as_child");
        assert!(to > from, "build_as_child comes after the chain starts");
        let chain = &source[from..to];
        assert!(
            chain.contains(".with_download_started_handler(deny_download)"),
            "the download handler must be deny_download: this window downloads nothing"
        );
        assert!(
            chain.contains(".with_permission_handler(deny_permission)"),
            "the permission handler must be deny_permission: macOS grants by default"
        );
        // The permissive spellings, named so a failure says what came back. Each is assembled
        // from two pieces: a whole spelling written here would be found by the search above when
        // the search is widened, and the test would fail on itself.
        for permissive in [
            concat!("|_, _", "| true"),
            concat!("PermissionResponse::", "Allow"),
            concat!("PermissionResponse::", "Default"),
        ] {
            assert!(
                !chain.contains(permissive),
                "{permissive:?} is in the builder: the page would be granted something"
            );
        }
    }
}
