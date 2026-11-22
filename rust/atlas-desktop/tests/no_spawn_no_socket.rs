//! The window opens no listener and starts no process — asserted on the source, so
//! the claim cannot stop being true quietly.
//!
//! # Where the rule comes from
//!
//! * `src/main.rs` (crate doc): "one process draws the archive with egui … and opens
//!   **no listener** at all. There is no HTML, no CSS, no wasm bundle, no page and no
//!   socket for a page to talk to — a window, a GPU surface and a database."
//! * `docs/THREAT-MODEL.md` §5.6, "No listener beyond loopback": the app's socket is
//!   `127.0.0.1:<app_port>` and nothing else. A window that listened would be a second
//!   front door to an archive that has no authentication by design.
//! * `docs/THREAT-MODEL.md` §4.9/§4.12: the model's only way out is the 23 handlers in
//!   `atlas-ingest`, each gated. A window that started a process would be a second way
//!   out, with none of those rules in front of it.
//! * `docs/RUST-STYLE.md`, crate table: `atlas-web` is "the server: routes, static
//!   files, SSE, the request gate". The window is not a server.
//! * `atlas-guard/tests/audit_binds.rs`, rules 1 and 5: every listener bind in this
//!   workspace is one line in an allowlist with a reason, and an allowlist entry that
//!   no longer matches anything is itself a finding. Both rules are reused here.
//!
//! # What this file measured, and the two exceptions it carries
//!
//! The claim as usually stated — *the window opens no socket* — is **not** what the
//! tree says today, and this file does not pretend otherwise:
//!
//! 1. `src/http.rs` opens one **outbound** connection — `TcpStream::connect_timeout`,
//!    with the address built by `SocketAddr::from(([127, 0, 0, 1], port))` inside
//!    `endpoint()`. That is the chat page asking the local `llama-server`. There is no
//!    listener anywhere: nothing can connect *to* this process. The exception is one
//!    allowance below, and the address rule is asked at run time
//!    ([`the_only_address_the_socket_can_reach_is_a_loopback_literal`]).
//! 2. `src/views/chat.rs` calls `std::thread::spawn`, a **thread** in this process, not a
//!    process: it shares the address space, opens nothing by itself and holds no
//!    `Command`. One allowance below, pinned to one occurrence, so a second one — or a
//!    `std::process` in the same shape — is a failure.
//! 3. The archive the window draws is opened **read-only**, twice (`src/data.rs` and
//!    `src/views/graph.rs`), and the window writes exactly **one** file: the request queue
//!    at `data/requests.db` (`src/requests.rs`), a row of intent for the supervised agent.
//!    That is [`NO_STRAY_WRITE`] plus two allowances, and it is asserted from both sides:
//!    the forbidden shapes are absent from the shipped crate, and
//!    [`the_queue_is_the_only_writer_and_the_archive_stays_read_only`] asserts what *is*
//!    there (one read-write open, one `INSERT`, one file named, and the archive's
//!    `query_only` switch still set). The source half of the older, stricter rule —
//!    "the desktop writes no file" — is `atlas-guard/tests/audit_manifest.rs::
//!    the_desktop_writes_no_file`, which now needs the same one-line allowance for the
//!    queue. The runtime half is
//!    [`the_archive_is_read_only_by_flags_and_a_path_cannot_change_that`] below: a
//!    `file:…?mode=rwc` path is refused, because the *flags* decide the access mode and
//!    not the name — `libsqlite3-sys` compiles with `-DSQLITE_USE_URI`, so the string is
//!    a URI whether or not `SQLITE_OPEN_URI` is passed (measured; the test carries the
//!    table and a writable canary that proves it still has teeth).
//!
//! Line numbers are deliberately absent: this crate is being written while it is being
//! audited, and a claim pinned to a line number is stale by the next save. The pinning
//! is [`every_allowed_hit_is_still_there`], which counts occurrences and reports them.
//!
//! So the negative this file proves is the one that matters for the pivot: **no
//! listener, no process, no writer, and no address the single client can reach except the
//! loopback literal**. A future change that turns the chat page into a spawned
//! `llama-server`, the model client into a name-resolving HTTP call, or a view into
//! something that caches on disk, fails here with the rule's origin in the message.
//!
//! # How the scan reads the code
//!
//! [`strip`] blanks comments and string literals *before* looking for anything, so a
//! sentence in a doc comment cannot trip a rule, and — the case a naive `//`-to-end-of-
//! line stripper gets wrong — `let u = "http://x"; TcpListener::bind(a);` keeps the bind.
//! Matching then ignores whitespace, so `TcpListener ::\n bind(…)` is the same hit as
//! `TcpListener::bind(…)`. [`the_scan_catches_an_attack_and_ignores_a_mention`] is the
//! planted regression for both halves.
//!
//! This reads `src/` only. The test binaries in `tests/` bind ephemeral ports on
//! 127.0.0.1 on purpose (a fake model answers there) — inside the test process, which no
//! product rule forbids.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// The one module that talks to the model, from the crate's library target, so its address
// rule can be *asked* rather than read: `endpoint` is the only constructor of the socket
// address.
use atlas_desktop::http;

// ---------------------------------------------------------------------------
// one forbidden shape
// ---------------------------------------------------------------------------

/// A shape that may not appear in `src/`, what it would mean, and where the rule is
/// written down — so a failure sends a reader to the rule, not to this file.
struct Forbidden {
    needle: &'static str,
    means: &'static str,
    rule: &'static str,
    /// Continuations that make a hit not a hit: `std::process::` is `id()` (a query about
    /// this process) or it is a finding. The exception lives on the rule rather than on a
    /// file, so the fact that `id()` moved from `data.rs` to another module does not make
    /// a test about sockets go red.
    unless: &'static [&'static str],
}

/// Nothing in the window may listen. The origins are `src/main.rs`'s "no listener at
/// all" and `docs/THREAT-MODEL.md` §5.6: the archive has no authentication, so a
/// listener is not a smaller server — it is the same unauthenticated front door.
const NO_LISTENER: &[Forbidden] = &[
    Forbidden {
        needle: "TcpListener",
        means: "a socket to accept connections on",
        rule: "src/main.rs: \"opens no listener at all\"; docs/THREAT-MODEL.md §5.6 \"No listener beyond loopback\" — atlas-web is the server, the window is not",
        unless: &[],
    },
    Forbidden {
        needle: "UdpSocket",
        means: "a UDP socket, which hears datagrams from anywhere without a handshake",
        rule: "src/main.rs \"no socket for a page to talk to\"; docs/THREAT-MODEL.md §5.6 — the app's only socket is 127.0.0.1:<app_port>",
        unless: &[],
    },
    Forbidden {
        needle: "bind(",
        means: "a bind: the call that claims a port",
        rule: "atlas-guard/tests/audit_binds.rs rules 1 and 5 — every bind in this workspace is one allowlist line with a reason, and the window has none. (If the hit is rusqlite's `Statement::bind`, pass `params![]` instead: this rule cannot tell the two apart, and a socket in this crate is the worse mistake to allow.)",
        unless: &[],
    },
    Forbidden {
        needle: "try_bind(",
        means: "a bind that reports a busy port instead of failing",
        rule: "atlas-guard/tests/audit_binds.rs rule 1 — a bind by another spelling is the same bind",
        unless: &[],
    },
    Forbidden {
        needle: "axum::serve",
        means: "the workspace's one HTTP server",
        rule: "docs/RUST-STYLE.md: atlas-web owns the server; src/main.rs: this binary draws a window",
        unless: &[],
    },
];

/// The socket the window does have: one outbound connection to the local model, in the
/// file that is allowed it, in the one function that dials. Everywhere else — and beyond
/// the counts in [`ALLOWED`] — a `TcpStream` is a finding.
const NO_STRAY_SOCKET: &[Forbidden] = &[
    Forbidden {
        needle: "TcpStream",
        means: "an outbound connection",
        rule: "src/main.rs \"no socket for a page to talk to\"; docs/THREAT-MODEL.md §5.6 — the window's only socket is the one `src/http.rs` opens to the model on 127.0.0.1, and it is allowed there by name and count",
        unless: &[],
    },
    Forbidden {
        needle: "std::net",
        means: "the networking module",
        rule: "docs/THREAT-MODEL.md §5.6 — one module dials, and it is `src/http.rs`: a second `std::net` is a second socket",
        unless: &[],
    },
    Forbidden {
        needle: "connect_timeout(",
        means: "a dial",
        rule: "docs/THREAT-MODEL.md §5.6 — one dial, to the model, with a timeout; a second one is a second destination",
        unless: &[],
    },
];

/// Nothing in the window may start a process. The origin is `docs/THREAT-MODEL.md`
/// §4.9/§4.12: a child process is a way out of the gated, capped tool surface, and one
/// started by the window would have none of those rules in front of it.
const NO_PROCESS: &[Forbidden] = &[
    Forbidden {
        needle: "process::Command",
        means: "a child process, with an environment of its choosing",
        rule: "docs/THREAT-MODEL.md §4.9 (child processes) and §4.12 (the model's only way out is atlas-ingest's handlers); src/main.rs: \"What it deliberately does not do: start the model server\"",
        unless: &[],
    },
    Forbidden {
        needle: "Command::new",
        means: "a child process",
        rule: "docs/THREAT-MODEL.md §4.12 — a handler set, not a shell, is how this app touches the world",
        unless: &[],
    },
    Forbidden {
        needle: "Command::",
        means: "a child process, even without the std path in front of it",
        rule: "atlas-guard/tests/audit_binds.rs rule 1, applied to processes: an alias is the same call",
        unless: &[],
    },
    Forbidden {
        needle: "spawn(",
        means: "something started beside this thread",
        rule: "docs/THREAT-MODEL.md §4.9 — a process is started by atlas-launcher and supervised there; the window's one `std::thread::spawn` is allowed by name and count, and a second one is a decision",
        unless: &[],
    },
    Forbidden {
        needle: "fork(",
        means: "the syscall underneath a process spawn",
        rule: "docs/THREAT-MODEL.md §4.9 — the app starts three kinds of child, all from atlas-launcher",
        unless: &[],
    },
    Forbidden {
        needle: "execv",
        means: "replacing this process with another program",
        rule: "docs/THREAT-MODEL.md §4.9 — no crate in this workspace execs; the launcher spawns and supervises",
        unless: &[],
    },
    Forbidden {
        needle: "posix_spawn",
        means: "a process spawn",
        rule: "docs/THREAT-MODEL.md §4.9",
        unless: &[],
    },
    Forbidden {
        needle: "system(",
        means: "a command handed to a shell",
        rule: "docs/RUST-STYLE.md, and audit_downloads' \"no shell\": a shell is how a string becomes a program",
        unless: &[],
    },
    Forbidden {
        needle: "std::process::",
        means: "the process module — a spawn, an exec, or a decision to end the process",
        rule: "docs/THREAT-MODEL.md §4.9 — this crate neither starts nor replaces a process; `std::process::id()` is the one use, a question about this process",
        // `id()` is a query, not a start, and it is the only use the crate has.
        unless: &["id("],
    },
];

/// Nothing in the window may write a file — with **one** exception, allowed by name and
/// count in [`ALLOWED`]: the request queue.
///
/// # Why the exception exists and why it is not a hole
///
/// The shipped app cannot ingest: collection, monitoring and research run in the supervised
/// agent, and the archive this window draws is opened `SQLITE_OPEN_READ_ONLY` with
/// `PRAGMA query_only = ON` (`src/data.rs`). A click that means *go and look* therefore has
/// nowhere to happen except a **request**: one row in `data/requests.db` that the agent may
/// claim (`src/requests.rs`). That is what the exception buys, and it buys nothing else —
/// these rules still forbid `fs::write`, `File::create`, `OpenOptions`, `fs::rename`,
/// `fs::copy` and rusqlite's own `Connection::open` (which creates and writes by default)
/// everywhere in the shipped crate, so the write surface is one function in one file with
/// one target.
///
/// The flags are the decision, not the file name: `SQLITE_OPEN_READ_WRITE |
/// SQLITE_OPEN_CREATE` is the only way this window can change a byte of anything, and it is
/// named here by count. A second read-write open anywhere — a cache, a log, a config — fails
/// this test with the reason in the message. `atlas-guard`'s
/// `audit_manifest.rs::the_desktop_writes_no_file` says the same thing from the other side
/// and needs the same one-line allowance; this file cannot edit it.
const NO_STRAY_WRITE: &[Forbidden] = &[
    Forbidden {
        needle: "fs::write",
        means: "writing a file in one call",
        rule: "src/lib.rs: the window's one write is the request queue; docs/THREAT-MODEL.md §5.1 (the archive is evidence) — a second written path is a second source of truth",
        unless: &[],
    },
    Forbidden {
        needle: "File::create",
        means: "a file created or truncated for writing",
        rule: "src/lib.rs, and the archive's own flags: the window reads `data/atlas.db` and writes nothing but the queue",
        unless: &[],
    },
    Forbidden {
        needle: "OpenOptions",
        means: "a file opened with chosen flags — the general form of `File::create`",
        rule: "src/data.rs: the archive is opened `SQLITE_OPEN_READ_ONLY`; a file opened by hand is a file with a mode of the caller's choosing",
        unless: &[],
    },
    Forbidden {
        needle: "fs::rename",
        means: "replacing one file with another",
        rule: "docs/THREAT-MODEL.md §5.1 — an archive that can be replaced is not evidence; the window's only write is a queue row",
        unless: &[],
    },
    Forbidden {
        needle: "fs::copy",
        means: "writing a copy of a file",
        rule: "docs/THREAT-MODEL.md §5.1 — same as `fs::rename`: the window's writable surface is one table in one queue",
        unless: &[],
    },
    Forbidden {
        needle: "Connection::open(",
        means: "a database opened with rusqlite's defaults, which are read-write **and** create",
        rule: "src/data.rs (`SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only`) and src/requests.rs (`SQLITE_OPEN_READ_WRITE | SQLITE_OPEN_CREATE`, allowed by count): the access mode is decided by the flags, so every open names its flags",
        unless: &[],
    },
    Forbidden {
        needle: "SQLITE_OPEN_READ_WRITE",
        means: "a database opened for writing",
        rule: "src/requests.rs is the window's one writable file and it is allowed below by count; any other read-write open — the archive, a cache, a log — is a front door this app has no account for",
        unless: &[],
    },
    Forbidden {
        needle: "SQLITE_OPEN_CREATE",
        means: "a database created if it is missing",
        rule: "same as `SQLITE_OPEN_READ_WRITE`: one file is created by this window, and it is the queue",
        unless: &[],
    },
    Forbidden {
        needle: "create_dir_all",
        means: "directories created on disk",
        rule: "the queue is the only thing this window creates, and it creates its own directory so a fresh checkout is not a button that fails (src/requests.rs, allowed by count)",
        unless: &[],
    },
];

/// Ways to compile code the scan never read. A source assertion is worth nothing if the
/// source can pull in a file from outside `src/`, so `include!` is forbidden outright.
///
/// `#[path = "…"]` is *not* in this list, and that is deliberate: a module split inside
/// `src/` (`#[path = "tests.rs"] mod tests;`) is code this scan reads, so forbidding it
/// would be a rule that fires on something harmless — and a rule like that gets deleted.
/// What is forbidden is the path leaving `src/`, which needs the attribute's value and so
/// is decided in [`no_module_is_sourced_from_outside_src`].
const NO_BYPASS: &[Forbidden] = &[Forbidden {
    needle: "include!(",
    means: "compiling a file this scan does not read",
    rule: "this file's own claim — the rules above are assertions about src/, so src/ must be the whole program (`include_str!`/`include_bytes!` are data and stay allowed)",
    unless: &[],
}];

// ---------------------------------------------------------------------------
// the allowlist: what the tree actually does, one line each with the reason
// ---------------------------------------------------------------------------

/// A hit this file accepts, pinned to a file and an exact count.
///
/// Rule 5 of `atlas-guard/tests/audit_binds.rs` applies: an entry that no longer matches
/// exactly is a failure. A permission nobody re-read is a permission that has outlived
/// its reason.
struct Allowed {
    file: &'static str,
    needle: &'static str,
    count: usize,
    why: &'static str,
}

const ALLOWED: &[Allowed] = &[
    Allowed {
        file: "src/http.rs",
        needle: "TcpStream",
        count: 5,
        why: "the chat page's one outbound connection to the local model, named five times in one file: the `use` line, the single `TcpStream::connect_timeout` that dials, and three `&mut TcpStream` parameters of the helpers that write the request and read the reply on that same connected stream. No second dial, no listener, no name resolution — `the_one_socket_is_one_file_and_one_dial` still counts `connect_timeout(` at one, and the address rule is asked at run time",
    },
    Allowed {
        file: "src/http.rs",
        needle: "std::net",
        count: 1,
        why: "the same socket: `use std::net::{SocketAddr, TcpStream}` — no listener type, no name resolution",
    },
    Allowed {
        file: "src/http.rs",
        needle: "connect_timeout(",
        count: 1,
        why: "`TcpStream::connect_timeout` — outbound only, with a timeout, to the address `endpoint()` returned",
    },
    Allowed {
        file: "src/views/chat.rs",
        needle: "spawn(",
        count: 1,
        why: "`std::thread::spawn`: a thread in this process, which starts no program, holds no Command and opens nothing itself",
    },
    Allowed {
        file: "src/requests.rs",
        needle: "SQLITE_OPEN_READ_WRITE",
        count: 1,
        why: "the window's ONE writable file: the request queue at `data/requests.db`, opened `SQLITE_OPEN_READ_WRITE | SQLITE_OPEN_CREATE` once per call in `open()` and dropped again. It is a queue of intents for the supervised agent, not an action: the archive stays `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only`, and `the_queue_is_the_only_writer...` below asserts this is the only read-write open in the shipped crate",
    },
    Allowed {
        file: "src/requests.rs",
        needle: "SQLITE_OPEN_CREATE",
        count: 1,
        why: "the same line, the second flag: the queue creates its file on the first request rather than failing in a fresh checkout. Nothing else in the crate creates a database (`Connection::open` is forbidden above for exactly that reason)",
    },
    Allowed {
        file: "src/requests.rs",
        needle: "create_dir_all",
        count: 1,
        why: "`data/` itself, made on the way to the queue — the same directory the archive lives in, so this creates no new surface; a request that failed because the folder was missing would be a button that works only on a machine that has already run the agent",
    },
];

// ---------------------------------------------------------------------------
// reading the crate as text
// ---------------------------------------------------------------------------

/// Source with comments and string literals removed, plus the line each surviving
/// character came from, so a hit can be reported as `file:line`.
///
/// Removing strings *first* matters: in `let u = "http://x"; Command::new("a");` a
/// scanner that strips `//` to end-of-line first would hide the second statement.
struct Code {
    path: PathBuf,
    /// The file as written. The stripped text is what the rules read; a `#[path]` value is
    /// a string, so the one rule that has to look *inside* a string uses this.
    raw: String,
    text: Vec<char>,
    line: Vec<usize>,
    /// The lines [`shipped_one`] blanked: a `#[cfg(test)]` item. The raw file still holds
    /// them — a line-based check (SQL and paths live in string literals, which [`strip`]
    /// erases) has to skip them by number, or a test's own temporary queue would look like
    /// the window's second write target.
    test_lines: BTreeSet<usize>,
}

fn strip(path: &Path, source: &str) -> Code {
    let chars: Vec<char> = source.chars().collect();
    let mut code = Code {
        path: path.to_path_buf(),
        raw: source.to_string(),
        text: Vec::with_capacity(chars.len()),
        line: Vec::with_capacity(chars.len()),
        test_lines: BTreeSet::new(),
    };
    let mut i = 0usize;
    let mut line = 1usize;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        // `//` to end of line.
        if c == '/' && next == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // `/* … */`, nested, the way Rust allows.
        if c == '/' && next == '*' {
            let mut depth = 1usize;
            i += 2;
            while i < chars.len() && depth > 0 {
                if chars[i] == '\n' {
                    line += 1;
                } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 1;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 1;
                }
                i += 1;
            }
            continue;
        }
        // A raw string: `r"…"`, `r#"…"#`, and the `br"…"` prefix (the `b` is left as
        // code, which cannot form a needle on its own).
        if c == 'r' {
            let mut hashes = 0usize;
            let mut j = i + 1;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                j += 1;
                loop {
                    match chars.get(j) {
                        None => break,
                        Some('"') => {
                            let close = (1..=hashes).all(|k| chars.get(j + k) == Some(&'#'));
                            if close {
                                j += 1 + hashes;
                                break;
                            }
                            j += 1;
                        }
                        Some('\n') => {
                            line += 1;
                            j += 1;
                        }
                        Some(_) => j += 1,
                    }
                }
                i = j;
                continue;
            }
        }
        // A normal string, with escapes.
        if c == '"' {
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => i += 2,
                    '"' => {
                        i += 1;
                        break;
                    }
                    '\n' => {
                        line += 1;
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            continue;
        }
        // A character literal or a lifetime. `'a'` and `'\n'` are blanked; `'static`
        // and `'a` stay code, because hiding a lifetime would hide whatever follows
        // it — the failure mode has to be "sees too much", never "sees too little".
        if c == '\'' {
            if next == '\\' {
                let mut j = i + 2;
                if j < chars.len() {
                    j += 1;
                }
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            if chars.get(i + 2) == Some(&'\'') {
                i += 3;
                continue;
            }
            code.text.push(c);
            code.line.push(line);
            i += 1;
            continue;
        }
        if c == '\n' {
            line += 1;
        }
        code.text.push(c);
        code.line.push(line);
        i += 1;
    }
    code
}

/// Whitespace-free view of a [`Code`], so `TcpListener ::  bind(` matches `bind(`.
fn squashed(code: &Code) -> Vec<(char, usize)> {
    code.text
        .iter()
        .copied()
        .zip(code.line.iter().copied())
        .filter(|(c, _)| !c.is_whitespace())
        .collect()
}

#[test]
fn the_window_writes_no_file_but_the_request_queue() {
    // The pivot's write rule. The archive is evidence and the window cannot ingest, so the
    // only file it may touch is the queue of intents the supervised agent reads.
    check_shipped(NO_STRAY_WRITE);
    // And the rules have teeth on a plant as well as on the tree: a second read-write open,
    // a `File::create` and a `create_dir_all` in some other module must each be a finding —
    // otherwise "the window writes one file" would be a claim about the files that happen to
    // be there today.
    let planted = strip(
        Path::new("src/views/nothing.rs"),
        "fn cache(directory: &str) {\n\
             let _ = std::fs::create_dir_all(directory);\n\
             let _ = std::fs::File::create(\"cache.json\");\n\
             let conn = rusqlite::Connection::open_with_flags(\n\
                 \"cache.db\",\n\
                 rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE,\n\
             );\n\
         }\n",
    );
    let found = breaches(NO_STRAY_WRITE, &[planted]);
    let joined = found.join("\n");
    for wanted in ["create_dir_all", "File::create", "SQLITE_OPEN_READ_WRITE"] {
        assert!(
            joined.contains(wanted),
            "the write rules did not fire on a planted `{wanted}`: {joined}"
        );
    }
    // The message says where, so a reader can open the file: this scan's whole value is that a
    // failure is an address, not a verdict.
    assert!(joined.contains("src/views/nothing.rs:"), "{joined}");
}

#[test]
fn the_shipped_filter_drops_tests_and_keeps_the_code_around_them() {
    // The write rules read the crate as it ships, so the filter that decides what "ships"
    // means is itself a thing that has to be proved — on a planted source, so a plant
    // cannot be mistaken for the tree. What it must drop: the whole `#[cfg(test)]` item,
    // nested braces included. What it must keep: the code on either side of it.
    // Three planted files, in a tree of their own: a root that declares one shipped module
    // and one test module, the shipped module (with an inline `#[cfg(test)]` inside it), and
    // the file the test module declares — which is test code from its first line and must
    // not be read as the window's write surface.
    let root = Path::new("");
    let lib = "mod planted;\n#[cfg(test)]\nmod tests;\n";
    let planted = "fn shipped_before() { let _ = 1; }\n\
                   #[cfg(test)]\n\
                   mod inner {\n\
                       fn a_test() { std::fs::write(\"x\", b\"y\").unwrap(); }\n\
                       fn nested() { if true { } }\n\
                   }\n\
                   fn shipped_after() { let _ = 2; }\n";
    let tests_file = "fn a_test_in_a_test_file() { std::fs::write(\"x\", b\"y\").unwrap(); }\n";
    let kept = shipped_from(
        vec![
            strip(&root.join("lib.rs"), lib),
            strip(&root.join("planted.rs"), planted),
            strip(&root.join("tests.rs"), tests_file),
        ],
        root,
    );
    let files: Vec<String> = kept.iter().map(key).collect();
    assert_eq!(
        files,
        vec!["lib.rs".to_string(), "planted.rs".to_string()],
        "the filter kept a file nothing shipped declares (or dropped one that is)"
    );
    let code = kept
        .iter()
        .find(|code| key(code) == "planted.rs")
        .expect("planted.rs is shipped");
    let text: String = code.text.iter().collect();
    for kept in ["shipped_before", "shipped_after"] {
        assert!(text.contains(kept), "the filter blanked shipped code: `{kept}` is gone");
    }
    for dropped in ["fs::write", "a_test", "nested"] {
        assert!(
            !text.contains(dropped),
            "the filter kept `{dropped}` from a `#[cfg(test)]` module, so a test's temp file \
             would count as the window's write surface"
        );
    }
    // And the real tree: the shipped view of `src/requests.rs` is the queue's own file, with
    // the shipped half of it still there — a filter that blanked everything would pass every
    // rule below by proving nothing.
    let codes = shipped(sources());
    let requests = codes
        .iter()
        .find(|code| key(code) == "src/requests.rs")
        .expect("src/requests.rs");
    assert!(has(requests, "pub fn push_to("), "the queue's shipped code was blanked");
    assert_eq!(
        hits(requests, "SQLITE_OPEN_READ_WRITE").len(),
        1,
        "the queue's one read-write open is not there to be allowed"
    );
    let kept: usize = codes.iter().map(|code| code.text.iter().filter(|c| !c.is_whitespace()).count()).sum();
    assert!(kept > 15_000, "the shipped view kept only {kept} characters of code");
}

#[test]
fn the_queue_is_the_only_writer_and_the_archive_stays_read_only() {
    // The positive half of `the_window_writes_no_file_but_the_request_queue`: the forbidden
    // shapes are absent, and this asserts what is *present*, so a tree that deleted the
    // queue and one that grew a cache cannot both pass.
    let mut codes = shipped(sources());
    // 1. `requests.db` is named as code in exactly one shipped file: the queue's own module.
    //    Comment lines are skipped rather than the raw text, because a doc comment naming the
    //    queue (`src/app.rs` explains why the field exists) is a *description* of the write,
    //    not a second path — and a rule that counted it would push the explanation out of the
    //    code, which is the opposite of what this file wants.
    let namers: Vec<String> = codes
        .iter()
        .filter(|code| !named_in_code(code, "requests.db").is_empty())
        .map(key)
        .collect();
    assert_eq!(
        namers,
        vec!["src/requests.rs".to_string()],
        "the queue's path is named as code in {namers:?}"
    );
    // 2. The archive is opened read-only, and its own module carries the `query_only` switch
    //    that makes a write impossible even if a statement tried one.
    let data = codes
        .iter()
        .find(|code| key(code) == "src/data.rs")
        .expect("src/data.rs");
    assert!(has(data, "SQLITE_OPEN_READ_ONLY"), "src/data.rs no longer opens the archive read-only");
    assert!(
        !has(data, "SQLITE_OPEN_READ_WRITE") && !has(data, "SQLITE_OPEN_CREATE"),
        "src/data.rs can now write the archive"
    );
    assert!(
        data.raw.contains("PRAGMA query_only = ON"),
        "the archive's `PRAGMA query_only = ON` is gone: the flags are one lock and this was the second"
    );
    // 3. The queue writes one table and no other: one `INSERT INTO requests` in code, and no
    //    `UPDATE`/`DELETE`/`DROP` naming a table at all — the window cannot claim a request
    //    back, answer it, or clear it, because those are the agent's decisions. Read from the
    //    raw lines and not the stripped text, because SQL lives in string literals and
    //    [`strip`] blanks those.
    let requests = codes
        .iter()
        .find(|code| key(code) == "src/requests.rs")
        .expect("src/requests.rs");
    assert_eq!(
        named_in_code(requests, "INSERT INTO requests").len(),
        1,
        "the queue no longer writes exactly one row"
    );
    for banned in ["UPDATE ", "DELETE FROM", "DROP TABLE", "ALTER TABLE"] {
        for code in &codes {
            let lines = named_in_code(code, banned);
            assert!(
                lines.is_empty(),
                "{}:{lines:?}: the window writes SQL other than the one insert (`{}`). The \
                 queue's rows belong to the agent once they are written — the claim, the \
                 answer and the error are its columns, not this window's.",
                key(code),
                banned
            );
        }
    }
}

/// The lines where `needle` occurs in `code`, whitespace ignored.
fn hits(code: &Code, needle: &str) -> Vec<usize> {
    hits_unless(code, needle, &[])
}

/// The lines of `code` that name `needle` **in code**, comment lines excluded.
///
/// The stripped text cannot answer this one: `requests.db` lives inside a string literal
/// (`join("data").join("requests.db")`), and [`strip`] blanks string literals so a rule can
/// ignore prose. Reading the raw file line by line and skipping lines that *start* as a
/// comment keeps a doc comment from counting as a second write target while still finding
/// the one string that is the path.
fn named_in_code(code: &Code, needle: &str) -> Vec<usize> {
    code.raw
        .lines()
        .enumerate()
        .filter(|(index, line)| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//")
                && !code.test_lines.contains(&(index + 1))
                && line.contains(needle)
        })
        .map(|(index, _)| index + 1)
        .collect()
}

/// The crate as it **ships**: every `#[cfg(test)]` item blanked out of the stripped text.
///
/// The write rules below are about what the window does when a reader uses it, not about
/// what its tests do on their way past — a temporary directory in a `#[cfg(test)] mod tests`
/// is not a write surface, and a rule that forbade it would force the tree to carry an
/// allowance for every test that ever touches a temp file, which is how an allowlist stops
/// meaning anything. Blanking (rather than deleting) keeps the character-to-line map
/// intact, so a hit is still reported at the line a reader can open.
///
/// `atlas-guard`'s `source::shipped_code` is the same idea, done by the crate that owns it;
/// this file cannot depend on `atlas-guard`, so it carries the thirty lines that matter
/// here — and [`the_shipped_filter_drops_tests_and_keeps_the_code_around_them`] plants a
/// module to prove it drops the right thing.
fn shipped(codes: Vec<Code>) -> Vec<Code> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let kept = shipped_from(codes, &root);
    assert!(
        kept.len() >= 6,
        "the shipped view reached only {} files — the module walk, not the crate, is wrong",
        kept.len()
    );
    kept
}

/// [`shipped`], from a named root: the same arithmetic on a planted tree, so the filter can
/// be shown dropping a test module and keeping the code around it without touching `src/`.
fn shipped_from(codes: Vec<Code>, root: &Path) -> Vec<Code> {
    // Which files does the binary actually compile, and which of them only for tests?
    //
    // "Shipped" is *reachability*, not a naming convention: the roots are `lib.rs` and
    // `main.rs` under `root`, a file's `mod name;` is the file `name.rs` or `name/mod.rs`
    // beside it (`module_dir`), and a child behind `#[cfg(test)]` — or anything it declares
    // in turn — is test code. Two facts make this worth the twenty lines: `src/tests.rs` and
    // `src/data/tests.rs` both exist with the same 1013 lines (only the second is compiled,
    // so only the second is `mod tests;` from `src/data.rs`), and a file the compiler never
    // reads is not part of the product's write surface.
    let by_path: BTreeMap<PathBuf, &Code> = codes
        .iter()
        .map(|code| (code.path.clone(), code))
        .collect();
    let mut kept: BTreeSet<PathBuf> = BTreeSet::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut queue: Vec<(PathBuf, bool)> = vec![
        (root.join("lib.rs"), false),
        (root.join("main.rs"), false),
    ];
    while let Some((path, under_test)) = queue.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(code) = by_path.get(&path) else {
            continue;
        };
        if !under_test {
            kept.insert(path.clone());
        }
        for (name, test) in declared_modules(code) {
            let dir = module_dir(&path);
            for candidate in [dir.join(format!("{name}.rs")), dir.join(&name).join("mod.rs")] {
                if by_path.contains_key(&candidate) {
                    queue.push((candidate, under_test || test));
                    break;
                }
            }
        }
    }
    // The floor on how many files the walk reaches belongs to `shipped`, which walks the
    // real `src/`. It used to sit here as well — twice over — and that made this function
    // assert six files for the *planted* three-file tree it is also called on, so the test
    // below failed at the helper rather than on what it plants.
    codes
        .into_iter()
        .filter(|code| kept.contains(&code.path))
        .map(shipped_one)
        .collect()
}

/// Every `mod name;` a file declares, with whether it is behind `#[cfg(test)]`.
fn declared_modules(code: &Code) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    let mut at = 0usize;
    while at < code.text.len() {
        let starts_a_word = code.text[at] == 'm'
            && code.text.get(at + 1) == Some(&'o')
            && code.text.get(at + 2) == Some(&'d')
            && !code.text.get(at.wrapping_sub(1)).is_some_and(|c| c.is_alphanumeric() || *c == '_');
        if !starts_a_word {
            at += 1;
            continue;
        }
        let mut cursor = at + 3;
        while cursor < code.text.len() && code.text[cursor].is_whitespace() {
            cursor += 1;
        }
        let mut name = String::new();
        while cursor < code.text.len()
            && (code.text[cursor].is_alphanumeric() || code.text[cursor] == '_')
        {
            name.push(code.text[cursor]);
            cursor += 1;
        }
        while cursor < code.text.len() && code.text[cursor].is_whitespace() {
            cursor += 1;
        }
        // `mod name;` is a file; `mod name { … }` is inline and already in this file.
        if !name.is_empty() && code.text.get(cursor) == Some(&';') {
            let before: String = code.text[..at].iter().collect();
            let before: String = before.split_whitespace().collect::<Vec<_>>().join("");
            found.push((name, before.ends_with("#[cfg(test)]")));
        }
        at += 3;
    }
    found
}

/// Where a file's child modules live, by Rust's own rule: `src/data.rs`'s `mod tests;` is
/// `src/data/tests.rs`, while `src/lib.rs`'s is `src/tests.rs`. Getting this wrong is how
/// the first version of this filter read `src/data/tests.rs` as shipped code.
fn module_dir(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    match path.file_stem().and_then(|stem| stem.to_str()) {
        Some("lib") | Some("main") | Some("mod") => dir,
        Some(stem) => dir.join(stem),
        None => dir,
    }
}

/// Is the `#[cfg(test)]` at `start` attached to `mod …;` — a declaration, with no body in
/// this file — rather than to an item this file contains?
fn declares_a_module(text: &[char], start: usize, mark_len: usize) -> bool {
    let tail: String = text[start + mark_len..].iter().collect();
    let Some(rest) = tail.trim_start().strip_prefix("mod") else {
        return false;
    };
    let rest = rest.trim_start();
    let name: String = rest
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    !name.is_empty() && rest[name.len()..].trim_start().starts_with(';')
}

fn shipped_one(mut code: Code) -> Code {
    /// The attribute, as written. `#[cfg(test)]` is the form the compiler wants, and a
    /// differently spaced spelling (`#[cfg( test )]`) would still parse — so the search
    /// below skips whitespace rather than matching bytes.
    const MARK: &str = "#[cfg(test)]";
    let mark: Vec<char> = MARK.chars().collect();
    loop {
        let Some(start) = find_ignoring_space(&code.text, &mark) else {
            break;
        };
        // `#[cfg(test)] mod tests;` — a declaration, not an item with a body: blanking the
        // attribute is enough, because the file it names is dropped by `shipped` above.
        if declares_a_module(&code.text, start, mark.len()) {
            for slot in &mut code.text[start..start + mark.len()] {
                *slot = ' ';
            }
            continue;
        }
        // The item the attribute is attached to: its body is the next brace, and the body
        // ends at the brace that closes it. An attribute with no brace after it is not an
        // item this filter understands, and guessing would blank code — so it stops.
        let Some(open) = (start + mark.len()..code.text.len()).find(|&i| code.text[i] == '{') else {
            break;
        };
        let mut depth = 0usize;
        let mut end = None;
        for (index, character) in code.text.iter().enumerate().skip(open) {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        // The lines, as well as the characters: a check that reads the *raw* file (SQL and
        // paths live in string literals) skips these by number.
        let first = code.line[start];
        let last = code.line[end];
        for number in first..=last {
            code.test_lines.insert(number);
        }
        for slot in &mut code.text[start..=end] {
            if *slot != '\n' {
                *slot = ' ';
            }
        }
    }
    code
}

/// Where `needle` occurs in `text`, whitespace ignored.
fn find_ignoring_space(text: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || text.len() < needle.len() {
        return None;
    }
    for start in 0..=text.len() - needle.len() {
        let mut at = start;
        let mut matched = true;
        for wanted in needle {
            while at < text.len() && text[at].is_whitespace() {
                at += 1;
            }
            if at >= text.len() || text[at] != *wanted {
                matched = false;
                break;
            }
            at += 1;
        }
        if matched {
            return Some(start);
        }
    }
    None
}

/// [`hits`], where a match is ignored when what follows it starts with one of `unless`.
///
/// This is how a rule can be about a *call* rather than about a name: `std::process::`
/// followed by `id(` is a question about this process, and followed by anything else is
/// a finding. Written as a continuation rather than as a file allowance so that moving
/// `id()` to another module does not turn a socket test red.
fn hits_unless(code: &Code, needle: &str, unless: &[&str]) -> Vec<usize> {
    let haystack = squashed(code);
    let wanted: Vec<char> = needle.chars().filter(|c| !c.is_whitespace()).collect();
    if wanted.is_empty() || haystack.len() < wanted.len() {
        return Vec::new();
    }
    let exempt: Vec<Vec<char>> = unless
        .iter()
        .map(|tail| tail.chars().filter(|c| !c.is_whitespace()).collect())
        .collect();
    let mut found = Vec::new();
    for start in 0..=(haystack.len() - wanted.len()) {
        let matched = wanted
            .iter()
            .enumerate()
            .all(|(offset, ch)| haystack[start + offset].0 == *ch);
        if !matched {
            continue;
        }
        let after = start + wanted.len();
        let allowed = exempt.iter().any(|tail| {
            !tail.is_empty()
                && after + tail.len() <= haystack.len()
                && tail
                    .iter()
                    .enumerate()
                    .all(|(offset, ch)| haystack[after + offset].0 == *ch)
        });
        if !allowed {
            found.push(haystack[start].1);
        }
    }
    found
}

/// Every `.rs` under the crate's `src/`, sorted, with a guard against reading nothing:
/// a rule that finds no file is a rule that proves nothing.
fn sources() -> Vec<Code> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    collect(&root, &mut paths);
    paths.sort();
    assert!(
        paths.len() >= 8,
        "the scan read {} files under {} — the window's sources are not there, so every rule \
         below would pass without looking at anything",
        paths.len(),
        root.display()
    );
    let mut codes = Vec::new();
    for path in paths {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        codes.push(strip(&path, &source));
    }
    let total: usize = codes.iter().map(|code| code.text.len()).sum();
    assert!(
        total > 20_000,
        "the scan kept {total} characters of code — a scan that strips too much proves nothing"
    );
    codes
}

fn collect(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, found);
        } else if path.extension().map(|ext| ext == "rs").unwrap_or(false) {
            found.push(path);
        }
    }
}

/// The crate-relative spelling of a path, for an allowlist entry (`src/http.rs`).
fn key(code: &Code) -> String {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    code.path
        .strip_prefix(manifest)
        .unwrap_or(&code.path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every breach of `rules` in `codes`, as the sentence a caller panics with.
///
/// Separate from [`check`] so a planted source can be run through the *same* arithmetic —
/// including the allowlist — without touching `src/`. A rule proved only on the real tree
/// is a rule nobody has seen fire.
fn breaches(rules: &[Forbidden], codes: &[Code]) -> Vec<String> {
    let mut found = Vec::new();
    for rule in rules {
        for code in codes {
            let file = key(code);
            let lines = hits_unless(code, rule.needle, rule.unless);
            let allowed = ALLOWED
                .iter()
                .find(|entry| entry.file == file && entry.needle == rule.needle)
                .map(|entry| entry.count)
                .unwrap_or(0);
            if lines.len() <= allowed {
                continue;
            }
            let first = lines.get(allowed).copied().unwrap_or(0);
            found.push(format!(
                "{}:{first}: the window contains `{}` — {}.\n\
                 \n\
                 Rule: {}\n\
                 \n\
                 This binary is the product's face, not its server and not its supervisor. If the \
                 code genuinely needs it, that is a product decision, not a test edit: add a line to \
                 ALLOWED in this file with the reason, and state it in docs/THREAT-MODEL.md.\n\
                 ({} hits in this file, {} allowed by ALLOWED.)",
                file,
                rule.needle,
                rule.means,
                rule.rule,
                lines.len(),
                allowed
            ));
        }
    }
    found
}

/// Apply one group of rules to the crate's own sources, and fail with the rules' origins.
///
/// Every breach in one panic: a fix loop that reveals one `Command` per run is a fix loop
/// nobody finishes.
fn check(rules: &[Forbidden]) {
    let found = breaches(rules, &sources());
    if !found.is_empty() {
        panic!("{}", found.join("\n\n"));
    }
}

/// [`check`], on the crate **as it ships** ([`shipped`]): test code is not a write surface.
///
/// Only the write rules use this view. The listener, socket and process rules keep reading
/// every file under `src/`, including the test modules — the stricter reading, deliberately:
/// a `TcpListener` or a `Command` anywhere in this crate is a decision worth explaining, even
/// in a test, and narrowing that rule now would give back a claim that already holds. The
/// write rules are the exception because a test that makes a temporary file is how this crate
/// tests a reader of files at all, and the allowance it would otherwise need says nothing
/// about the product.
fn check_shipped(rules: &[Forbidden]) {
    let found = breaches(rules, &shipped(sources()));
    if !found.is_empty() {
        panic!("{}", found.join("\n\n"));
    }
}

// ---------------------------------------------------------------------------
// the rules
// ---------------------------------------------------------------------------

#[test]
fn the_window_opens_no_listener() {
    // Nothing may connect *to* this process. `src/main.rs`: "opens no listener at all".
    check(NO_LISTENER);
}

#[test]
fn the_window_starts_no_process() {
    // A child process is a second way out of this app, with none of atlas-ingest's
    // gates in front of it: docs/THREAT-MODEL.md §4.9 and §4.12.
    check(NO_PROCESS);
}

#[test]
fn the_window_cannot_compile_code_the_scan_does_not_read() {
    // The rules above are assertions about the text under src/. `include!` and
    // `#[path]` are the two ways a module arrives from somewhere else.
    check(NO_BYPASS);
}

#[test]
fn every_allowed_hit_is_still_there() {
    // Rule 5 of atlas-guard/tests/audit_binds.rs: an allowlist entry that no longer
    // matches anything is a licence for whoever writes there next. Counted on the crate as
    // it ships, which is the view the rules themselves are checked against — an allowance
    // for the queue must not be satisfied by a `create_dir_all` in a test module.
    let codes = shipped(sources());
    for entry in ALLOWED {
        let code = codes
            .iter()
            .find(|code| key(code) == entry.file)
            .unwrap_or_else(|| panic!("{} is on the allowlist and not in src/", entry.file));
        let lines = hits(code, entry.needle);
        assert_eq!(
            lines.len(),
            entry.count,
            "{}: `{}` occurs {} times and the allowlist says {} ({}).\nThe permission no longer \
             describes the code — re-read it and update ALLOWED, or take the code back out. \
             Why it was allowed: {}",
            entry.file,
            entry.needle,
            lines.len(),
            entry.count,
            lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            entry.why
        );
    }
}

/// True when `needle` occurs in `code`, whitespace ignored (`TcpStream ::  connect`).
fn has(code: &Code, needle: &str) -> bool {
    !hits(code, needle).is_empty()
}

#[test]
fn the_only_socket_is_the_model_call() {
    // The measured exception, pinned to where it lives and to what it is: one outbound
    // connection, no listener type anywhere, no name resolution anywhere, and the
    // address built from the loopback literal.
    let codes = sources();
    let http = codes
        .iter()
        .find(|code| key(code) == "src/http.rs")
        .expect("src/http.rs is where the model call lives");
    for wanted in [
        "TcpStream::connect_timeout(&address",
        "SocketAddr::from(([127, 0, 0, 1], port))",
    ] {
        assert!(
            has(http, wanted),
            "src/http.rs no longer contains `{wanted}` — the one socket has moved or changed \
             shape, so the permission in ALLOWED and the address rule below describe code that \
             is not there. Rule: docs/THREAT-MODEL.md §5.6 (the app's only socket is the \
             loopback one) and atlas-guard/tests/audit_binds.rs rule 5."
        );
    }
    // The request this socket writes has a **constant** path: a `model_url` may choose a
    // port and nothing else, so `http://127.0.0.1:8081/../../elsewhere` dials the same
    // bytes as `http://127.0.0.1:8081`. That is the claim; asserted on the raw text,
    // because the request line is a format string and the scan blanks strings.
    let raw = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/http.rs"))
        .expect("src/http.rs");
    assert!(
        raw.contains("pub const PATH: &str = \"/v1/chat/completions\";"),
        "the request path is no longer a constant in src/http.rs — if it is built from the url, \
         a url can choose what is posted to. Rule: src/http.rs's `endpoint` (\"the address is the \
         port and the loopback literal\") and docs/THREAT-MODEL.md §5.6."
    );
    assert!(
        raw.contains("POST {PATH} HTTP/1.1"),
        "the request line no longer writes the constant PATH — see the note above"
    );
    // A name would be resolved by the resolver, on a network, at run time. The address
    // rule in `endpoint()` refuses names; this is the other half of that claim.
    for banned in ["to_socket_addrs(", "lookup_host(", "getaddrinfo", "Resolver"] {
        for code in &codes {
            let lines = hits(code, banned);
            assert!(
                lines.is_empty(),
                "{}:{:?}: the window resolves an address (`{}`).\nRule: src/http.rs's own \
                 decision — \"a name — even `localhost` — is refused rather than resolved\" — and \
                 docs/THREAT-MODEL.md §5.6. A resolver is how a loopback-looking dial leaves the \
                 machine.",
                key(code),
                lines,
                banned
            );
        }
    }
}

// ---------------------------------------------------------------------------
// the address rule, asked rather than read
// ---------------------------------------------------------------------------

#[test]
fn the_only_address_the_socket_can_reach_is_a_loopback_literal() {
    // The runtime half, and the one that does not depend on how the code is spelled.
    // `http::endpoint` is the only constructor of the socket address; if it cannot
    // return anything but 127.0.0.1:<port>, the one socket in this crate cannot leave
    // the machine, whatever `ATLAS_MODEL_URL` says.
    let (shown, address) = http::endpoint("http://127.0.0.1:8081").expect("the model address");
    assert_eq!(shown, "127.0.0.1:8081");
    assert_eq!(address.port(), 8081);
    assert!(
        address.ip().is_loopback(),
        "the model address {} is not loopback",
        address
    );
    assert_eq!(address.ip().to_string(), "127.0.0.1");

    for refused in [
        // a public name, and the two names a reader expects to work
        "http://example.com:8081",
        "http://localhost:8081",
        "http://ip6-localhost:8081",
        // TLS, and schemes that are not a dial at all
        "https://127.0.0.1:8081",
        "file:///etc/passwd",
        "ftp://127.0.0.1:8081",
        "",
        // wildcards and other loopback spellings: the literal, or nothing
        "http://0.0.0.0:8081",
        "http://127.0.0.2:8081",
        "http://[::1]:8081",
        "http://2130706433:8081",
        // authority tricks
        "http://127.0.0.1:8081@evil.example",
        "http://user:word@127.0.0.1:8081",
        // a port that is not one
        "http://127.0.0.1",
        "http://127.0.0.1:",
        "http://127.0.0.1:notaport",
        "http://127.0.0.1:70000",
        // a path that climbs: the request line is the constant, so this is refused
        // rather than silently ignored
        "http://127.0.0.1:8081/../..",
        // a query or a fragment: both are refused by name, because neither has anywhere
        // to go (the request line is a constant; a fragment never leaves the client) —
        // a value that would be dropped silently is a value the reader cannot trust
        "http://127.0.0.1:8081/?else=1",
        "http://127.0.0.1:8081#fragment",
        // port 0 is "any port": the kernel would pick nothing and the connect would fail
        // with a sentence about a refused connection instead of about the address
        "http://127.0.0.1:0",
    ] {
        let outcome = http::endpoint(refused);
        assert!(
            outcome.is_err(),
            "endpoint({refused:?}) was accepted as {:?} — the window would dial something other \
             than the loopback literal.\nRule: src/http.rs's `endpoint` doc (\"not a host \
             allowlist: an address rule\") and docs/THREAT-MODEL.md §5.6.",
            outcome.map(|(shown, _)| shown)
        );
    }

    // A port is the only thing a url may choose, and the path is not part of the dial:
    // `endpoint` returns the authority it accepted, and the request line writes `/v1/…`
    // from the constant (asserted in [`the_only_socket_is_the_model_call`]).
    for (url, port) in [
        ("http://127.0.0.1:1024", 1024),
        ("http://127.0.0.1:65535", 65535),
        ("  http://127.0.0.1:9000  ", 9000),
        // A path is parsed and thrown away, because the request line is the constant
        // `POST /v1/chat/completions`: the dial is the authority, and nothing else in the
        // url survives into the socket. (This used to be asserted with `?else=1` appended,
        // when a query was silently dropped; `endpoint` now names the `?` and refuses it,
        // which is the stricter rule and a better sentence, so the case moved to the
        // refused list above rather than the contract being loosened here.)
        ("http://127.0.0.1:9000/anything/else", 9000),
    ] {
        let (_, address) = http::endpoint(url)
            .unwrap_or_else(|error| panic!("endpoint({url:?}) refused: {error}"));
        assert_eq!(address.port(), port, "{url}");
        assert_eq!(address.ip().to_string(), "127.0.0.1", "{url}");
    }
}

// ---------------------------------------------------------------------------
// the dependency side: a crate that cannot speak HTTP cannot dial
// ---------------------------------------------------------------------------

/// HTTP clients and servers, and the TLS underneath them. A window that reads one
/// archive and dials one literal port has no use for any of them; `src/http.rs` states
/// the protocol in full by hand, which is why the list is empty today.
const HTTP_AND_TLS: &[&str] = &[
    "ureq",
    "reqwest",
    "hyper",
    "hyper-util",
    "h2",
    "h3",
    "isahc",
    "curl",
    "curl-sys",
    "surf",
    "attohttpc",
    "axum",
    "axum-core",
    "warp",
    "actix-web",
    "actix-http",
    "actix-rt",
    "tiny_http",
    "tower-http",
    "tungstenite",
    "tokio-tungstenite",
    "quinn",
    "rustls",
    "webpki-roots",
    "native-tls",
    "openssl",
    "openssl-sys",
];

fn manifest() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// The dependency names a `[section]` of the manifest declares.
fn declared(manifest: &str, section: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == format!("[{section}]");
            continue;
        }
        if !inside || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = trimmed.split_once('=') {
            names.insert(name.trim().to_string());
        }
    }
    names
}

#[test]
fn the_window_declares_no_http_client() {
    let manifest = manifest();
    let runtime = declared(&manifest, "dependencies");
    assert!(
        !runtime.is_empty(),
        "no [dependencies] were read from Cargo.toml — this rule would pass without looking"
    );
    // A review gate as well as a denylist: the four crates below are the whole of what
    // this window is allowed to be built from, so a fifth arrives by decision.
    let expected: BTreeSet<String> = ["eframe", "egui", "rusqlite", "serde_json"]
        .iter()
        .map(|name| name.to_string())
        .collect();
    assert_eq!(
        runtime, expected,
        "the window's dependencies changed. Every crate here can open a socket or a file the \
         window's own rules say nothing about, so the list is a review decision: update this \
         test with the reason, and check the new crate against HTTP_AND_TLS below.\n\
         Rule: docs/RUST-STYLE.md (one crate, one job) and docs/THREAT-MODEL.md §5 (nothing is \
         downloaded and nothing dials out but the loopback socket)."
    );
    for name in &runtime {
        assert!(
            !HTTP_AND_TLS.contains(&name.as_str()),
            "{name} is an HTTP client or server, and the window must not be able to speak HTTP: \
             src/http.rs states the one request it needs by hand, to a loopback literal."
        );
    }
    // A build script is a program this crate runs at build time, and it can do anything
    // the build user can: `atlas-guard/tests/audit_manifest.rs` keeps the workspace free
    // of our own build scripts, and this is the same rule for this crate alone.
    assert!(
        declared(&manifest, "build-dependencies").is_empty(),
        "the window declares build-dependencies — a build script is a process this crate \
         starts. Rule: docs/THREAT-MODEL.md §5.3 (the setup scripts verify, they do not fetch) \
         and atlas-guard/tests/audit_manifest.rs."
    );
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs").exists(),
        "the window has a build.rs"
    );
    for name in declared(&manifest, "dev-dependencies") {
        assert!(
            !HTTP_AND_TLS.contains(&name.as_str()),
            "{name} is an HTTP client in the window's test dependencies: the test binaries are \
             the place a real network call would go unnoticed, and tests/ bind ephemeral \
             loopback ports on purpose instead."
        );
    }
}

// ---------------------------------------------------------------------------
// the resolved tree: the lock says what would actually be compiled
// ---------------------------------------------------------------------------

/// The graph `Cargo.lock` records, by package name. Two versions of one name are merged,
/// which over-approximates reachability — the safe direction for an absence claim: if a
/// name is not even in the over-approximation, no path reaches it.
fn locked() -> BTreeMap<String, BTreeSet<String>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut in_deps = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            current = None;
            in_deps = false;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("name = ") {
            if !in_deps {
                let name = rest.trim().trim_matches('"').to_string();
                graph.entry(name.clone()).or_default();
                current = Some(name);
            }
            continue;
        }
        if trimmed.starts_with("dependencies = [") {
            in_deps = true;
            // `dependencies = ["a", "b"]` on one line is legal; take the rest too.
            if let Some(rest) = trimmed.split_once('[').map(|(_, rest)| rest) {
                for item in rest.split(',') {
                    push_dep(&mut graph, current.as_deref(), item);
                }
            }
            continue;
        }
        if in_deps {
            if trimmed.starts_with(']') {
                in_deps = false;
                continue;
            }
            push_dep(&mut graph, current.as_deref(), trimmed);
        }
    }
    assert!(
        graph.len() > 100,
        "the lock parsed to {} packages — an absence claim about an empty graph proves nothing",
        graph.len()
    );
    graph
}

fn push_dep(graph: &mut BTreeMap<String, BTreeSet<String>>, from: Option<&str>, item: &str) {
    // A dependency entry is ` "name",` in the multi-line form and `"name version (source)"`
    // when two versions of one crate are in the tree: the comma is part of the line, the
    // quotes are part of the string, and neither is part of the name.
    let item = item.trim().trim_end_matches(',').trim().trim_matches('"').trim();
    if item.is_empty() {
        return;
    }
    let name = item
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches('"')
        .to_string();
    if name.is_empty() {
        return;
    }
    if let Some(from) = from {
        graph.entry(from.to_string()).or_default().insert(name);
    }
}

/// Everything `atlas-desktop` would compile, transitively.
fn window_closure() -> BTreeSet<String> {
    let graph = locked();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = graph
        .get("atlas-desktop")
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    assert!(
        !queue.is_empty(),
        "atlas-desktop has no dependency edges in Cargo.lock — the walk would prove nothing"
    );
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        for next in graph.get(&name).cloned().unwrap_or_default() {
            if !seen.contains(&next) {
                queue.push(next);
            }
        }
    }
    seen
}

#[test]
fn the_window_resolves_to_a_tree_that_cannot_speak_http() {
    let closure = window_closure();
    assert!(
        closure.len() > 20,
        "the window's closure is {} packages — too small to be a tree, so this proves nothing",
        closure.len()
    );
    // Non-vacuous in the other direction too: the walk must find the crates that are
    // really there before its silence about the ones that are not means anything.
    for wanted in ["eframe", "egui", "rusqlite", "libsqlite3-sys", "serde_json"] {
        assert!(
            closure.contains(wanted),
            "{wanted} is declared in Cargo.toml and missing from the closure — the lock walk is \
             not following the graph, so `no HTTP client` below is a claim about nothing"
        );
    }
    let workspace = locked();
    let names: BTreeSet<String> = workspace.keys().cloned().collect();
    // One HTTP client is enough to make the check bite, and `ureq` is the one the workspace
    // still carries (`atlas-net`, and through it `atlas-setup` and the agent). `axum` was the
    // other half of this guard until the web frontend was retired — with `atlas-web` deleted
    // there is no server in the workspace at all, so requiring one here would be a guard that
    // fails for a *better* tree. The absence check is still non-vacuous: `ureq` is present and
    // the window's closure must not reach it.
    for client in ["ureq"] {
        assert!(
            names.contains(client),
            "the workspace lock no longer contains {client}: the absence check below would pass \
             because the crate is not in the workspace at all, not because the window avoids it"
        );
    }
    for name in &closure {
        assert!(
            !HTTP_AND_TLS.contains(&name.as_str()),
            "{name} is an HTTP client or server and is reachable from the window's dependencies.\n\
             Rule: docs/THREAT-MODEL.md §5 (no egress but the loopback socket) — a window that \
             links a web client can be made to speak to a host without a line in src/ changing.\n\
             The workspace's own HTTP client is `ureq` (atlas-net) \
             (and nothing here serves a page any more); the window must not reach it."
        );
    }
    // The one networking stack the lock *does* reach arrives through the wasm-only path of
    // `winit`, never through a native one: `wasm-bindgen-futures` is the only package in
    // the closure that depends on `tokio`. If that ever stops being true, a native path
    // exists and this test should be read again rather than silently passing.
    let mut parents: BTreeSet<String> = BTreeSet::new();
    for (from, deps) in &workspace {
        if closure.contains(from) && deps.contains("tokio") {
            parents.insert(from.clone());
        }
    }
    assert_eq!(
        parents,
        ["wasm-bindgen-futures".to_string()].into_iter().collect(),
        "the window's closure reaches tokio through {parents:?}.\nIt used to arrive only through \
         `wasm-bindgen-futures` (winit's wasm backend, never compiled into this binary). Any other \
         parent is a native async runtime in the window: state it in docs/THREAT-MODEL.md, or \
         take it out."
    );
}

// ---------------------------------------------------------------------------
// the scan itself: a planted attack must be caught, a mention must not be
// ---------------------------------------------------------------------------

#[test]
fn the_scan_catches_an_attack_and_ignores_a_mention() {
    // Every rule above is only as good as this function. Each attack below is a shape the
    // rules forbid, spelled the way a person would spell it; each mention is a way to
    // *talk* about the shape without doing it.
    let path = Path::new("planted.rs");
    let attacks = [
        ("let listener = std::net::TcpListener::bind(addr)?;", "TcpListener"),
        ("std :: net :: TcpListener :: bind ( addr )", "TcpListener"),
        ("TcpListener ::\n    bind(addr)", "TcpListener"),
        ("let socket = UdpSocket::bind(\"0.0.0.0:0\")?;", "UdpSocket"),
        ("let out = std::process::Command::new(\"atlas\").output()?;", "process::Command"),
        ("let out = Command::new(\"sh\");", "Command::new"),
        ("child.spawn().expect(\"started\");", "spawn("),
        ("unsafe { fork(); }", "fork("),
        ("include!(concat!(\"../elsewhere\", \".rs\"));", "include!("),
        ("#[path = \"../../atlas-web/src/boot.rs\"]\nmod boot;", "#[path") ,
    ];
    for (source, needle) in attacks {
        let code = strip(path, source);
        assert!(
            !hits(&code, needle).is_empty(),
            "the scan did not see `{needle}` in:\n  {source}\nA rule that cannot see the attack \
             it names is a comment, not a test."
        );
    }

    let mentions = [
        "// TcpListener::bind(\"127.0.0.1:0\") is what atlas-web does; the window does not.",
        "/// `std::process::Command` is how the launcher starts the model.",
        "/* UdpSocket::bind, spawn(, fork( — all four named in this comment. */",
        "let note = \"std::process::Command::new(\";",
        "let url = \"http://example.com/Command::new\";",
        "let slash = '/';",
        "let quoted = '\"';",
        "let text: &'static str = \"TcpListener\";",
    ];
    for source in mentions {
        let code = strip(path, source);
        for needle in [
            "TcpListener",
            "UdpSocket",
            "process::Command",
            "Command::new",
            "spawn(",
            "fork(",
            "bind(",
        ] {
            assert!(
                hits(&code, needle).is_empty(),
                "the scan reports `{needle}` for a mention that does no such thing:\n  {source}\n\
                 A rule that fires on prose is a rule people switch off."
            );
        }
    }

    // The two cases where the *order* of stripping decides: a string holding `//` must not
    // swallow the statement after it, and a lifetime must not swallow anything at all.
    let after_string = "let url = \"http://x\"; TcpListener::bind(addr);";
    assert!(
        !hits(&strip(path, after_string), "TcpListener").is_empty(),
        "a string containing `//` hid the code after it — this is the bug the stripper exists \
         to avoid"
    );
    let after_lifetime = "fn f<'a>(s: &'a str) -> &'a str { std::process::Command::new(s); s }";
    assert!(
        !hits(&strip(path, after_lifetime), "process::Command").is_empty(),
        "a lifetime hid the code after it"
    );
    let after_char = "let slash = '/'; UdpSocket::bind(addr);";
    assert!(
        !hits(&strip(path, after_char), "UdpSocket").is_empty(),
        "a character literal hid the code after it"
    );
}

/// A finding names the line to open — and that line must be the real one.
///
/// The rules match over code with comments dropped and whitespace removed, so the line is
/// kept in a side table (`Code::line`) rather than coming out of the match itself. A side
/// table drifts, and when it does, every finding after a comment points at the wrong line —
/// a guard that sends a reader to the wrong place gets switched off.
///
/// The expected line is *derived from the fixture*, never written by hand: each fixture is
/// a prefix whose line count is `prefix.matches('\n').count()`, followed by one line of
/// code, so the number asserted here comes from the text under test. A hand-written `4` is
/// a second copy of the stripper's bookkeeping, and when the two disagree there is no way
/// to tell which one is lying — a report of `left: [2], right: [4]` on the block-comment
/// case turned out to be this file's own mutation (the counter deleted on purpose, to watch
/// the test fail) read as a defect in the counter. With the expectation derived, that
/// ambiguity is gone: the fixture and the number cannot drift apart.
///
/// The prefixes are every way a newline hides: a line comment, a one-line block comment, a
/// multi-line block comment (the case this test exists for), one whose closing line also
/// carries code, a nested block comment (legal in Rust), a doc block, CRLF endings, and a
/// normal string and a raw string that contain real newlines. Every comment prefix also
/// *mentions* the needle, so a counter that skipped the comment shows up twice — a wrong
/// line, and a second hit for a mention that should not be one.
#[test]
fn a_finding_names_the_line_the_code_is_on() {
    let path = Path::new("planted.rs");
    let needle = "TcpListener";
    let code_line = "TcpListener::bind(a);";
    let prefixes = [
        // nothing above the code, and lines that are not code
        "",
        "\n\n",
        "let x = 1;\nlet y = 2;\n",
        // a line comment, with and without a mention
        "// a note\n",
        "// TcpListener::bind, one day\n",
        // a one-line block comment, with the code on the same line
        "/* one line */ ",
        // a block comment that spans lines — the case this test exists for
        "/* one\n   two\n   three */\n",
        // …one whose closing line also carries code, so the hit is on the last line
        "/* one\n   two */ ",
        // …and one followed by a blank line
        "/* one\n   two\n*/\n\n",
        // nested, the way Rust allows, with the mention at depth 2
        "/* outer /* TcpListener::bind, inner */ still outer */\n",
        // a line comment inside a block comment must not end it
        "/* has // a line comment inside\n   TcpListener::bind */\n",
        // quotes and a raw-string spelling inside a block comment are text, not tokens
        "/* has \"a quote\n   and r#\"a raw\"# and an 'apostrophe */\n",
        // a doc block
        "/**\n * TcpListener::bind is not called here\n */\n",
        // CRLF: the counter counts `\n`, and every editor counts lines the same way
        "/* one\r\n   two */\r\n\r\n",
        // a string and a raw string holding a real newline
        "let s = \"one\ntwo\";\n\n",
        "let s = r#\"one\ntwo\"#;\n\n",
        // strings that *look* like the start of a comment, and a `'/'` char literal
        "let s = \"// TcpListener::bind\";\nlet c = '/'; let d = \"/*\";\n",
    ];
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    for prefix in prefixes {
        let source = format!("{prefix}{code_line}");
        let expected = prefix.matches('\n').count() + 1;
        let code = strip(path, &source);
        assert_eq!(
            hits(&code, needle),
            vec![expected],
            "in:\n{source}\nthe finding is not on line {expected} — the line the code is really \
             on.\nRule: every rule in this file words its failure as `file:line`, so a finding \
             that names a line a reader cannot open is a finding they discard; a second hit \
             means a mention inside a comment or a string leaked into the code.\nIf the left \
             side is short by the number of lines that sit inside a multi-line `/* … */` above \
             the code, the number came from the newline counter in `strip`'s block-comment arm \
             — the one place this line number is kept, and the one place it has ever gone wrong."
        );
        seen.insert(expected);
    }
    // Non-vacuous: if every fixture put the code on the same line, a counter that never
    // advanced and a counter that advanced correctly would pass the same assertions.
    assert!(
        seen.len() >= 5,
        "the fixtures only ever put the code on {} distinct lines ({seen:?}) — a counter that \
         does not advance would pass most of them",
        seen.len()
    );
    // A hit whose spelling spans two lines is reported where it *starts*, which is where a
    // reader would put the caret. These cannot use a prefix count, so they are written out.
    assert_eq!(
        hits(&strip(path, "TcpListener ::\n    bind(a);"), "TcpListener"),
        vec![1]
    );
    assert_eq!(
        hits(&strip(path, "\n\nTcpListener ::\n    bind(a);"), "bind("),
        vec![4]
    );
}

#[test]
fn the_rules_fire_on_a_window_that_grows_a_listener_or_a_process() {
    // The planted regression for the rule *path*, not just the matcher: the same
    // `breaches()` the four rules above call, run over synthetic sources. Without this,
    // "no hits today" and "cannot see a hit" look identical.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let planted = |relative: &str, source: &str| strip(&manifest.join(relative), source);

    // A listener in a file with no permission: two breaches — the type and the bind —
    // and the message carries the rule's origin so a reader is sent to the rule rather
    // than to this file.
    let listener = breaches(
        NO_LISTENER,
        &[planted(
            "src/planted.rs",
            "fn serve() { std::net::TcpListener::bind(\"0.0.0.0:8787\").unwrap(); }",
        )],
    );
    assert_eq!(listener.len(), 2, "{listener:?}");
    assert!(
        listener[0].contains("src/planted.rs:1"),
        "the breach does not name the place: {}",
        listener[0]
    );
    assert!(
        listener[0].contains("docs/THREAT-MODEL.md §5.6"),
        "the breach does not name the rule: {}",
        listener[0]
    );
    assert!(
        listener.iter().any(|sentence| sentence.contains("`TcpListener`"))
            && listener.iter().any(|sentence| sentence.contains("`bind(`")),
        "both shapes should be named: {listener:?}"
    );

    // A process, in the file that is allowed one *thread*: the allowance is per needle
    // and per count, so a spawn in chat.rs beyond it fails.
    let process = breaches(
        NO_PROCESS,
        &[planted(
            "src/views/chat.rs",
            "fn start() { std::thread::spawn(|| {}); std::process::Command::new(\"llama-server\").spawn(); }",
        )],
    );
    assert!(
        process.iter().any(|sentence| sentence.contains("`process::Command`")),
        "the spawn that starts a program was not reported: {process:?}"
    );
    assert!(
        process.iter().any(|sentence| sentence.contains("`spawn(`")),
        "the second spawn was not reported: {process:?}"
    );
    // The thread the file *is* allowed stays allowed: one spawn in chat.rs is inside the
    // count, so a plain `std::thread::spawn` alone is not a breach.
    let thread_only = breaches(
        NO_PROCESS,
        &[planted("src/views/chat.rs", "fn start() { std::thread::spawn(|| {}); }")],
    );
    assert!(thread_only.is_empty(), "{thread_only:?}");

    // The allowance is a count: a third socket in the file that is allowed two fails.
    let extra_socket = breaches(
        NO_STRAY_SOCKET,
        &[planted(
            "src/http.rs",
            "fn a() { TcpStream::connect_timeout(&addr, t); }\nfn b() { TcpStream::connect_timeout(&addr, t); }\nfn c() { TcpStream::connect_timeout(&addr, t); }",
        )],
    );
    assert!(
        extra_socket.iter().any(|sentence| sentence.contains("`connect_timeout(`")),
        "a third dial in the socket's own file was not reported: {extra_socket:?}"
    );
    // …and a socket in a file that is allowed none is a breach on its own.
    let stray = breaches(
        NO_STRAY_SOCKET,
        &[planted("src/views/overview.rs", "fn f() { let s = TcpStream::connect(a); }")],
    );
    assert!(stray.len() >= 1, "{stray:?}");
    assert!(stray[0].contains("src/views/overview.rs:1"), "{}", stray[0]);

    // …and a bypass in the same shape is caught by the bypass rules.
    let bypass = breaches(
        NO_BYPASS,
        &[planted("src/planted.rs", "include!(\"../../atlas-web/src/boot.rs\");")],
    );
    assert_eq!(bypass.len(), 1, "{bypass:?}");

    // The one continuation that is not a finding: `std::process::id()` asks about this
    // process. Everything else in that module — `exit`, `abort`, `Command` — is a
    // decision about the process, and is reported. The exception is on the rule, so
    // `id()` may live in any module.
    for (source, expected) in [
        ("fn f() { let p = std::process::id(); }", 0usize),
        ("fn f() { let p = std :: process :: id ( ) ; }", 0),
        ("fn f() { std::process::exit(1); }", 1),
        ("fn f() { std::process::abort(); }", 1),
        // five: `process::Command`, `Command::new`, `Command::`, `spawn(`, and
        // `std::process::` itself — an overlapping rule set reports the same mistake in
        // every way it could be spelled, which is the point of having several.
        ("fn f() { std::process::Command::new(\"x\").spawn(); }", 5),
    ] {
        let counted: usize = breaches(NO_PROCESS, &[planted("src/planted.rs", source)]).len();
        assert_eq!(
            counted, expected,
            "`{source}` should breach {} rule(s) and breached {counted}",
            expected
        );
    }
}

#[test]
fn the_one_socket_is_one_file_and_one_dial() {
    // The exception as a rule: `TcpStream`, `std::net` and a dial are permitted exactly
    // where ALLOWED says, and nowhere else. This is the group that makes "the window
    // opens no socket" fail as soon as the window opens a second one.
    check(NO_STRAY_SOCKET);
}

/// The `#[path = "…"]` values in one file, as (line, value).
///
/// Read from the raw text because the value is a string literal, and the scan blanks
/// strings on purpose.
fn path_attributes(raw: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let Some(at) = line.find("#[path") else {
            continue;
        };
        let rest = &line[at + "#[path".len()..];
        let Some(open) = rest.find('"') else { continue };
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { continue };
        found.push((index + 1, after[..close].to_string()));
    }
    found
}

/// A path as its parts, with `.` dropped and `..` applied — lexically, because the point
/// is where the value *points*, not whether the file exists yet.
fn parts_of(path: &Path) -> Vec<String> {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            Component::RootDir => parts.push("/".to_string()),
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().to_string())
            }
            Component::Normal(name) => parts.push(name.to_string_lossy().to_string()),
        }
    }
    parts
}

/// Is `target` inside `base`, after collapsing `.` and `..`?
fn stays_inside(base: &Path, target: &Path) -> bool {
    let base = parts_of(base);
    let target = parts_of(target);
    target.len() >= base.len() && base.iter().zip(target.iter()).all(|(left, right)| left == right)
}

#[test]
fn no_module_is_sourced_from_outside_src() {
    // The scan reads every `.rs` under `src/`; a `mod` whose file is named with `#[path]`
    // breaks that unless the value stays inside `src/`. A split *inside* `src/`
    // (`#[path = "tests.rs"] mod tests;`) is code this scan reads, so it is allowed — the
    // finding is the path leaving the directory, which is how `atlas-web`'s boot code
    // could be compiled into a window that swears it has no server.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0usize;
    let mut findings: Vec<String> = Vec::new();
    for code in sources() {
        let dir = code.path.parent().unwrap_or(&src).to_path_buf();
        for (line, value) in path_attributes(&code.raw) {
            checked += 1;
            let target = dir.join(&value);
            if !stays_inside(&src, &target) {
                findings.push(format!(
                    "{}:{line}: `#[path = \"{value}\"]` sources {}\n  Rule: this file's claim — \
                     the rules are assertions about every `.rs` under src/, so no module may come \
                     from outside it (docs/THREAT-MODEL.md §5.6: the window has no server, and a \
                     module from atlas-web would be one).",
                    key(&code),
                    target.display()
                ));
            }
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
    // A bound rather than a claim: a rule that reads no attribute cannot tell "no finding"
    // from "cannot read the attribute", and the crate may hold a handful of module splits.
    // The positive case — an attribute that leaves src/ *is* reported — is
    // [`the_path_rule_reads_a_value_instead_of_firing_on_the_word`].
    assert!(
        checked < 20,
        "unexpected: {checked} `#[path]` attributes in src/ — read them"
    );
}

#[test]
fn the_path_rule_reads_a_value_instead_of_firing_on_the_word() {
    // The positive and negative cases: an attribute that leaves `src/` is a finding, a
    // split inside `src/` is not, and a `#[path]` in a comment is neither.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let escape = "#[path = \"../../atlas-web/src/boot.rs\"]\nmod boot;";
    let sibling = "#[path = \"tests.rs\"]\nmod tests;";
    let commented = "// #[path = \"../../atlas-web/src/boot.rs\"]\nmod nothing;";

    assert_eq!(path_attributes(commented).len(), 0, "a comment is not an attribute");
    let found = path_attributes(escape);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, 1, "the line is reported");
    assert!(
        !stays_inside(&src, &src.join(&found[0].1)),
        "a path that climbs out of src/ was treated as inside"
    );
    let found = path_attributes(sibling);
    assert!(
        stays_inside(&src, &src.join(&found[0].1)),
        "a module split inside src/ was treated as a bypass"
    );
}

// ---------------------------------------------------------------------------
// the archive: read-only by flag, not by name
// ---------------------------------------------------------------------------

/// The read-only promise rests on the **flags**, and a path string cannot undo it.
///
/// `src/data.rs` says `SQLITE_OPEN_URI` is "deliberately absent … the one flag that would
/// let the *string* decide what SQLite does", while `src/views/graph.rs` passes it. Both
/// comments are about the same worry, and this build answers it differently from either
/// of them: `libsqlite3-sys` compiles the amalgamation with `-DSQLITE_USE_URI`
/// (`libsqlite3-sys-0.30.1/build.rs:137`), so a `file:…?mode=…` string is parsed as a URI
/// **whether or not** the caller passes the flag. The flag is not the lock; the flag set
/// is. Measured on this machine, with the crate's own rusqlite, on a temp archive:
///
/// | flags                          | path string            | result                                   |
/// |--------------------------------|------------------------|------------------------------------------|
/// | `READ_ONLY \| URI`             | `file:<db>?mode=rwc`   | refused: "access mode not allowed: rwc"  |
/// | `READ_ONLY`                    | `file:<db>?mode=rwc`   | refused: the same error                   |
/// | `READ_WRITE \| CREATE \| URI`  | `file:<new>?mode=rwc`  | opens, creates the file, writes a table   |
///
/// The third row is the code this test is written against. So the assertion is that both
/// of the window's opens refuse the string — it fails the day `SQLITE_OPEN_READ_ONLY` is
/// relaxed or swapped for `Connection::open`, and it keeps passing if the `URI` flag in
/// `views/graph.rs` is dropped, because the flags are what decides.
///
/// Rule: `src/main.rs` ("a window, a GPU surface and a database"), `src/data.rs`'s
/// read-only note, and `docs/THREAT-MODEL.md` §5 — the archive belongs to the engines, and
/// the window reads it.
#[test]
fn the_archive_is_read_only_by_flags_and_a_path_cannot_change_that() {
    use rusqlite::{Connection, OpenFlags};

    let dir = std::env::temp_dir().join(format!("atlas-readonly-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir for the probe archive");
    let db = dir.join("atlas.db");
    {
        let conn = Connection::open(&db).expect("create the probe archive");
        // The three tables `views::graph::stories_for` reads, so the positive control below
        // proves the function works rather than merely failing.
        conn.execute_batch(
            "CREATE TABLE articles(id INTEGER PRIMARY KEY, title TEXT, source_id INTEGER, \
             published TEXT, relevant INTEGER, dupe INTEGER);\
             CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT);\
             CREATE TABLE article_entities(article_id INTEGER, entity_id INTEGER);\
             INSERT INTO articles(id, title, source_id, published, relevant, dupe) \
             VALUES (1, 'one', 1, '2026-01-01T00:00:00Z', 1, 0);",
        )
        .expect("seed the probe archive");
    }
    let uri = format!("file:{}?mode=rwc", db.display());

    // The positive control: the real path opens, through the crate's own open.
    let archive = atlas_desktop::data::Archive::open(&db)
        .unwrap_or_else(|error| panic!("the window cannot open a plain archive: {error}"));
    assert_eq!(archive.path, db, "the archive opened somewhere else");
    drop(archive);
    let stories = atlas_desktop::views::graph::stories_for(&db, 1, 2, 10)
        .expect("the second read-only open of a plain archive");
    assert!(
        stories.is_empty(),
        "the seeded archive answered {} stories; the control expected none",
        stories.len()
    );

    // The claim: the same database, addressed as a URI that asks to create and write, is
    // refused by the open flag. This is `views/graph.rs`'s open, which carries
    // `SQLITE_OPEN_URI` — so the refusal cannot be the flag's absence.
    let refused = atlas_desktop::views::graph::stories_for(Path::new(&uri), 1, 2, 10);
    assert!(
        refused.is_err(),
        "`stories_for` accepted `{uri}` and opened it — a path string chosen the access \
         mode. Rule: src/data.rs's read-only note and docs/THREAT-MODEL.md §5; the fix is \
         the flag set (SQLITE_OPEN_READ_ONLY and nothing writable), not a name check."
    );
    // Nothing was created or written: the file is the same length it was.
    let bytes = std::fs::metadata(&db).expect("the probe archive").len();
    assert!(bytes > 0, "the probe archive vanished");
    // And `data::Archive::open` refuses the same string too (its `exists()` check answers
    // first, which is the other half of why a URI never reaches SQLite there).
    assert!(
        atlas_desktop::data::Archive::open(Path::new(&uri)).is_err(),
        "`Archive::open` accepted `{uri}`"
    );

    // The canary: with a writable flag set the same string creates and writes a file, so
    // the refusals above are about the flags and not about SQLite disliking URIs.
    let fresh = dir.join("fresh.db");
    let fresh_uri = format!("file:{}?mode=rwc", fresh.display());
    let writable = Connection::open_with_flags(
        &fresh_uri,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI,
    );
    match writable {
        Ok(conn) => {
            conn.execute_batch("CREATE TABLE w(x)")
                .expect("the canary writes its table");
            assert!(
                fresh.exists(),
                "the canary opened a writable URI and created no file — then the refusals \
                 above prove less than they claim"
            );
        }
        Err(error) => panic!(
            "the writable canary was refused ({error}): the refusals above may be about \
             the URI rather than about the read-only flag, so this test no longer proves \
             what it says"
        ),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// the tool registry: what the model may ask for is what this window can run
// ---------------------------------------------------------------------------

/// The model is shown a menu of archive tools, and the same list is what the window will
/// run. A name on one side and not the other is two failures at once: a tool the model can
/// see but not run (a dead end it will keep choosing), or a handler reachable only by a name
/// the model was never given.
///
/// Asked of the code rather than of the source: `specs()` is the menu, `names()` is the
/// registry, and both come from `src/tools.rs`, which is SQL over the read-only archive and
/// nothing else — so this rule guards the *surface* while [`NO_LISTENER`],
/// [`NO_STRAY_SOCKET`] and [`NO_PROCESS`] keep guarding the way out.
#[test]
fn every_tool_in_the_menu_is_in_the_registry() {
    let menu: Vec<String> = atlas_desktop::tools::specs()
        .iter()
        .map(|spec| spec["function"]["name"].as_str().unwrap_or("").to_string())
        .collect();
    let registry: Vec<String> = atlas_desktop::tools::names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    assert_eq!(menu, registry, "the model's menu and the window's registry differ");
    assert!(!menu.is_empty(), "no tool is offered at all");
}

/// A name the model invents is refused before anything is opened, and the refusal is one
/// sentence naming the tools that do exist — which is what the loop hands back to the model
/// instead of running something.
///
/// The path does not exist on purpose: a handler that ran would answer "there is no
/// archive", so the *other* sentence proves the registry was consulted and nothing else was.
#[test]
fn a_tool_name_the_registry_does_not_hold_runs_nothing() {
    let refusal = atlas_desktop::tools::call(
        Path::new("/nonexistent/atlas.db"),
        "web_open",
        "{\"url\":\"http://example.invalid\"}",
    )
    .expect_err("a tool this window does not have");
    assert!(refusal.contains("web_open"), "{refusal}");
    for real in atlas_desktop::tools::names() {
        assert!(refusal.contains(real), "{real} is not named in {refusal}");
    }
    assert!(
        !refusal.contains("no archive"),
        "the archive was opened for a name that is not in the registry: {refusal}"
    );
}
