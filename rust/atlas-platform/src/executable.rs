//! Is this file executable, and where is the one of that name on `PATH`?
//!
//! Two platforms, two definitions, one module — because `metadata().is_file()`
//! is *not* an answer to "may I start this": on POSIX a plain text file is not
//! executable, and on Windows a file without an executable extension is not
//! either. A crate that treats "the file exists" as "I may spawn it" gets a
//! permission error at the worst moment, or worse, starts something else.
//!
//! The `PATH` search is a pure function over the path *text*: `;` on Windows,
//! `:` on POSIX, and on Windows a name without an extension is tried with each
//! entry of `PATHEXT`, which is how `chrome` becomes `chrome.exe`.

use std::path::{Path, PathBuf};

use crate::host::{self, Os};

/// Extensions Windows will start, as the default `PATHEXT` spells them.
///
/// This is [`host::PATHEXT_DEFAULT`] in the other shape, not a second opinion: it
/// used to carry `.PS1` as well, which Windows will not `CreateProcess`, while
/// the list actually consulted (`host`'s) did not — two answers to one question,
/// drifting. `the_pathext_default_is_written_once` keeps them equal.
pub const WINDOWS_EXECUTABLE_EXTENSIONS: [&str; 4] = [".COM", ".EXE", ".BAT", ".CMD"];

/// May this file be started?
///
/// A regular file. On POSIX it also needs an execute bit; on Windows its
/// extension has to be one the shell starts. A directory is never executable
/// here, and neither is a dangling symlink (`metadata` follows links, so a
/// broken one is simply absent).
pub fn is_executable(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return false,
    };
    if !meta.is_file() {
        return false;
    }
    if host::is_windows() {
        has_executable_extension(path, &pathext_in_force())
    } else {
        mode_has_execute(&meta)
    }
}

/// `PATHEXT` in force: the environment's, or the documented default when it is
/// absent **or empty**.
///
/// An empty `PATHEXT` is a variable that exists and says "no extension is
/// executable", which would make every `.exe` on the machine non-executable and
/// the boot gate refuse to start with all nine binaries reported missing. A
/// stripped environment is the normal shape of a service or a scheduled task, so
/// the empty value falls back to the default instead of being obeyed.
pub fn pathext_in_force() -> String {
    pathext_or_default(std::env::var("PATHEXT").ok().as_deref())
}

/// [`pathext_in_force`] as a pure function of the value that was read.
pub fn pathext_or_default(value: Option<&str>) -> String {
    match value.map(str::trim) {
        Some(found) if !found.is_empty() => found.to_string(),
        _ => host::PATHEXT_DEFAULT.to_string(),
    }
}

/// The POSIX half of [`is_executable`].
fn mode_has_execute(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// The Windows half: does the file name carry an executable extension?
///
/// Case-insensitive, and `pathext` is the `;`-separated list as Windows spells it
/// (`.COM;.EXE;…`) so the rule follows the machine's own setting instead of a
/// second list.
pub fn has_executable_extension(path: &Path, pathext: &str) -> bool {
    let name = match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => name.to_ascii_lowercase(),
        None => return false,
    };
    pathext
        .split(';')
        .map(|ext| ext.trim())
        .filter(|ext| !ext.is_empty())
        .any(|ext| name.ends_with(&ext.to_ascii_lowercase()))
}

/// The candidates a `PATH` search would try, in order — the pure form.
///
/// `os` decides the separator and whether the extensions are applied; nothing
/// here reads the environment or the disk.
///
/// Three shapes of `PATH` that a real machine produces are handled here, because
/// each of them used to produce a path that cannot exist:
///
/// * an entry wrapped in double quotes — installers quote an entry as soon as it
///   contains a space (`"C:\Program Files\Git\cmd"`), and the quotes were kept;
/// * a `PATHEXT` entry without its leading dot (`EXE;COM`), which was appended
///   as-is and spelled `chromeEXE`;
/// * a `UNC` entry (`\\server\share\tools`), which must stay a share and never
///   become a local name.
pub fn path_candidates(name: &str, os: Os, path_value: &str, pathext: &str) -> Vec<PathBuf> {
    let separator = match os {
        Os::Windows => ';',
        Os::Posix => ':',
    };
    let extensions: Vec<String> = pathext
        .split(';')
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(|ext| {
            if ext.starts_with('.') {
                ext.to_string()
            } else {
                format!(".{}", ext)
            }
        })
        .collect();
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in path_value.split(separator) {
        let dir = unquote(dir.trim());
        if dir.is_empty() {
            continue;
        }
        out.push(join(dir, name, os));
        if os == Os::Windows && !name.contains('.') {
            for ext in &extensions {
                out.push(join(dir, &format!("{}{}", name, ext), os));
            }
        }
    }
    out
}

/// A `PATH` entry without the quotes an installer may have written around it.
///
/// Only a *matched* pair of quotes is stripped: a stray quote is left alone, so a
/// nonsense entry stays a nonsense entry instead of becoming a plausible one.
fn unquote(entry: &str) -> &str {
    let bytes = entry.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        entry[1..entry.len() - 1].trim()
    } else {
        entry
    }
}

/// Join two pieces with the separator of `os`.
///
/// A Windows answer is spelled with `\\` even when the question is asked on
/// POSIX, so the candidate list can be asserted on any machine and printed the
/// way a Windows user would type it. Windows accepts both separators, so the
/// result is a valid path there either way.
pub fn join(dir: &str, name: &str, os: Os) -> PathBuf {
    match os {
        Os::Windows => PathBuf::from(format!("{}\\{}", dir.trim_end_matches(['\\', '/']), name)),
        Os::Posix => PathBuf::from(dir).join(name),
    }
}

/// The first of `candidates` that is executable.
pub fn first_executable(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| is_executable(path)).cloned()
}

/// `which name` — the first executable of that name on `PATH`, from this
/// process's environment.
pub fn on_path(name: &str) -> Option<PathBuf> {
    let path_value = std::env::var("PATH").unwrap_or_default();
    let pathext = pathext_in_force();
    let candidates = path_candidates(name, host::os(), &path_value, &pathext);
    first_executable(&candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_path_search_tries_pathext_and_a_posix_one_does_not() {
        let windows = path_candidates(
            "chrome",
            Os::Windows,
            r"C:\Windows;C:\Tools",
            ".COM;.EXE;.BAT",
        );
        assert_eq!(
            windows,
            vec![
                PathBuf::from(r"C:\Windows\chrome"),
                PathBuf::from(r"C:\Windows\chrome.COM"),
                PathBuf::from(r"C:\Windows\chrome.EXE"),
                PathBuf::from(r"C:\Windows\chrome.BAT"),
                PathBuf::from(r"C:\Tools\chrome"),
                PathBuf::from(r"C:\Tools\chrome.COM"),
                PathBuf::from(r"C:\Tools\chrome.EXE"),
                PathBuf::from(r"C:\Tools\chrome.BAT"),
            ]
        );
        let posix = path_candidates("chrome", Os::Posix, "/usr/bin:/bin", ".COM;.EXE");
        assert_eq!(
            posix,
            vec![
                PathBuf::from("/usr/bin/chrome"),
                PathBuf::from("/bin/chrome")
            ]
        );
    }

    #[test]
    fn an_empty_entry_in_the_path_is_skipped_not_treated_as_the_cwd() {
        let found = path_candidates("x", Os::Posix, ":/usr/bin::", "");
        assert_eq!(found, vec![PathBuf::from("/usr/bin/x")]);
    }

    #[test]
    fn a_quoted_path_entry_with_spaces_is_searched_and_not_taken_literally() {
        // A Windows `PATH` written by an installer quotes an entry as soon as it
        // contains a space (`"C:\Program Files\Git\cmd";…`). Trimming whitespace
        // left the quotes in place, so the candidate was `"C:\Program Files\…"` —
        // a file name with a quote in it, which nothing can ever be.
        let found = path_candidates(
            "chrome",
            Os::Windows,
            r#""C:\Program Files\Google\Chrome\Application";C:\Tools"#,
            ".EXE",
        );
        assert_eq!(
            found,
            vec![
                PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome"),
                PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.EXE"),
                PathBuf::from(r"C:\Tools\chrome"),
                PathBuf::from(r"C:\Tools\chrome.EXE"),
            ]
        );
        // A home directory with spaces needs no quoting to work, only the quote
        // stripping above so that a quoted spelling works *too*.
        let spaced = path_candidates("node", Os::Windows, r"C:\Users\Ann Marie\bin", ".EXE");
        assert_eq!(
            spaced,
            vec![
                PathBuf::from(r"C:\Users\Ann Marie\bin\node"),
                PathBuf::from(r"C:\Users\Ann Marie\bin\node.EXE"),
            ]
        );
    }

    #[test]
    fn a_pathext_entry_without_its_dot_still_produces_a_real_file_name() {
        // `PATHEXT` is `.EXE;.COM` on a default machine, but a hand-set one is
        // often `EXE;COM`. Appending that spelled `chromeEXE`, which is not a
        // file name Windows would ever resolve.
        let found = path_candidates("node", Os::Windows, r"C:\Program Files\nodejs", "EXE;.CMD");
        assert_eq!(
            found,
            vec![
                PathBuf::from(r"C:\Program Files\nodejs\node"),
                PathBuf::from(r"C:\Program Files\nodejs\node.EXE"),
                PathBuf::from(r"C:\Program Files\nodejs\node.CMD"),
            ]
        );
    }

    #[test]
    fn a_unc_path_entry_is_searched_as_a_share_and_never_as_a_local_name() {
        let found = path_candidates("chrome", Os::Windows, r"\\srv\share\tools", ".EXE");
        assert_eq!(
            found,
            vec![
                PathBuf::from(r"\\srv\share\tools\chrome"),
                PathBuf::from(r"\\srv\share\tools\chrome.EXE"),
            ]
        );
        // The share prefix survives the join: a candidate is never a local path
        // that merely starts with the server name.
        assert!(!crate::winpath::same_path(
            &found[0].to_string_lossy(),
            r"srv\share\tools\chrome",
            Os::Windows
        ));
        // A share root with a trailing separator is one separator, not two.
        let root = path_candidates("chrome", Os::Windows, r"\\srv\share\", ".EXE");
        assert_eq!(root[0], PathBuf::from(r"\\srv\share\chrome"));
    }

    #[test]
    fn an_empty_pathext_falls_back_to_the_default_instead_of_refusing_everything() {
        // A variable that exists and is empty says "no extension is
        // executable" if it is obeyed, and then `is_executable` answers no for
        // every `.exe` on the machine: the boot gate would report all nine
        // binaries missing and refuse to start. A stripped environment (a
        // service, a scheduled task) is where this arrives.
        assert_eq!(pathext_or_default(Some("")), host::PATHEXT_DEFAULT);
        assert_eq!(pathext_or_default(Some("   ")), host::PATHEXT_DEFAULT);
        assert_eq!(pathext_or_default(None), host::PATHEXT_DEFAULT);
        assert_eq!(pathext_or_default(Some(" .EXE;.COM ")), ".EXE;.COM");
        // And the value in force is never empty, whatever this machine holds.
        assert!(!pathext_in_force().is_empty());
        assert!(has_executable_extension(
            Path::new(r"C:\bin\llama-server.exe"),
            &pathext_or_default(Some(""))
        ));
    }

    #[test]
    fn the_pathext_default_is_written_once() {
        // Two lists described the same rule: this one claimed `.PS1` was
        // startable (Windows will not `CreateProcess` a script) and `host`'s
        // `PATHEXT_DEFAULT` — the list actually consulted — did not. One list now,
        // and this test is what keeps the second from growing back.
        let from_host: Vec<&str> = host::PATHEXT_DEFAULT.split(';').collect();
        assert_eq!(WINDOWS_EXECUTABLE_EXTENSIONS.to_vec(), from_host);
        assert!(!WINDOWS_EXECUTABLE_EXTENSIONS.contains(&".PS1"));
    }

    #[test]
    fn the_executable_extension_test_is_case_insensitive() {
        assert!(has_executable_extension(
            Path::new(r"C:\bin\LLAMA-SERVER.EXE"),
            ".COM;.EXE"
        ));
        assert!(!has_executable_extension(
            Path::new(r"C:\models\model.gguf"),
            ".COM;.EXE"
        ));
        assert!(!has_executable_extension(Path::new("/bin/llama-server"), ""));
    }
}
