//! "Does this line download something?" — the rule table and the matchers.
//!
//! One decision per rule, and one table ([`DOWNLOAD_RULES`]) that names them, so
//! the whole vocabulary of a download is readable in one screen: `pip install`,
//! `git clone`, `urlretrieve`, `urlopen` with a write, `wget`, `curl -O`, a
//! PowerShell download, a browser download, a tunnel. [`audit_line_allowed`]
//! then decides that a comment, a docstring or a printed hint is documentation,
//! not a fetch.
//!
//! It does not: walk a tree, report a finding, or cache a verdict — that is
//! [`crate::audit`]. A rule is a predicate over one lower-cased line.


fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&index| &hay[index..index + needle.len()] == needle)
}

/// `\bword\b` on a lowercase copy of the line.
fn has_word(hay: &str, word: &str) -> bool {
    let hay = hay.as_bytes();
    if word.is_empty() || hay.len() < word.len() {
        return false;
    }
    let mut start = 0;
    while start < hay.len() {
        let found = match find_bytes(&hay[start..], word.as_bytes()) {
            Some(found) => start + found,
            None => return false,
        };
        let before_ok = found == 0 || !is_word_byte(hay[found - 1]);
        let after = found + word.len();
        let after_ok = after >= hay.len() || !is_word_byte(hay[after]);
        if before_ok && after_ok {
            return true;
        }
        start = found + 1;
    }
    false
}

/// Every occurrence of an ASCII `needle` in an ASCII-lowercase `hay`.
fn find_spans(hay: &str, needle: &str) -> Vec<usize> {
    let hay = hay.as_bytes();
    let needle = needle.as_bytes();
    let mut out = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    let mut index = 0;
    while index + needle.len() <= hay.len() {
        if &hay[index..index + needle.len()] == needle {
            out.push(index);
            index += needle.len();
        } else {
            index += 1;
        }
    }
    out
}

/// `\b` on both sides of the match at `at`.
fn word_at(low: &str, at: usize, word: &str) -> bool {
    let bytes = low.as_bytes();
    let before_ok = at == 0 || !is_word_byte(bytes[at - 1]);
    let after = at + word.len();
    let after_ok = after >= bytes.len() || !is_word_byte(bytes[after]);
    before_ok && after_ok
}

const STOP: [u8; 4] = [b'|', b';', b'&', b'\n'];

/// The rest of the line up to the first `|`, `;`, `&` or newline — Python's
/// `[^|;&\n]*`.
fn tail_until_stop(low: &str, from: usize) -> &str {
    let bytes = low.as_bytes();
    let mut end = from;
    while end < bytes.len() && !STOP.contains(&bytes[end]) {
        end += 1;
    }
    &low[from..end]
}

fn any_token_in(tail: &str, tokens: &[&'static str]) -> bool {
    tokens.iter().any(|token| tail.contains(token))
}

// ---------------------------------------------------------------------------
// the rules — one predicate per line of the Python table
// ---------------------------------------------------------------------------

fn lower(line: &str) -> String {
    line.to_ascii_lowercase()
}

/// `\bpip3?\b[^|;&\n]*\binstall\b`
fn rule_pip_install(low: &str) -> bool {
    let bytes = low.as_bytes();
    for at in find_spans(low, "pip") {
        // `pip3?` — the digit is part of the word, so it needs its own check.
        let pip3 = low[at..].starts_with("pip3") && word_at(low, at, "pip3");
        if !pip3 && !word_at(low, at, "pip") {
            continue;
        }
        let mut after = at + 3;
        if pip3 {
            after += 1;
        }
        if after < bytes.len() && is_word_byte(bytes[after]) {
            continue;
        }
        if has_word(tail_until_stop(low, after), "install") {
            return true;
        }
    }
    false
}

/// `\bgit\s+clone\b`
fn rule_git_clone(low: &str) -> bool {
    let bytes = low.as_bytes();
    for at in find_spans(low, "git") {
        if !word_at(low, at, "git") {
            continue;
        }
        let mut index = at + 3;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index == start || index >= bytes.len() {
            continue;
        }
        if low[index..].starts_with("clone") && word_at(low, index, "clone") {
            return true;
        }
    }
    false
}

/// `\burlretrieve\s*\(`
fn rule_urlretrieve(low: &str) -> bool {
    let bytes = low.as_bytes();
    for at in find_spans(low, "urlretrieve") {
        if !word_at(low, at, "urlretrieve") {
            continue;
        }
        let mut index = at + "urlretrieve".len();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index) == Some(&b'(') {
            return true;
        }
    }
    false
}

/// `\burlopen\s*\([^)\n]*(?:MODEL_URL|GEO_URL|GEO_MAP_URL|LLAMACPP_RELEASES_API|browser_download_url)`
fn rule_urlopen_download(line: &str) -> bool {
    const NAMES: [&str; 5] = [
        "model_url",
        "geo_url",
        "geo_map_url",
        "llamacpp_releases_api",
        "browser_download_url",
    ];
    let low = lower(line);
    let bytes = low.as_bytes();
    for at in find_spans(&low, "urlopen") {
        if !word_at(&low, at, "urlopen") {
            continue;
        }
        let mut index = at + "urlopen".len();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index) != Some(&b'(') {
            continue;
        }
        let mut end = index + 1;
        while end < bytes.len() && bytes[end] != b')' && bytes[end] != b'\n' {
            end += 1;
        }
        if any_token_in(&low[index + 1..end], &NAMES) {
            return true;
        }
    }
    false
}

/// `(?:^|[\s;&|(])wget(?:\.exe)?\b`
fn rule_wget(low: &str) -> bool {
    let bytes = low.as_bytes();
    for at in find_spans(low, "wget") {
        // `(?:^|[\s;&|(])` — a bare `wget` at the start of a word is not enough,
        // it must start the command.
        let anchored = at == 0
            || bytes[at - 1].is_ascii_whitespace()
            || matches!(bytes[at - 1], b';' | b'&' | b'|' | b'(');
        if !anchored {
            continue;
        }
        let mut after = at + 4;
        if low[after..].starts_with(".exe") {
            after += 4;
        }
        if after >= bytes.len() || !is_word_byte(bytes[after]) {
            return true;
        }
    }
    false
}

/// `\bcurl(?:\.exe)?\b[^|;&\n]*(?:\s-O\b|\s-L\b|--remote-name|--output|-o\s|--url)`
fn rule_curl_download(low: &str) -> bool {
    let bytes = low.as_bytes();
    for at in find_spans(low, "curl") {
        if !word_at(low, at, "curl") {
            continue;
        }
        let mut after = at + 4;
        if low[after..].starts_with(".exe") {
            after += 4;
        }
        if after < bytes.len() && is_word_byte(bytes[after]) {
            continue;
        }
        let tail = tail_until_stop(low, after);
        if tail.contains("--remote-name") || tail.contains("--output") || tail.contains("--url") {
            return true;
        }
        let tail_bytes = tail.as_bytes();
        for flag in ["-o", "-l"] {
            for offset in find_spans(tail, flag) {
                let before_space = offset > 0 && tail_bytes[offset - 1].is_ascii_whitespace();
                let after_flag = offset + 2;
                let after_space =
                    after_flag < tail_bytes.len() && tail_bytes[after_flag].is_ascii_whitespace();
                if before_space || after_space {
                    return true;
                }
            }
        }
    }
    false
}

/// `\b(Invoke-WebRequest|Invoke-RestMethod|DownloadFile|Start-BitsTransfer)\b`
fn rule_powershell_download(low: &str) -> bool {
    const NAMES: [&str; 4] = [
        "invoke-webrequest",
        "invoke-restmethod",
        "downloadfile",
        "start-bitstransfer",
    ];
    NAMES.iter().any(|name| has_word(low, name))
}

const HOST_TOKENS: [&str; 5] = [
    "huggingface.co",
    "raw.githubusercontent.com",
    "api.github.com",
    "releases/download",
    "browser_download_url",
];

const FETCH_VERBS: [&str; 10] = [
    "urlopen",
    "urlretrieve",
    "curl",
    "wget",
    "invoke-webrequest",
    "invoke-restmethod",
    "downloadfile",
    "requests.get",
    "hf_hub_download",
    "snapshot_download",
];

/// `(urlopen|urlretrieve|\bcurl\b|\bwget\b|…)[^\n]*(huggingface\.co|…)`
fn rule_verb_then_host(low: &str) -> bool {
    for verb in FETCH_VERBS {
        let anchored = matches!(verb, "curl" | "wget");
        for at in find_spans(low, verb) {
            if anchored && !word_at(low, at, verb) {
                continue;
            }
            let after = at + verb.len();
            if after > low.len() {
                continue;
            }
            if any_token_in(&low[after..], &HOST_TOKENS) {
                return true;
            }
        }
    }
    false
}

/// `(huggingface\.co|…)[^\n]*(urlopen|urlretrieve|\bcurl\b|\bwget\b|invoke-|downloadfile)`
fn rule_host_then_verb(low: &str) -> bool {
    const VERBS: [&str; 6] = [
        "urlopen",
        "urlretrieve",
        "curl",
        "wget",
        "invoke-",
        "downloadfile",
    ];
    for host in HOST_TOKENS {
        for at in find_spans(low, host) {
            let after = at + host.len();
            if after > low.len() {
                continue;
            }
            if any_token_in(&low[after..], &VERBS) {
                return true;
            }
        }
    }
    false
}

/// `\bplaywright\b[^|;&\n]*\binstall\b` | `python\d?\s+-m\s+playwright` |
/// `npx\s+playwright` | `ms-playwright.*install`
fn rule_browser_download(low: &str) -> bool {
    for at in find_spans(low, "playwright") {
        if word_at(low, at, "playwright") && has_word(tail_until_stop(low, at + 10), "install") {
            return true;
        }
        let head = low[..at].trim_end();
        if head.ends_with("npx") || head.ends_with("npx.cmd") {
            return true;
        }
        if let Some(before) = head.strip_suffix("-m") {
            let before = before.trim_end();
            if ["python", "python2", "python3", "python.exe", "py"]
                .iter()
                .any(|runner| before.ends_with(runner))
            {
                return true;
            }
        }
    }
    if find_spans(low, "ms-playwright").is_empty() {
        return false;
    }
    has_word(low, "install")
}

/// `\b(cloudflared|ngrok|localtunnel|serveo\.net|bore\.pub)\b`
fn rule_tunnel(low: &str) -> bool {
    const NAMES: [&str; 5] = ["cloudflared", "ngrok", "localtunnel", "serveo.net", "bore.pub"];
    NAMES.iter().any(|name| has_word(low, name))
}

/// A program that fetches, named as the *program* of a process rather than as the
/// first word of a shell line: `Command::new("curl")`, `program: "wget"`,
/// `exec("pip")`.
///
/// The shell-shaped rules above all read a command line, and a Rust caller does not
/// write one: it hands an argv array to `std::process::Command`. Without this rule
/// the boot audit reads the Rust tree and clears `Command::new("curl").arg("-O")`,
/// which is the same download with the flags in the wrong place. The vocabulary is
/// the one `atlas-guard/tests/audit_downloads.rs` uses for the same question — a
/// program name is a security decision, not a claim about the news.
fn rule_program_name(low: &str) -> bool {
    const PROGRAMS: [&str; 12] = [
        "curl", "wget", "pip", "pip3", "npm", "npx", "yarn", "brew", "git", "powershell", "pwsh",
        "winget",
    ];
    const CALLS: [&str; 6] = [
        "command::new(",
        "program:",
        "exec(",
        "execve(",
        "spawn(",
        "system(",
    ];
    for call in CALLS {
        for at in find_spans(low, call) {
            let after = at + call.len();
            if after > low.len() {
                continue;
            }
            let rest = low[after..].trim_start();
            for quote in ['"', '\''] {
                for program in PROGRAMS {
                    let quoted = format!("{}{}{}", quote, program, quote);
                    if rest.starts_with(&quoted) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn check_pip(low: &str) -> bool {
    rule_pip_install(low)
}
fn check_git_clone(low: &str) -> bool {
    rule_git_clone(low)
}
fn check_urlretrieve(low: &str) -> bool {
    rule_urlretrieve(low)
}
fn check_urlopen_download(low: &str) -> bool {
    rule_urlopen_download(low)
}
fn check_wget(low: &str) -> bool {
    rule_wget(low)
}
fn check_curl_download(low: &str) -> bool {
    rule_curl_download(low)
}
fn check_powershell_download(low: &str) -> bool {
    rule_powershell_download(low)
}
fn check_verb_then_host(low: &str) -> bool {
    rule_verb_then_host(low)
}
fn check_host_then_verb(low: &str) -> bool {
    rule_host_then_verb(low)
}
fn check_browser_download(low: &str) -> bool {
    rule_browser_download(low)
}
fn check_tunnel(low: &str) -> bool {
    rule_tunnel(low)
}
fn check_program_name(low: &str) -> bool {
    rule_program_name(low)
}

/// One rule of the audit: a name and the predicate that recognises it.
pub struct DownloadRule {
    pub name: &'static str,
    /// The predicate, run against the ASCII-lowercased line.
    pub check: fn(&str) -> bool,
}

/// The rules, in table order. A finding names the rule that fired; the order is
/// the Python order, so the most specific shape wins.
pub const DOWNLOAD_RULES: [DownloadRule; 12] = [
    DownloadRule { name: "pip-install", check: check_pip },
    DownloadRule { name: "git-clone", check: check_git_clone },
    DownloadRule { name: "urlretrieve", check: check_urlretrieve },
    DownloadRule { name: "urlopen-download", check: check_urlopen_download },
    DownloadRule { name: "wget", check: check_wget },
    DownloadRule { name: "curl-download", check: check_curl_download },
    DownloadRule { name: "powershell-download", check: check_powershell_download },
    DownloadRule { name: "url-fetch", check: check_verb_then_host },
    DownloadRule { name: "host-fetch", check: check_host_then_verb },
    DownloadRule { name: "browser-download", check: check_browser_download },
    DownloadRule { name: "tunnel", check: check_tunnel },
    DownloadRule { name: "program-name", check: check_program_name },
];

/// Which rule (first match, in table order) fires on a line, if any.
///
/// The line is lowercased once and handed to every rule; each rule is a scan
/// over that copy, so a line costs one allocation rather than one per rule.
pub fn rule_hit(line: &str) -> Option<&'static str> {
    let low = lower(line);
    DOWNLOAD_RULES
        .iter()
        .find(|rule| (rule.check)(&low))
        .map(|rule| rule.name)
}

// ---------------------------------------------------------------------------
// what a line is allowed to be
// ---------------------------------------------------------------------------

/// True for comments, printed instructions and pattern tables — never a call.
///
/// Docs and instructions are not code paths: `echo "run pip install x"` tells a
/// human what to type, it downloads nothing. A rule table is documentation too —
/// a bare string entry and a URL constant describe a download, they cannot
/// perform one — so the audit never reports its own rules (or another
/// auditor's) as a finding.
///
/// The "it only talks about a download" excuse stops at a redirect: a sentence
/// someone reads never ends in `> file`, and `curl -O … >> fetch.log` used to be
/// excused because the word `log` appears in it. A redirect outside a quoted
/// string is therefore a *call* no matter which printing word the line carries;
/// a `>` inside quotes stays a hint.
pub fn audit_line_allowed(line: &str, suffix: &str) -> bool {
    let stripped = line.trim();
    if stripped.is_empty() {
        return true;
    }
    if ["#", "//", "/*", "*", ";"]
        .iter()
        .any(|prefix| stripped.starts_with(prefix))
    {
        return true;
    }
    let low = lower(line);
    const NOISY: [&str; 11] = [
        "echo",
        "printf",
        "write-host",
        "write-output",
        "write-information",
        "write-warning",
        "print",
        "warn",
        "log",
        "info",
        "note",
    ];
    if !redirects(line) && NOISY.iter().any(|word| has_word(&low, word)) {
        return true;
    }
    if line.contains("127.0.0.1") || low.contains("localhost") || line.contains("::1") {
        return true; // local llama-server / CDP / health checks are ours
    }
    if line.contains("re.compile(") {
        return true; // pattern table entry, not an action
    }
    if suffix == ".rs" && rust_line_without_a_call(stripped) {
        // The `.rs` half of the same excuse, and the reason `AUDIT_EXTENSIONS`
        // can hold `.rs` at all: this crate's own rule table is written in Rust,
        // so a line of it names `pip install` as a *string* and no parenthesis is
        // anywhere near it. A call is written with `(` in Rust; a line without one
        // is a value, and a value cannot fetch.
        return true;
    }
    if starts_with_constant(stripped) {
        return true; // URL/host constant: a printed hint, never a fetch
    }
    if suffix == ".py" && starts_with_python_string(stripped) {
        return true; // bare string entry in a Python list/tuple
    }
    false
}

/// True when the line sends output somewhere — a `>` outside quotes.
///
/// `>` inside a quote is part of a sentence (`echo "copy it > data/"`), and it is
/// also how a PowerShell path or a comparison looks; both are text, not a shell.
fn redirects(line: &str) -> bool {
    let mut quoted: Option<char> = None;
    let mut previous = '\0';
    for ch in line.chars() {
        match quoted {
            Some(quote) => {
                if ch == quote {
                    quoted = None;
                }
            }
            None if ch == '"' || ch == '\'' => quoted = Some(ch),
            // `->` is a Rust/Python arrow, not a redirect.
            None if ch == '>' && previous != '-' => return true,
            None => {}
        }
        previous = ch;
    }
    false
}

/// True when a line of Rust carries no call, so it is a value and not an action.
///
/// In Rust a call is written with a parenthesis (`Command::new(…)`, `.arg(…)`,
/// `format!(…)`), a macro with `!`, and a field access with none. A line with no
/// `(` — a bare string literal in a table, a `const … = [ … ]` declaration, a
/// `Rule { name: "…", check: … }` entry — is data. It is what lets the audit read
/// the shipped `.rs` tree without reporting its own vocabulary as a download, and
/// it cannot excuse a fetch: a fetch is a call, and a call has a `(`.
///
/// The lines that are *not* excused are exactly the ones worth reading: the
/// `Command::new("curl")` a hostile edit would add is a call, so it is a finding
/// whether or not it is on a line of its own.
fn rust_line_without_a_call(stripped: &str) -> bool {
    !stripped.contains('(') && !stripped.contains('!')
}

/// `^[A-Z][A-Z0-9_]{2,}\s*=\s*["'(]`
fn starts_with_constant(stripped: &str) -> bool {
    let mut chars = stripped.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    let rest: Vec<char> = chars.collect();
    let mut index = 0;
    while index < rest.len()
        && (rest[index].is_ascii_uppercase() || rest[index].is_ascii_digit() || rest[index] == '_')
    {
        index += 1;
    }
    if index < 2 {
        return false;
    }
    while index < rest.len() && rest[index].is_whitespace() {
        index += 1;
    }
    if !matches!(rest.get(index), Some('=')) {
        return false;
    }
    let mut after = index + 1;
    while after < rest.len() && rest[after].is_whitespace() {
        after += 1;
    }
    matches!(rest.get(after), Some('"') | Some('\'') | Some('('))
}

/// `^(?:[rbfu]{0,2})["']`
fn starts_with_python_string(stripped: &str) -> bool {
    let mut rest = stripped;
    let mut taken = 0;
    while taken < 2 {
        match rest.chars().next() {
            Some(ch) if "rbfu".contains(ch) => {
                rest = &rest[ch.len_utf8()..];
                taken += 1;
            }
            _ => break,
        }
    }
    matches!(rest.chars().next(), Some('"') | Some('\''))
}
