//! The shape and the validity of Windows path text — on every platform.
//!
//! ATLAS stores paths in `data/config.json`, in the database and in the
//! arguments it hands to binaries. That file may be written on Windows and read
//! on POSIX, so a path is *text* here, not a `PathBuf`: the Windows rules are
//! applied by this module wherever it compiles, which is the only way the rules
//! get tested on the machine the tests actually run on.
//!
//! ## What this module decides
//!
//! * **Both separators are separators.** `C:\a\b` and `C:/a/b` are the same
//!   path, always — that is the decision a `.json` written on Windows needs in
//!   order to resolve elsewhere. (`std::path` on POSIX would say the backslash
//!   is part of a file name, which is true of the filesystem and false of the
//!   file ATLAS wrote.)
//! * **The prefix survives normalisation.** `\\server\share\x`,
//!   `\\?\C:\x` and `\\.\pipe\p` are not "a path that starts with a slash":
//!   collapsing `//` to `/` would quietly turn a network path into a local one
//!   on the drive of the current directory. [`normalise`] keeps the prefix
//!   byte for byte and collapses only the components after it.
//! * **A name Windows would silently reinterpret is refused.**
//!   [`name_problem`] answers for one component: `CON`/`NUL`/`COM1` and the rest
//!   of the device namespace, a trailing dot, a trailing space, an alternate
//!   data stream (`file:stream`), and the characters the file system cannot
//!   hold. Writing `models/CON.gguf` does not create a file; it writes to a
//!   device, so a caller that means "a file inside the project" must refuse it.
//! * **Containment is a comparison of normalised text**, case-folded exactly
//!   where the file system folds it ([`crate::host::folds_case_of`]) and only as
//!   far as it can be reproduced without the file system's own table: ASCII
//!   (see [`fold`] — a Unicode fold can turn two names into one and that is a
//!   false "inside", the one direction this module may not go).
//!
//! ## What it does not decide
//!
//! Whether the file exists, whether it is executable, and whether the *parent*
//! of a path is the app root — that is [`crate::root`]. Nothing here touches the
//! disk.

use std::fmt;

use crate::host::{folds_case_of, Os};

/// Everything from the start of `text` up to here is a prefix, not a component.
///
/// A prefix is one of: the device form `//?/` or `//./` (with `//?/UNC/server/share`
/// and `//?/C:` extending it), the UNC form `//server/share`, a drive `C:`, or
/// the single `/` of a drive-less rooted path. Anything else has no prefix — and
/// in particular path *text* that begins with a character of several bytes has
/// none, because a drive letter is one ASCII byte.
///
/// The answer is always a character boundary, because every byte it counts is
/// ASCII. Callers split the string there ([`split_prefix`]).
fn prefix_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    // Every test here reads *bytes*, never `&text[..n]`: a path component may
    // begin with a character of three or four bytes (`中`, `🙂`), and slicing two
    // bytes into it panics. A path is user text — it may be refused, it may be
    // relative, it may not crash the process that configured it.
    //
    // Every offset this function returns therefore follows ASCII, which is what
    // lets the callers split the string at it.
    // The device namespace first: `//?/…` and `//./…` would otherwise look like
    // a UNC server named `?`.
    if device_prefix_at(bytes, 0) {
        if unc_at(bytes, 4) {
            let mut at = 8;
            // server, then share
            for _ in 0..2 {
                at += component_len(&text[at..]);
                if bytes.get(at) == Some(&b'/') {
                    at += 1;
                }
            }
            return at.min(text.len());
        }
        if drive_at(bytes, 4) {
            return 6;
        }
        return 4;
    }
    if bytes.len() >= 2 && bytes[0] == b'/' && bytes[1] == b'/' {
        let mut at = 2;
        for _ in 0..2 {
            at += component_len(&text[at..]);
            if bytes.get(at) == Some(&b'/') {
                at += 1;
            }
        }
        return at.min(text.len());
    }
    if drive_at(bytes, 0) {
        return 2;
    }
    if bytes.first() == Some(&b'/') {
        return 1;
    }
    0
}

/// Bytes of the first component of `text` (up to the next separator).
fn component_len(text: &str) -> usize {
    text.find('/').unwrap_or(text.len())
}

/// `C:` at byte `at` — a drive letter followed by a colon.
fn drive_at(bytes: &[u8], at: usize) -> bool {
    bytes.len() >= at + 2 && bytes[at].is_ascii_alphabetic() && bytes[at + 1] == b':'
}

/// `UNC/` at byte `at`, case-insensitively.
fn unc_at(bytes: &[u8], at: usize) -> bool {
    bytes.len() >= at + 4 && bytes[at..at + 4].eq_ignore_ascii_case(b"UNC/")
}

/// `//?/` or `//./` at byte `at`: the start of a device-namespace path.
fn device_prefix_at(bytes: &[u8], at: usize) -> bool {
    bytes.len() >= at + 4
        && bytes[at] == b'/'
        && bytes[at + 1] == b'/'
        && (bytes[at + 2] == b'?' || bytes[at + 2] == b'.')
        && bytes[at + 3] == b'/'
}

/// `text` split at a byte offset a prefix length named.
///
/// A prefix length always follows ASCII (see [`prefix_len`]), so the split is a
/// character boundary; `get` is used anyway, so that a future rule that answers
/// otherwise is a refusal rather than a panic in a path comparison.
fn split_prefix(text: &str, len: usize) -> (&str, &str) {
    match (text.get(..len), text.get(len..)) {
        (Some(prefix), Some(rest)) => (prefix, rest),
        _ => ("", text),
    }
}

/// Backslashes become slashes. This is the whole of "both separators are
/// separators", and it is applied before every other function in this module.
pub fn unify(text: &str) -> String {
    if text.contains('\\') {
        text.replace('\\', "/")
    } else {
        text.to_string()
    }
}

/// What kind of path this text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// No characters at all.
    Empty,
    /// `a/b` — resolved against the current directory.
    Relative,
    /// `\a\b` or `/a/b` — resolved against the current *drive*.
    Rooted,
    /// `C:\a` — absolute.
    DriveAbsolute,
    /// `C:a` — relative to the current directory **on** drive `C`. Windows reads
    /// this happily and it almost never means what the author meant.
    DriveRelative,
    /// `\\server\share\a` — absolute on another machine.
    Unc,
    /// `\\?\C:\a` — the extended-length form `canonicalize` returns.
    VerbatimDisk,
    /// `\\?\UNC\server\share\a`.
    VerbatimUnc,
    /// `\\.\pipe\p`, `\\?\GLOBALROOT\…` — a device, not a file.
    Device,
}

impl Shape {
    /// Is this an absolute path in the sense a caller means (it names a root)?
    pub fn is_absolute(self) -> bool {
        matches!(
            self,
            Shape::DriveAbsolute | Shape::Unc | Shape::VerbatimDisk | Shape::VerbatimUnc
        )
    }

    /// Does this path escape the local file system (a device or a UNC share)?
    pub fn is_remote_or_device(self) -> bool {
        matches!(
            self,
            Shape::Unc | Shape::VerbatimUnc | Shape::Device
        )
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Shape::Empty => "empty",
            Shape::Relative => "relative",
            Shape::Rooted => "rooted",
            Shape::DriveAbsolute => "drive-absolute",
            Shape::DriveRelative => "drive-relative",
            Shape::Unc => "unc",
            Shape::VerbatimDisk => "verbatim-disk",
            Shape::VerbatimUnc => "verbatim-unc",
            Shape::Device => "device",
        })
    }
}

/// The shape of `text`, on either platform (a Windows path can arrive anywhere).
pub fn shape(text: &str) -> Shape {
    let text = unify(text);
    if text.is_empty() {
        return Shape::Empty;
    }
    let (prefix, rest) = split_prefix(&text, prefix_len(&text));
    let bytes = text.as_bytes();
    // The device namespace first: `//?/UNC/server/share` is a UNC path in the
    // extended-length spelling, `//?/C:` is a disk path, `//./pipe` is a device.
    if device_prefix_at(bytes, 0) {
        if unc_at(bytes, 4) {
            return Shape::VerbatimUnc;
        }
        if bytes[2] == b'?' && drive_at(bytes, 4) {
            return Shape::VerbatimDisk;
        }
        return Shape::Device;
    }
    if prefix.len() > 1 && prefix.starts_with("//") {
        Shape::Unc
    } else if drive_at(bytes, 0) {
        if rest.starts_with('/') {
            Shape::DriveAbsolute
        } else {
            Shape::DriveRelative
        }
    } else if prefix == "/" {
        Shape::Rooted
    } else {
        Shape::Relative
    }
}

/// The extended-length prefix removed, the separators unified.
///
/// `\\?\C:\a` → `C:/a`, `\\?\UNC\srv\share\a` → `//srv/share/a`. The prefix is a
/// DOS-era compatibility marker, not part of the path, and comparing it against
/// a path that lacks it is the bug that makes every file look like a symlink on
/// Windows.
///
/// Only the two forms that name a file are unwrapped. `\\?\` also opens the
/// device namespace (`\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy1\…`,
/// `\\?\Volume{…}\…`), and unwrapping one of those used to answer
/// `GLOBALROOT/Device/…` — a **relative** path, so a device path compared equal
/// to a project-relative name (measured: `same_path("GLOBALROOT/…/x",
/// "\\\\?\\GLOBALROOT\\…\\x")` was `true`). A device path is returned unchanged:
/// it is not a file path and must not be compared as one, and
/// [`path_problem`] refuses it for the same reason.
pub fn strip_verbatim(text: &str) -> String {
    let text = unify(text);
    let bytes = text.as_bytes();
    if device_prefix_at(bytes, 0) && bytes[2] == b'?' {
        if unc_at(bytes, 4) {
            return format!("//{}", &text[8..]);
        }
        if drive_at(bytes, 4) {
            return text[4..].to_string();
        }
    }
    text
}

/// Collapse `.`, `..` and repeated separators without touching the disk; the
/// prefix is preserved and `..` never climbs above a root.
///
/// `C:\a\..\b` → `C:/b`, `\\srv\share\..\..\x` → `//srv/share/x` (a share cannot
/// climb above itself), `a/../..` → `..` (a relative path may).
pub fn normalise(text: &str) -> String {
    let text = unify(text);
    if text.is_empty() {
        return String::new();
    }
    let (prefix, rest) = split_prefix(&text, prefix_len(&text));
    let absolute = !prefix.is_empty() && !matches!(shape(&text), Shape::DriveRelative);
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(last) if *last != ".." => {
                    parts.pop();
                }
                _ => {
                    if !absolute {
                        parts.push("..");
                    }
                }
            },
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    // `C:foo` is relative to the current directory *on* C:, so it keeps the
    // absent separator: `C:foo/../bar` is `C:bar`, not `C:/bar`, which would be
    // the drive root.
    if matches!(shape(&text), Shape::DriveRelative) && !joined.is_empty() {
        return format!("{}{}", prefix, joined);
    }
    if prefix.is_empty() {
        return if joined.is_empty() {
            ".".to_string()
        } else {
            joined
        };
    }
    let sep = if prefix.ends_with('/') { "" } else { "/" };
    if joined.is_empty() {
        let trimmed = prefix.trim_end_matches('/');
        return if trimmed.is_empty() {
            "/".to_string()
        } else {
            trimmed.to_string()
        };
    }
    format!("{}{}{}", prefix, sep, joined)
}

/// Why a path or a component of one cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// No characters at all.
    Empty,
    /// A control character (`\0` … `\x1f`, `\x7f`): truncation on the way into
    /// a C string, and never legitimate in a name.
    Control,
    /// A character Windows cannot store in a name.
    Illegal(char),
    /// `CON`, `NUL`, `COM1`, `CONOUT$`, … — a device, not a file.
    ReservedDevice(String),
    /// A component that ends with `.`; Windows drops the dot, so the name
    /// written and the name created differ.
    TrailingDot,
    /// A component that ends with a space; same story.
    TrailingSpace,
    /// The device namespace (`\\.\pipe\…`): not a file path at all, and the
    /// Win32 normaliser does not check its components, so ATLAS refuses it
    /// instead of guessing what the device would do.
    DeviceNamespace,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::Empty => f.write_str("the path is empty"),
            Problem::Control => f.write_str("the path contains a control character"),
            Problem::Illegal(ch) => write!(f, "the character {:?} cannot appear in a name", ch),
            Problem::ReservedDevice(name) => write!(
                f,
                "{} is a Windows device name, not a file name",
                name
            ),
            Problem::TrailingDot => f.write_str("a name component ends with a dot"),
            Problem::TrailingSpace => f.write_str("a name component ends with a space"),
            Problem::DeviceNamespace => {
                f.write_str("the device namespace is not a file path")
            }
        }
    }
}

/// The Windows device namespace, as a rule rather than a list of files.
///
/// These are the names the file system reserves in *any* directory and with
/// *any* extension: `NUL.gguf` is still the null device.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The three names Windows reserves with a superscript digit, which are the same
/// devices with a different spelling.
const RESERVED_SUPERSCRIPT: [&str; 6] = ["COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³"];

/// The two console names, reserved with a trailing `$`.
const RESERVED_CONSOLE: [&str; 2] = ["CONIN$", "CONOUT$"];

/// Is this one path component a reserved device name?
///
/// Case-insensitive, and the extension does not save it: Windows compares the
/// part before the first dot, after dropping trailing dots and spaces.
pub fn is_reserved_device(segment: &str) -> bool {
    let base = segment.split('.').next().unwrap_or("");
    let base = base.trim_end_matches(|c| c == ' ' || c == '.');
    if base.is_empty() {
        return false;
    }
    let upper = base.to_ascii_uppercase();
    RESERVED.contains(&upper.as_str())
        || RESERVED_CONSOLE.contains(&upper.as_str())
        || RESERVED_SUPERSCRIPT
            .iter()
            .any(|name| base.eq_ignore_ascii_case(name))
}

/// The first thing wrong with one component of a Windows path, if anything.
pub fn name_problem(segment: &str) -> Option<Problem> {
    if segment.is_empty() {
        return Some(Problem::Empty);
    }
    for ch in segment.chars() {
        let code = ch as u32;
        if code < 0x20 || code == 0x7f {
            return Some(Problem::Control);
        }
        // `:` covers the alternate data stream: `notes.txt:hidden` is a stream
        // on a file, not a file, and it will not be listed or cleaned up.
        if matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            return Some(Problem::Illegal(ch));
        }
    }
    if segment.ends_with('.') {
        return Some(Problem::TrailingDot);
    }
    if segment.ends_with(' ') {
        return Some(Problem::TrailingSpace);
    }
    if is_reserved_device(segment) {
        return Some(Problem::ReservedDevice(segment.to_string()));
    }
    None
}

/// The first thing wrong with `text` on `os`.
///
/// On Windows every component is judged; on POSIX only a NUL byte is refused,
/// because everything else is a legal file name there — refusing `models/CON`
/// would refuse a file a POSIX machine created legitimately.
pub fn path_problem(text: &str, os: Os) -> Option<Problem> {
    if text.trim().is_empty() {
        return Some(Problem::Empty);
    }
    if text.contains('\0') {
        return Some(Problem::Control);
    }
    if os == Os::Posix {
        return None;
    }
    let unified = unify(text);
    let (_, rest) = split_prefix(&unified, prefix_len(&unified));
    // A verbatim path is passed through to the file system untouched by the
    // Win32 normaliser, which means its components are *not* checked by Windows
    // either — and that is exactly why ATLAS refuses it rather than guessing.
    if matches!(shape(&unified), Shape::Device) {
        return Some(Problem::DeviceNamespace);
    }
    for segment in rest.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            continue;
        }
        if let Some(problem) = name_problem(segment) {
            return Some(problem);
        }
    }
    None
}

/// Do two paths name the same place?
///
/// Separators are unified, `.`/`..` are collapsed, the extended-length prefix is
/// dropped, a trailing separator is ignored, and case is folded exactly where
/// the file system folds it.
pub fn same_path(a: &str, b: &str, os: Os) -> bool {
    let left = compare_key(a, os);
    let right = compare_key(b, os);
    left == right
}

/// The form two paths are compared in: normalised, verbatim prefix stripped,
/// case folded on Windows.
pub fn compare_key(text: &str, os: Os) -> String {
    let normal = strip_verbatim(&normalise(text));
    if folds_case_of(os) {
        fold(&normal)
    } else {
        normal
    }
}

/// Windows' case folding, as far as this crate is willing to claim it: **ASCII
/// only, one byte per byte**.
///
/// `str::to_lowercase` was used here, and it is not Windows' rule. It is Unicode
/// full case mapping, which maps one character to **several** (`İ` U+0130 →
/// `i` + U+0307) and across scripts (`K` U+212A KELVIN SIGN → `k`). Measured
/// before the fix, on the Windows rule:
///
/// ```text
/// same_path("C:/a/İ", "C:/a/i̇")        -> true
/// inside("C:/a/İ", "C:/a/i̇/x")          -> true
/// same_path("C:/a/K", "C:/a/k")   (K = U+212A) -> true
/// ```
///
/// `C:\a\İ` and `C:\a\i̇` are two different directories on NTFS, so a folded
/// comparison that calls them one string **widens** a containment verdict — the
/// one direction a security comparison may never go. NTFS folds with a per-code-
/// unit table (`$UpCase`), which never changes a string's length; without that
/// table the safe rule is the one byte-per-byte subset every Windows build
/// agrees on. The cost is the other direction, and it is the acceptable one: a
/// non-ASCII name typed in the wrong case is refused rather than matched. A
/// false negative is a 404 or a refusal; a false positive is an escape.
fn fold(text: &str) -> String {
    text.to_ascii_lowercase()
}

/// Is `target` inside `base` (or the same path)?
///
/// Lexical on purpose: the caller that needs the file system's opinion asks
/// [`crate::root::inside_root`], which canonicalises first and then calls this.
///
/// Two refusals come before the prefix comparison, because a base that is not a
/// root cannot contain what sits above it:
///
/// * `base` empty contains only the empty path;
/// * `base` `.` is the current directory, and a target that still climbs
///   ([`climbs_out`]) after normalisation points **above** it. Measured before
///   the fix: `inside(".", "..")` and `inside(".", "../outside/x")` both answered
///   `true`, so a caller that passed `.` for "the project" was told every parent
///   of its working directory was inside the project.
pub fn inside(base: &str, target: &str, os: Os) -> bool {
    let base = strip_verbatim(&normalise(base));
    let target = strip_verbatim(&normalise(target));
    let (base, target) = if folds_case_of(os) {
        (fold(&base), fold(&target))
    } else {
        (base, target)
    };
    if base.is_empty() {
        return target.is_empty();
    }
    if base == "." {
        // `target` is normalised, so a climbing one is exactly `..` or starts
        // with `../`: `..foo` is an ordinary directory name and stays inside.
        let climbs = target == ".." || target.starts_with("../");
        return !target.starts_with('/') && !target.contains(':') && !climbs;
    }
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        // The file system root: everything absolute is inside it.
        return target.starts_with('/');
    }
    if target == base {
        return true;
    }
    // `C:` is the drive root, so `C:/a` is inside it; every other base needs the
    // separator to follow, so `C:/app` does not contain `C:/application`.
    match target.strip_prefix(base) {
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

/// Does this text still climb after normalisation — that is, is it a relative
/// path whose `..` survives because there is no root to clamp it?
pub fn climbs_out(text: &str) -> bool {
    normalise(text).starts_with("..")
}

/// The components of a path, prefix excluded, in order.
pub fn components(text: &str) -> Vec<String> {
    let unified = unify(&normalise(text));
    let (_, rest) = split_prefix(&unified, prefix_len(&unified));
    rest.split('/')
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unicode_drive_path_never_loses_its_drive() {
        assert_eq!(normalise(r"C:\Program Files\ATLAS\data"), "C:/Program Files/ATLAS/data");
        assert_eq!(normalise("c:/a//b/./c"), "c:/a/b/c");
        assert_eq!(normalise(r"\\?\C:\a\b"), "//?/C:/a/b");
    }

    #[test]
    fn dot_dot_is_clamped_at_a_root_and_kept_in_a_relative_path() {
        assert_eq!(normalise(r"C:\a\..\..\..\b"), "C:/b");
        assert_eq!(normalise("a/../.."), "..");
        assert_eq!(normalise(r"\\srv\share\..\..\x"), "//srv/share/x");
        assert_eq!(normalise("/a/../b"), "/b");
    }

    #[test]
    fn path_text_that_is_not_ascii_is_never_sliced_mid_character() {
        // A directory can be named in any script, and every configured path is
        // text this module reads. `text[..2]` used to be how the drive letter and
        // the `//?/` prefix were read, so a first character of three or four
        // bytes (中, 🙂) made slicing panic — a crash on a path a user typed,
        // not a refusal.
        for text in ["中文/目录", "🙂/atlas", "日本語", "//?/中文", "//?/🙂\\x", "C:/中文/目录"] {
            let normal = normalise(text);
            assert!(!normal.is_empty(), "{} normalised to nothing", text);
            let _ = shape(text);
            let _ = strip_verbatim(text);
            let _ = components(text);
            let _ = path_problem(text, Os::Windows);
            let _ = path_problem(text, Os::Posix);
            let _ = climbs_out(text);
        }
        // The rules are unchanged for text that happens to be non-ASCII: a
        // three-byte first character is a relative path, not a drive.
        assert_eq!(shape("中文"), Shape::Relative);
        assert_eq!(normalise("中文/../目录"), "目录");
        assert_eq!(shape("//?/中文"), Shape::Device);
        assert_eq!(strip_verbatim("//?/UNC/服务器/共享/x"), "//服务器/共享/x");
        assert_eq!(normalise(r"C:\程序 Files\ATLAS\data"), "C:/程序 Files/ATLAS/data");
        assert_eq!(components(r"C:\程序 Files\ATLAS"), vec!["程序 Files", "ATLAS"]);
        // A non-ASCII name is a legal file name on both platforms.
        assert_eq!(name_problem("报告.gguf"), None);
        assert_eq!(path_problem(r"C:\app\models\模型.gguf", Os::Windows), None);
        // And containment still answers, in either separator style.
        assert!(inside(r"C:\程序 Files\ATLAS", r"C:\程序 Files\ATLAS\data", Os::Windows));
        assert!(same_path("C:/中文/a", r"C:\中文\a", Os::Windows));
    }

    #[test]
    fn the_device_prefix_is_its_own_shape() {
        assert_eq!(shape(r"\\?\C:\a"), Shape::VerbatimDisk);
        assert_eq!(shape(r"\\?\UNC\srv\share\a"), Shape::VerbatimUnc);
        assert_eq!(shape(r"\\.\pipe\atlas"), Shape::Device);
        assert_eq!(shape(r"\\srv\share\a"), Shape::Unc);
        assert_eq!(shape(r"C:\a"), Shape::DriveAbsolute);
        assert_eq!(shape("C:a"), Shape::DriveRelative);
        assert_eq!(shape(r"\a\b"), Shape::Rooted);
        assert_eq!(shape("a/b"), Shape::Relative);
        assert_eq!(shape(""), Shape::Empty);
    }

    #[test]
    fn a_device_namespace_is_never_unwrapped_into_a_relative_path() {
        // `\\?\` opens the device namespace as well as naming a long file, and
        // unwrapping one of those turned `\\?\GLOBALROOT\Device\…\x` into
        // `GLOBALROOT/Device/…/x` — a path that looks project-relative, so it
        // compared equal to a relative name. `shape` always called it a device;
        // `strip_verbatim` must agree, and a device path is never a file path.
        for device in [
            r"\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy1\Windows\System32\config\SAM",
            r"\\?\Volume{1234abcd-0000-0000-0000-000000000000}\data\atlas.db",
            r"\\.\pipe\atlas",
            r"\\.\C:\app\x",
        ] {
            assert_eq!(
                strip_verbatim(device),
                unify(device),
                "{} must not be unwrapped",
                device
            );
            assert_eq!(shape(device), Shape::Device, "{} is a device", device);
        }
        // The measured false positive: a device path that compared equal to a
        // relative name.
        assert!(!same_path(
            "GLOBALROOT/Device/HarddiskVolumeShadowCopy1/x",
            r"\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy1\x",
            Os::Windows
        ));
        // The two file forms still unwrap, or every file on Windows looks like a
        // symlink to a caller that canonicalised it.
        assert_eq!(strip_verbatim(r"\\?\C:\app\x"), "C:/app/x");
        assert_eq!(strip_verbatim(r"\\?\UNC\srv\share\x"), "//srv/share/x");
    }

    #[test]
    fn case_folding_is_ascii_because_a_unicode_fold_only_ever_widens() {
        // Windows folds case one code unit at a time; `str::to_lowercase` is
        // Unicode full case mapping, which maps one character to several and
        // across scripts. Measured before the fix, on this machine:
        //   same_path("C:/a/İ", "C:/a/i̇")                    -> true
        //   inside("C:/a/İ", "C:/a/i̇/x")                      -> true
        //   same_path("C:/a/\u{212a}", "C:/a/k")  (KELVIN)     -> true
        // `C:\a\İ` and `C:\a\i̇` are two directories on NTFS, so `true` here is a
        // containment verdict that is wider than the file system's.
        assert!(!same_path("C:/a/\u{130}", "C:/a/i\u{307}", Os::Windows));
        assert!(!inside("C:/a/\u{130}", "C:/a/i\u{307}/x", Os::Windows));
        assert!(!same_path("C:/a/\u{212a}", "C:/a/k", Os::Windows));
        assert!(!same_path("C:/a/\u{212b}", "C:/a/\u{e5}", Os::Windows));
        // ASCII folding — the part every Windows build agrees on — still works,
        // in both directions, and the extension's case never mattered.
        assert!(same_path("C:/App/DATA/atlas.DB", "c:/app/data/atlas.db", Os::Windows));
        assert!(inside("C:/APP", "c:/app/sub/x", Os::Windows));
        // POSIX folds nothing, and a non-ASCII name is ordinary text there.
        assert!(!same_path("/App/\u{130}", "/app/\u{130}", Os::Posix));
        assert!(same_path("/App/\u{130}", "/App/\u{130}", Os::Posix));
    }

    #[test]
    fn a_short_name_is_not_guessed_at() {
        // `PROGRA~1` is `Program Files` under 8.3. Folding a short name into its
        // long name needs `GetLongPathName`, which this crate does not have and
        // will not guess at, so the two spellings are **different paths** here.
        // That is the fail-closed direction: a short name is refused, never
        // resolved to a name nobody checked. Both halves are pinned so a future
        // change to this has to argue with a test.
        assert!(!same_path("C:/PROGRA~1/x", "C:/Program Files/x", Os::Windows));
        assert!(!inside("C:/Program Files", "C:/PROGRA~1/x", Os::Windows));
        // `~` in an ordinary name is still ordinary text on both platforms.
        assert!(same_path("C:/a/b~1", "c:/A/B~1", Os::Windows));
        assert!(same_path("/a/b~1", "/a/b~1", Os::Posix));
    }

    #[test]
    fn a_relative_base_cannot_contain_what_sits_above_it() {
        // `.` is the working directory. Before the fix, `inside(".", "..")` and
        // `inside(".", "../outside/x")` both answered `true`: a caller that
        // passed `.` for "the project" was told every parent of its working
        // directory was inside the project.
        for climbing in ["..", "../outside/x", "../../etc/passwd", "a/../../x"] {
            assert!(
                !inside(".", climbing, Os::Posix),
                "{} must not be inside .",
                climbing
            );
        }
        // What is genuinely relative to it still is, including a name that only
        // starts with two dots.
        for here in ["x", "a/b", "..foo/bar", "."] {
            assert!(inside(".", here, Os::Posix), "{} is inside .", here);
        }
        // An empty base contains only itself.
        assert!(inside("", "", Os::Posix));
        assert!(!inside("", "x", Os::Posix));
        // And the ordinary answers are untouched.
        assert!(inside("/app", "/app/x", Os::Posix));
        assert!(!inside("/app", "/app/../x", Os::Posix));
        assert!(!inside("/app", "/application/x", Os::Posix));
    }
}
