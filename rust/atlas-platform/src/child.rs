//! How a child process is started: a curated environment, and nothing else.
//!
//! A child never inherits the calling environment. Nothing from the user's shell
//! — tokens, proxies, API keys, `LD_PRELOAD`/`DYLD_*`, `GIT_*` — reaches
//! `llama-server`, Chrome or an `atlas-*` engine. [`command`] is the only way a
//! crate in this workspace builds a `Command`: it clears the environment, adds
//! the allowlist for this platform, and on Windows hides the console window a
//! GUI-less app would otherwise pop up.
//!
//! Two lists, because the platforms differ: POSIX children need `PATH`, `HOME`,
//! `TMPDIR` and `LANG`; a Windows child cannot even start without `SYSTEMROOT`,
//! and a Chromium profile needs `USERPROFILE`, `APPDATA` and `LOCALAPPDATA`.
//! Both lists are passed in as [`Os`], so the Windows list is testable natively.
//!
//! Two rules decide what a caller may *add* ([`curate`]'s extras): a name that
//! looks like a credential ([`key_is_secret`]) is refused, and so is a name that
//! changes how the child runs rather than what it knows — a proxy, a preloaded
//! library, an interpreter's search path ([`key_is_hijack`]). Both are reported
//! in [`Curated::refused`], so a caller can say which variable it dropped
//! instead of failing quietly.
//!
//! What this module does not do: it does not decide *what* to run (that is the
//! caller's), it never uses a shell, and it never returns the process
//! environment itself — only the `Vec` a spawn primitive consumes.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::host::{self, Os};

/// Explicit allowlist for a POSIX child process.
pub const ENV_ALLOWLIST: [&str; 4] = ["PATH", "HOME", "TMPDIR", "LANG"];

/// Windows adds the directories and variables any child process needs to start
/// at all, plus the three a browser profile reads.
pub const ENV_ALLOWLIST_NT: [&str; 14] = [
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "PATHEXT",
    "COMSPEC",
    "APPDATA",
    "LOCALAPPDATA",
];

/// The allowlist for `os`.
pub fn allowed(os: Os) -> &'static [&'static str] {
    match os {
        Os::Posix => &ENV_ALLOWLIST,
        Os::Windows => &ENV_ALLOWLIST_NT,
    }
}

/// `^[A-Z][A-Z0-9_]{0,63}$` — the only shape a caller may add.
pub fn valid_extra_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    let mut count = 1;
    for ch in chars {
        if !(ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_') {
            return false;
        }
        count += 1;
        if count > 64 {
            return false;
        }
    }
    true
}

/// Key shapes that say their value is a credential — never stored, never passed
/// to a child.
///
/// This is a *storage and spawn* rule (which names may carry a value at all),
/// not a redaction rule: text that is already a log line goes through
/// `atlas-guard`'s redaction. The shape is `config._SECRET_NAME_RE`.
pub fn key_is_secret(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    const PLAIN: [&str; 8] = [
        "pass",
        "secret",
        "token",
        "credential",
        "authorization",
        "cookie",
        "bearer",
        "apikey",
    ];
    if PLAIN.iter().any(|needle| lower.contains(needle)) {
        return true;
    }
    // The two-part spelling: `API_KEY`, `ACCESS-KEY`, `SESSION_ID`. The
    // concatenated form (`APIKEY`) is `PLAIN`'s business; it used to be repeated
    // here as a prefix that then demanded the tail *twice*, so it matched
    // nothing and `apikeykey` — and only that — would have been a credential.
    const PAIRS: [(&str, &str); 4] = [
        ("api", "key"),
        ("access", "key"),
        ("private", "key"),
        ("session", "id"),
    ];
    let paired = PAIRS.iter().any(|(head, tail)| {
        [format!("{}_", head), format!("{}-", head)]
            .iter()
            .any(|prefix| match lower.find(prefix.as_str()) {
                Some(at) => lower[at + prefix.len()..].starts_with(tail),
                None => false,
            })
    });
    if paired {
        return true;
    }
    // The suffix spelling, which is how a key names itself when it is not one of
    // the four pairs above: `ATLAS_SIGNING_KEY`, `GITHUB_AUTH`, `PGPKEY`,
    // `X_OAUTH`. A name that ends in one of these carries a value that must never
    // reach a child; the cost of being wrong is a refused variable and a warning,
    // and the cost of the other error is a credential in another process.
    //
    // `pwd`, `pat` and `dsn` are here because the shape rule above missed them
    // (measured on `MYSQL_PWD`, `GITHUB_PAT`, `DATABASE_DSN`: all three were
    // handed to the child). They are suffixes and not a list of variables, so the
    // rule stays shape-based: a name *ending* in `_PWD`/`_PAT`/`_DSN` is how a
    // credential introduces itself, and no allowlisted name ends in any of them.
    const SUFFIXES: [&str; 7] = ["key", "auth", "secret", "pass", "pwd", "pat", "dsn"];
    SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

/// Key shapes that would change *how* a child runs, rather than what it is
/// configured with: a proxy, a preloaded library, a search path a library
/// loader obeys, a program that runs before the child does.
///
/// This is the other half of the promise in the module comment. The allowlist
/// keeps the caller's environment out of the child, but an extra arrives through
/// the front door, and `LD_PRELOAD` is not a credential: it is a shared object
/// the dynamic linker loads into `llama-server` (or Chrome) before `main`. The
/// same is true of a proxy variable, which silently routes the model's traffic
/// through another machine, and of `GIT_*`, which is how a git hook is told to
/// run something else.
///
/// The rule is shape-based, like [`key_is_secret`], and it is deliberately
/// narrow: `ATLAS_*` is the family ATLAS itself hands to its children (the ports,
/// the data directory), so nothing here may match it.
pub fn key_is_hijack(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    if lower.contains("proxy") {
        return true;
    }
    // The loaders of the three platforms that have one, by their own prefix
    // rather than by an exhaustive list of names: `LD_…` and `DYLD_…` are the
    // dynamic linker's namespaces, `MALLOC_…` overrides the allocator.
    const PREFIXES: [&str; 6] = ["ld_", "dyld_", "malloc_", "git_", "perl5", "ruby"];
    if PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        return true;
    }
    // Interpreters that read their search path or their start-up file from the
    // environment, and the shells that read a start-up file from it.
    const NAMES: [&str; 8] = [
        "node_options",
        "pythonpath",
        "pythonstartup",
        "pythonhome",
        "bash_env",
        "zdotdir",
        "shellopts",
        "globignore",
    ];
    NAMES.contains(&lower.as_str())
}

/// The environment a child gets, and the names that were refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Curated {
    /// The pairs to hand to the spawn primitive, in allowlist order, then the
    /// caller's extras.
    pub env: Vec<(String, String)>,
    /// Extra names that were dropped, so a caller can say so out loud instead of
    /// failing silently.
    pub refused: Vec<String>,
}

/// Build the environment for a child of a caller with environment `caller`.
///
/// `caller` is passed in rather than read, so a test can prove exactly what a
/// child sees — and so this function has no opinion about which process it is in.
pub fn curate(caller: &[(String, String)], extra: &[(String, String)], os: Os) -> Curated {
    let caller_map: BTreeMap<&str, &str> = caller
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let mut env: Vec<(String, String)> = Vec::new();
    for name in allowed(os) {
        if let Some(value) = caller_map.get(name) {
            if !value.is_empty() {
                env.push((name.to_string(), value.to_string()));
            }
        }
    }
    if !env.iter().any(|(name, _)| name == "PATH") {
        let fallback = match os {
            Os::Windows => r"C:\Windows\System32",
            Os::Posix => "/usr/bin:/bin",
        };
        env.push(("PATH".to_string(), fallback.to_string()));
    }
    if os == Os::Posix && !env.iter().any(|(name, _)| name == "TMPDIR") {
        let fallback = caller_map.get("TMPDIR").copied().unwrap_or("/tmp");
        let fallback = if fallback.is_empty() { "/tmp" } else { fallback };
        env.push(("TMPDIR".to_string(), fallback.to_string()));
    }
    let mut refused: Vec<String> = Vec::new();
    for (name, value) in extra {
        // The allowlist names are the caller's own; an extra that repeats one is
        // already what the child gets, so it is accepted, not judged twice.
        let on_allowlist = allowed(os).contains(&name.as_str());
        if !valid_extra_name(name) || key_is_secret(name) || (!on_allowlist && key_is_hijack(name)) {
            refused.push(name.clone());
            continue;
        }
        // A newline or NUL in a value is environment smuggling, not a setting.
        if value.contains('\0') || value.contains('\n') || value.contains('\r') {
            refused.push(name.clone());
            continue;
        }
        if value.chars().count() > 4096 {
            refused.push(name.clone());
            continue;
        }
        match env.iter_mut().find(|(seen, _)| seen == name) {
            Some(slot) => slot.1 = value.clone(),
            None => env.push((name.clone(), value.clone())),
        }
    }
    Curated { env, refused }
}

/// [`curate`] for this process: the allowlist built from *this* environment.
pub fn environment(extra: &[(String, String)]) -> Curated {
    curate(&snapshot(), extra, host::os())
}

/// This process's environment, as pairs. `vars_os` because one non-UTF-8
/// variable must not abort the process on the boot path.
pub fn snapshot() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().to_string(),
                value.to_string_lossy().to_string(),
            )
        })
        .collect()
}

/// A `Command` for a child of ATLAS: no inherited environment, the curated
/// allowlist, and — on Windows — no console window.
///
/// Every spawn site in the workspace builds its command here, so no site can
/// forget `env_clear`, and the Windows-only detail lives in one place instead of
/// behind a `cfg` at each call site.
pub fn command(program: impl AsRef<Path>) -> Command {
    command_with(program, &environment(&[]).env)
}

/// [`command`] with an environment the caller already built (a test, or a site
/// that adds its own variables through [`curate`]).
pub fn command_with(program: impl AsRef<Path>, env: &[(String, String)]) -> Command {
    let mut command = Command::new(program.as_ref());
    command.env_clear();
    for (name, value) in env {
        command.env(name, value);
    }
    hide_console(&mut command);
    command
}

/// No console window for a child of a GUI-less process.
///
/// Windows only, and std's own `CommandExt`: no `unsafe`, no extra crate.
#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn the_allowlist_is_the_documented_one_and_the_extras_come_last() {
        let caller = pairs(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/atlas"),
            ("TMPDIR", "/tmp/atlas"),
            ("LANG", "en_GB.UTF-8"),
            ("GITHUB_TOKEN", "ghp_secret"),
            ("LD_PRELOAD", "/tmp/evil.so"),
            ("http_proxy", "http://proxy"),
        ]);
        let extra = pairs(&[("ATLAS_TEST_FLAG", "1")]);
        let curated = curate(&caller, &extra, Os::Posix);
        let names: Vec<&str> = curated.env.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["PATH", "HOME", "TMPDIR", "LANG", "ATLAS_TEST_FLAG"]
        );
        assert!(curated.refused.is_empty());
    }

    #[test]
    fn the_defaults_a_child_needs_when_the_caller_has_none() {
        let curated = curate(&[], &[], Os::Posix);
        let names: Vec<&str> = curated.env.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["PATH", "TMPDIR"]);
        assert_eq!(curated.env[0].1, "/usr/bin:/bin");
        assert_eq!(curated.env[1].1, "/tmp");

        let curated = curate(&[], &[], Os::Windows);
        let names: Vec<&str> = curated.env.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["PATH"]);
        assert_eq!(curated.env[0].1, r"C:\Windows\System32");
    }

    #[test]
    fn a_secret_named_or_ill_shaped_extra_is_refused_and_reported() {
        let extra = pairs(&[
            ("ATLAS_API_KEY", "x"),
            ("ATLAS_NEWLINE", "a\nb"),
            ("lowercase", "1"),
            ("ATLAS_OK", "yes"),
        ]);
        let curated = curate(&[], &extra, Os::Posix);
        assert_eq!(
            curated.env.iter().map(|(name, _)| name.as_str()).last(),
            Some("ATLAS_OK")
        );
        assert!(curated.refused.contains(&"ATLAS_API_KEY".to_string()));
        assert!(curated.refused.contains(&"ATLAS_NEWLINE".to_string()));
        assert!(curated.refused.contains(&"lowercase".to_string()));
        assert!(!curated.refused.contains(&"ATLAS_OK".to_string()));
    }

    #[test]
    fn a_loader_hijack_or_a_proxy_is_refused_as_an_extra() {
        // The module's promise is that nothing from the shell — a proxy, a
        // preloaded library — reaches a child. The name allowlist kept the
        // *caller's* environment clean, but an extra was only checked against
        // `key_is_secret`, so `LD_PRELOAD` and `HTTPS_PROXY` were passed straight
        // through: a caller could hand llama-server a proxy and a shared object,
        // and `LD_PRELOAD` is not a credential by any name rule.
        let extra = pairs(&[
            ("LD_PRELOAD", "/tmp/evil.so"),
            ("DYLD_INSERT_LIBRARIES", "/tmp/evil.dylib"),
            ("HTTPS_PROXY", "http://proxy.test:3128"),
            ("http_proxy", "http://proxy.test:3128"),
            ("ALL_PROXY", "socks5://proxy.test"),
            ("NO_PROXY", "localhost"),
            ("GIT_SSH_COMMAND", "/tmp/evil"),
            ("ATLAS_PORT", "8899"),
        ]);
        for os in [Os::Posix, Os::Windows] {
            let curated = curate(&[], &extra, os);
            let names: Vec<&str> = curated.env.iter().map(|(name, _)| name.as_str()).collect();
            for refused in [
                "LD_PRELOAD",
                "DYLD_INSERT_LIBRARIES",
                "HTTPS_PROXY",
                "http_proxy",
                "ALL_PROXY",
                "NO_PROXY",
                "GIT_SSH_COMMAND",
            ] {
                assert!(
                    curated.refused.contains(&refused.to_string()),
                    "{} must be refused on {:?}",
                    refused,
                    os
                );
                assert!(
                    !names.contains(&refused),
                    "{} must not reach the child on {:?}",
                    refused,
                    os
                );
            }
            // A port is not a proxy: the e2e launcher's own extras still pass.
            assert!(names.contains(&"ATLAS_PORT"), "{:?}: {:?}", os, names);
            assert!(!curated.refused.contains(&"ATLAS_PORT".to_string()));
        }
    }

    #[test]
    fn a_credential_is_recognised_by_the_shape_of_its_name() {
        // The shapes a key names itself with: a word, a two-part spelling, and
        // the suffix `_KEY`/`_AUTH` that the pairs alone missed, so
        // `ATLAS_SIGNING_KEY` was passed to a child as if it were a setting.
        for secret in [
            "GITHUB_TOKEN",
            "ATLAS_API_KEY",
            "ACCESS_KEY",
            "SESSION_ID",
            "ATLAS_SIGNING_KEY",
            "GITHUB_AUTH",
            "PGPKEY",
            "X_OAUTH",
            "DB_PASSWORD",
            "HTTP_COOKIE",
            "AWS_SECRET_ACCESS_KEY",
            // The three shapes the suffix list did not carry, measured being
            // passed to a child before `pwd`/`pat`/`dsn` were added: each is a
            // credential's own name for itself, and none is an ATLAS setting.
            "MYSQL_PWD",
            "GITHUB_PAT",
            "DATABASE_DSN",
            "AWS_SESSION_TOKEN",
        ] {
            assert!(key_is_secret(secret), "{} carries a credential", secret);
        }
        // And a name that only looks similar is a setting, not a credential.
        for setting in [
            "ATLAS_PORT",
            "ATLAS_LLAMA_PORT",
            "ATLAS_CDP_PORT",
            "ATLAS_DATA_DIR",
            "ATLAS_KEYBOARD",
            "ATLAS_AUTHOR",
            "API_VERSION",
            "PATH",
            "LANG",
            "HOME",
        ] {
            assert!(!key_is_secret(setting), "{} is a setting", setting);
        }
    }

    #[test]
    fn nothing_the_allowlist_carries_is_something_the_rules_refuse() {
        // The two halves must agree: a name on the allowlist that the refusal
        // rule rejects is a variable the child silently never gets, and the two
        // lists would drift apart silently. Both directions are asked through
        // `curate`, so this test cannot pass by re-implementing the rule.
        for os in [Os::Posix, Os::Windows] {
            for name in allowed(os) {
                let caller = pairs(&[(name, "value")]);
                let curated = curate(&caller, &[], os);
                assert!(
                    curated.env.iter().any(|(seen, _)| seen == name),
                    "{} is on the {:?} allowlist but the caller's value was dropped",
                    name,
                    os
                );
                // The same name as an *extra* must be accepted too, or a site
                // that sets it explicitly would be refused a variable it already
                // gets.
                let extra = pairs(&[(name, "value")]);
                let curated = curate(&[], &extra, os);
                assert!(
                    !curated.refused.contains(&name.to_string()),
                    "{} is on the {:?} allowlist but refused as an extra",
                    name,
                    os
                );
            }
        }
    }

    #[test]
    fn a_child_command_starts_from_an_empty_environment() {
        let compiled = command("atlas-guard");
        let env: Vec<_> = compiled.get_envs().collect();
        assert!(!env.is_empty(), "the curated environment must be applied");
        for (name, value) in env {
            let name = name.to_string_lossy().to_string();
            assert!(
                allowed(host::os()).contains(&name.as_str()),
                "{} is not on the allowlist",
                name
            );
            assert!(value.is_some(), "{} must carry a value", name);
        }
    }
}
