//! No hard-coded classification vocabulary in shipped code.
//!
//! The rule: **nothing in this app may key off a fixed word list.** The model decides
//! what to search, what to read, what to tag, and what a story is; the archive decides
//! what it already knows. A list of words in a `.rs` file is one person's opinion about
//! the world, frozen into a build, invisible to the reader who wonders why their story
//! was not found — and it cannot be corrected except by a release.
//!
//! What is a *classification* list, in the words of the rule: a list that decides
//! whether a story is news, which entity it mentions or what kind of thing that is,
//! which topic or category it belongs to, whether it is relevant, or how it reads.
//!
//! What is not: vocabulary that describes a **format** or a **mechanism** rather than a
//! story —
//!
//! * SQL and schema names (`select`, `articles`, `pragma_table_info`) — the database's
//!   own words;
//! * HTTP and wire vocabulary (header names, `GET`/`POST`, status text, content types,
//!   `charset=` labels) — the protocol's;
//! * URL mechanics (schemes, file extensions, query parameter names) — a URL's shape,
//!   which classifies no story and names no publisher;
//! * operating-system facts (reserved Windows names, executable suffixes, environment
//!   variable allow-lists, browser bundle names) — the platform's;
//! * prompt vocabulary (the JSON field names and entity *types* a prompt teaches the
//!   model, its placeholders, its fence markers) — the model's own schema;
//! * language mechanics (function words, calendar names used to recognise a
//!   sentence-initial capital) — grammar, not judgement;
//! * reference data the app ships as a table (the gazetteer of place names, the
//!   country/alias file) — a dictionary, and the equivalent of a database row;
//! * security evidence (instruction-injection markers, wall/paywall phrases) — these
//!   name what a hostile page does to the *process*, and they are reported, never used
//!   to judge a story.
//!
//! Everything else is a finding. The scan is deliberately about **shape**: it collects
//! every run of four or more string literals that are mostly lower-case (a vocabulary
//! someone would match text against) in every shipped module of every crate, and
//! requires each run to be named in [`ALLOWED`] — or, when it is a known violation
//! owned by another agent, in [`OPEN`] with the exact fix. A new list cannot appear
//! without someone writing down which of those it is, and [`the_table_never_calls_a_content_list_mechanics`]
//! keeps the justifications honest.
//!
//! This file found one violation in the shipped code and was written after fixing it:
//! `atlas-ingest`'s `POSITIVE`/`NEGATIVE` lists decided a story's sentiment from 71
//! English words (see `atlas-ingest/src/enrich.rs`'s `heuristic_sentiment`).

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// the table
// ---------------------------------------------------------------------------

/// Why a vocabulary is allowed to exist, or that it is a known violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// SQL, schema and column names — the database's own vocabulary.
    Sql,
    /// HTTP, headers, status text, content types, wire tokens.
    Wire,
    /// URL and file-name mechanics: schemes, extensions, parameter names.
    Url,
    /// Operating-system facts: reserved names, executable suffixes, env allow-lists.
    Os,
    /// Prompt vocabulary: field names, entity types, placeholders, fence markers.
    Prompt,
    /// Language mechanics: function words, calendar names, morphology.
    Language,
    /// Reference data shipped as a table (gazetteer, country aliases).
    ///
    /// No table entry carries this class: `atlas-places/src/data/` is skipped by path (the
    /// scan says why), so the class is spelled here and not used — the docs of a table
    /// outlive its rows, and the retired web crates' entries that used it are gone.
    #[expect(dead_code, reason = "the class names a kind of vocabulary, not a row in it")]
    Reference,
    /// Security evidence: injection markers, wall/paywall phrases. Reported, never
    /// used to judge a story.
    Security,
    /// UI labels, column headers and element ids — what a screen says.
    Label,
    /// Format strings for a report or a table.
    Format,
    /// A measuring harness (the end-to-end probes, the benchmark's fixtures).
    Harness,
    /// A known violation, owned by another agent; the `why` says the exact fix.
    Open,
}

/// One file's shipped vocabularies: the class they belong to, why, and the runs.
///
/// Grouped by file under one reason, because a file's vocabulary is usually one decision
/// — `quality.rs`'s wall phrases and its injection markers are the same judgement about a
/// page. The *runs* are the key: `(count, first element)`, not a line number. The tree is
/// edited by several people at once, so a line-keyed table would fail for the wrong reason
/// every hour; a count is deliberate, because adding a word to an allowed list is a change
/// to a vocabulary and should make a person look here and say which class it is.
struct Group {
    file: &'static str,
    class: Class,
    why: &'static str,
    runs: &'static [(usize, &'static str)],
}

/// Shipped vocabularies, each with the reason it is not a content filter.
const ALLOWED: &[Group] = &[
    Group {
        file: "atlas-ai/src/error.rs",
        class: Class::Wire,
        why: "llama.cpp's own failure text (`context`, `exceed`, `too long`): it tells a context \
              overflow from another error and decides nothing about a story",
        runs: &[(4, "context")],
    },
    Group {
        file: "atlas-ai/src/specs.rs",
        class: Class::Prompt,
        why: "the argument types and field names a tool's JSON schema teaches the model (`url`, \
              `integer`, `string`)",
        runs: &[(4, "url")],
    },
    Group {
        file: "atlas-bench/src/fixture.rs",
        class: Class::Harness,
        why: "the synthetic articles the benchmark measures on",
        runs: &[(49, "report")],
    },
    Group {
        file: "atlas-bench/src/report.rs",
        class: Class::Format,
        why: "a table header's format string",
        runs: &[(10, "{:<24} {:>5} {:>9} {:>9} {:>9} {:>9} {:>10} {:>8}  {}\\n")],
    },
    Group {
        file: "atlas-config/src/download.rs",
        class: Class::Os,
        why: "shell, PowerShell and Python downloader names (`curl`, `Invoke-WebRequest`, \
              `urlopen`), matched when a script is audited for a download it should not make",
        runs: &[
            (4, "invoke-webrequest"),
            (6, "urlopen"),
            (5, "python"),
            (12, "curl"),
            (11, "echo"),
        ],
    },
    Group {
        file: "atlas-core/src/htmlscan.rs",
        class: Class::Wire,
        why: "HTML element and attribute names — the scanner's own grammar",
        runs: &[(12, "script"), (27, "div"), (4, "script"), (6, "body")],
    },
    Group {
        file: "atlas-dates/src/phrases.rs",
        class: Class::Language,
        why: "date words: month and weekday abbreviations, and the relative words (`now`, \
              `month`, `year`) a phrase is built from",
        runs: &[
            (8, "st"),
            (8, "a"),
            (8, "s"),
            (6, "m"),
            (8, "h"),
            (6, "d"),
            (5, "w"),
            (5, "month"),
            (5, "year"),
            (9, "now"),
        ],
    },
    Group {
        file: "atlas-dates/src/scanners.rs",
        class: Class::Language,
        why: "the connectors a written date uses in the languages the scanner reads (`um`, `de`, \
              `le`, `à`)",
        runs: &[(12, "um")],
    },
    Group {
        file: "atlas-desktop/src/views/monitors.rs",
        class: Class::Label,
        why: "table column headers",
        runs: &[(4, "name")],
    },
    Group {
        file: "atlas-desktop/src/views/topics.rs",
        class: Class::Label,
        why: "table column headers",
        runs: &[(4, "name")],
    },
    Group {
        file: "atlas-graph/src/lib.rs",
        class: Class::Language,
        why: "function words, verbs and calendar names: the grammar `is_junk_name`/`ok_name` \
              use to tell a name from a sentence (`the`, `said`, `monday`, and the corporate \
              suffixes canonicalisation strips). They are grammar, not a judgement about a \
              story — the four vocabularies of *things* this file's `OPEN` list named \
              (`JUNK_L` 1209, `JUNK_BASE` 83, `UI_JUNK` 77, `OUTLETS` 69) are deleted, and \
              whether a capitalised run is a page's furniture is now the article's own \
              evidence (`atlas-ingest/src/entities.rs`), not a list",
        runs: &[
            (481, "a"),
            (574, "a"),
            (446, "actualité"),
            (373, "about"),
            (103, "abril"),
            (24, "&"),
            (22, "a"),
            (12, "ag"),
            (26, "acting"),
        ],
    },
    Group {
        file: "atlas-guard/src/hostname.rs",
        class: Class::Security,
        why: "names that resolve inside a machine or a LAN (`localhost`, `home.arpa`, the IANA \
              special-use registry): an SSRF refusal: it is about the address, not the text",
        runs: &[(17, "localhost")],
    },
    Group {
        file: "atlas-guard/src/sanitize.rs",
        class: Class::Wire,
        why: "the HTML elements and attributes a page may keep — the sanitiser's grammar",
        runs: &[(16, "script"), (34, "p")],
    },
    Group {
        file: "atlas-ingest/src/gate.rs",
        class: Class::Url,
        why: "file extensions that are never an article *page* (documents, images, archives): a \
              URL's shape, and it names no publisher",
        runs: &[(62, "pdf")],
    },
    Group {
        file: "atlas-ingest/src/quality.rs",
        class: Class::Security,
        why: "wall/paywall phrases and instruction-injection markers: what a page is doing to the \
              process. They are reported, never used to judge a story",
        runs: &[
            (4, "ignore"),
            (8, "previous"),
            (6, "you are now"),
            (4, "new instruction"),
            (6, "send"),
            (5, "do not tell"),
            (6, "enable javascript"),
            (88, "subscribe to continue"),
        ],
    },
    Group {
        file: "atlas-ingest/src/read.rs",
        class: Class::Security,
        why: "markers of a page that only renders with JavaScript — the signal to fall back to \
              the browser renderer, not a judgement on the text",
        runs: &[(6, "enable javascript")],
    },
    Group {
        file: "atlas-ingest/src/entities.rs",
        class: Class::Language,
        why: "corporate suffixes (`inc`, `gmbh`, `ag`): `canonical_name` folds `Nvidia Inc` \
              onto `Nvidia`, which is normalisation of one name, not a judgement about which \
              thing it is. It no longer types anything — `guess_type` returns `MISC` until a \
              model answers, because measured on `data/atlas.db` all 4539 stored types were \
              this vocabulary's or a space count's (3478 ORG default, 947 PERSON = exactly the \
              two-word rows, 1 ORG holding a corporate token, 0 model receipts)",
        runs: &[(12, "ag")],
    },
    Group {
        file: "atlas-ingest/src/rules/inproc.rs",
        class: Class::Os,
        why: "the rule-engine binaries this workspace can run",
        runs: &[(5, "atlas-text")],
    },
    Group {
        file: "atlas-ingest/src/rules/mod.rs",
        class: Class::Os,
        why: "the rule-engine binaries this workspace can run",
        runs: &[(5, "atlas-text")],
    },
    Group {
        file: "atlas-ingest/src/tags.rs",
        class: Class::Language,
        why: "function words: a tag made only of these carries no content, which is a grammar \
              rule about labels, not a choice of topic",
        runs: &[(81, "a")],
    },
    Group {
        file: "atlas-ingest/src/titles.rs",
        class: Class::Language,
        why: "function words: what does not distinguish one headline from another (reprint \
              detection), not what a headline is about",
        runs: &[(81, "a")],
    },
    Group {
        file: "atlas-migrate/src/redact.rs",
        class: Class::Sql,
        why: "column names that hold reader-visible text and are redacted on export",
        runs: &[(7, "content"), (11, "title")],
    },
    Group {
        file: "atlas-net/src/charset.rs",
        class: Class::Wire,
        why: "charset labels the transport knows (`iso-8859-1`, `utf-16`)",
        runs: &[(9, "iso-8859-1"), (4, "utf-16")],
    },
    Group {
        file: "atlas-platform/src/browser.rs",
        class: Class::Os,
        why: "the browser executables and bundle names the launcher looks for",
        runs: &[(7, "google-chrome"), (4, "chrome")],
    },
    Group {
        file: "atlas-platform/src/child.rs",
        class: Class::Security,
        why: "environment names that look like credentials and are never passed to a child \
              (`pass`, `key`, `secret`, `token`) — the other half of the allow-list",
        runs: &[(8, "pass"), (7, "key")],
    },
    Group {
        file: "atlas-platform/src/host.rs",
        class: Class::Os,
        why: "platform tokens from `std::env::consts`",
        runs: &[(8, "linux")],
    },
    Group {
        file: "atlas-search/src/html.rs",
        class: Class::Wire,
        why: "HTML element names — the extractor's grammar",
        runs: &[(33, "div")],
    },
    Group {
        file: "atlas-text/src/publisher.rs",
        class: Class::Url,
        why: "host labels that say nothing about who publishes (`www`, `amp`, `edition`) and the \
              suffix labels in `example.co.uk`: a display name derived from a URL. There is no \
              list of publishers here",
        runs: &[(10, "www"), (9, "uk")],
    },
    Group {
        file: "atlas-text/src/tokens.rs",
        class: Class::Language,
        why: "function words of the tokeniser",
        runs: &[(84, "a")],
    },
    // The briefing scopes, in the four crates that spell them: the window draws them, the
    // agent answers them, the monitor refreshes them, the store keys rows by them.
    //
    // Not a vocabulary in this file's sense, and the distinction is the whole point of the
    // rule: these six words are a **closed set of table keys**, the address of a row
    // (`PRIMARY KEY(scope, key, hours)`), and no story is ever judged against them. The
    // content decision — what the sentence says, which stories it names — is the model's.
    // They appear as words rather than as an enum because they travel in JSON payloads and
    // in SQL, where a `String` is what both ends can read.
    Group {
        file: "atlas-desktop/src/views/brief.rs",
        class: Class::Sql,
        why: "the six briefing scopes: the row key the card reads, not a filter on a story",
        runs: &[(6, "window")],
    },
    Group {
        file: "atlas-ingest/src/brief.rs",
        class: Class::Sql,
        why: "the same six scopes, as the publisher of a briefing row",
        runs: &[(6, "window")],
    },
    Group {
        file: "atlas-monitor/src/brief.rs",
        class: Class::Sql,
        why: "the same six scopes, as the refresher of a briefing row (it refuses any other)",
        runs: &[(6, "window")],
    },
    Group {
        file: "atlas-store/src/briefings.rs",
        class: Class::Sql,
        why: "the same six scopes as the table's own closed set; `briefing_put` refuses the rest",
        runs: &[(6, "window")],
    },
];

/// Known violations that belong to another agent's file, so this test cannot go green by
/// pretending they are fine. Each `why` carries the fix.
///
/// **Empty, and that is the point.** The two entries this list held are closed:
/// `atlas-graph/src/lib.rs`'s `JUNK_L` (1209) / `JUNK_BASE` (83) / `UI_JUNK` (77) /
/// `OUTLETS` (69) were deleted, and `atlas-ingest/src/entities.rs`'s `guess_type` no
/// longer types a name from a corporate vocabulary — it writes `MISC` until a model
/// answers. Both files keep a vocabulary that is *grammar* (function words, calendar
/// names, the suffixes `canonical_name` folds away), and those are in [`ALLOWED`] with
/// their class. Adding an entry here is how to record a violation that cannot be fixed in
/// the file this test owns; a `Group` whose `class` is [`Class::Open`] must say `fix:`.
const OPEN: &[Group] = &[];

/// The group that accounts for a run, from either table.
fn accounted(file: &str, count: usize, first: &str) -> Option<&'static Group> {
    ALLOWED
        .iter()
        .chain(OPEN.iter())
        .find(|group| group.file == file && group.runs.iter().any(|run| *run == (count, first)))
}

// ---------------------------------------------------------------------------
// the rule
// ---------------------------------------------------------------------------

/// Every `.rs` under every crate's `src/`, as (relative path, source).
fn shipped() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    let entries = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", root.display()));
    for entry in entries.flatten() {
        let crate_dir = entry.path();
        let src = crate_dir.join("src");
        if !src.is_dir() {
            continue;
        }
        let name = crate_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        // A `target/` never appears here (it is not a crate), but a vendored crate would.
        if name.starts_with('.') || name == "target" {
            continue;
        }
        let mut paths = Vec::new();
        collect(&src, &mut paths);
        for path in paths {
            let relative = format!(
                "{}/{}",
                name,
                path.strip_prefix(&src)
                    .map(|rest| format!("src/{}", rest.display()))
                    .unwrap_or_else(|_| path.display().to_string())
            );
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            if is_test_module(&relative) {
                continue;
            }
            files.push((relative, source));
        }
    }
    files.sort();
    assert!(
        files.len() > 100,
        "the scan read {} files — the workspace's sources are not there, so every rule below \
         would pass without looking at anything",
        files.len()
    );
    files
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

/// Is this file a test module rather than shipped code?
///
/// Inline tests are dropped by [`without_test_code`]; a separate test file is not marked
/// inside itself, so the module *name* is the evidence, and this workspace is consistent
/// about it: `src/tests.rs`, `src/tests/…`, `src/*_tests.rs`. Their fixtures are full of
/// word lists that decide nothing about a story — a test asserting "these four words are
/// refused" is the opposite of a filter in the product.
fn is_test_module(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    if name == "tests.rs" || name.ends_with("_tests.rs") {
        return true;
    }
    relative
        .split('/')
        .any(|part| part == "tests" || part == "testdata" || part == "fixtures")
}

/// One run of string literals: a candidate vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    file: String,
    line: usize,
    first: String,
    count: usize,
    lowercase: usize,
    /// Elements that are *words*: lower-case letters, spaces, apostrophes, hyphens and
    /// digits, at least two characters. A format string (`"{:<24} {:>5}"`), a URL, a
    /// JSON body or a path is not a word list, however many literals it has.
    words: usize,
}

impl Run {
    /// Four or more literals, three quarters of them lower-case, four fifths of them
    /// words: the shape of a vocabulary someone matches prose against. A list of *types*
    /// written in capitals (`"PERSON" | "ORG"`) is vocabulary for a schema, not a filter
    /// on a story, and this shape rule deliberately does not reach it.
    fn is_candidate(&self) -> bool {
        self.count >= 4 && self.lowercase * 4 >= self.count * 3 && self.words * 5 >= self.count * 4
    }
}

#[test]
fn no_shipped_module_declares_a_classification_vocabulary() {
    let mut findings: Vec<String> = Vec::new();
    let mut seen: Vec<(String, usize, String)> = Vec::new();
    let mut candidates = 0usize;
    for (file, source) in shipped() {
        // `atlas-places/src/data/` is a reference table (country names, cities, country
        // aliases) exported as Rust so it needs no file at run time. It is data, not a
        // decision: the matching logic that reads it lives in `index.rs`/`scan.rs` and
        // is scanned like everything else.
        if file.starts_with("atlas-places/src/data/") {
            continue;
        }
        for run in runs(&file, &source) {
            if !run.is_candidate() {
                continue;
            }
            candidates += 1;
            seen.push((run.file.clone(), run.count, run.first.clone()));
            if accounted(&run.file, run.count, &run.first).is_none() {
                findings.push(format!(
                    "{}:{}: count={} first={:?} lower-case={}",
                    run.file, run.line, run.count, run.first, run.lowercase
                ));
            }
        }
    }
    assert!(
        candidates >= 10,
        "the scan found {candidates} candidate vocabularies — a scan that sees almost nothing \
         is not evidence of anything"
    );
    assert!(
        findings.is_empty(),
        "{} vocabulary/ies in shipped code are not accounted for:\n\n{}\n\n\
         If a list there decides what a story is about, who it mentions, or whether it is \
         news, it may not exist: the model or the archive decides that. If it is format or \
         mechanism vocabulary (SQL, headers, URL shapes, OS facts, prompt fields, function \
         words, security markers, reference data), add it to ALLOWED in this file with its \
         class and one sentence saying what it actually decides.",
        findings.len(),
        findings.join("\n")
    );
    // The other direction: every table entry must still describe something real, so a
    // deleted list takes its justification with it instead of proving nothing.
    for group in ALLOWED.iter().chain(OPEN.iter()) {
        for (count, first) in group.runs {
            assert!(
                seen.iter().any(|(file, seen_count, seen_first)| file == group.file
                    && seen_count == count
                    && seen_first == first),
                "{}: the table describes a vocabulary of {} literal(s) starting {:?} that no \
                 longer exists. Delete the entry (a justification for a list nobody has is a \
                 comment).",
                group.file,
                count,
                first
            );
        }
    }
}

/// A known violation that is still shipped is not allowed to look like the rest: the
/// open list is a debt, and this test says so out loud.
#[test]
fn the_open_defects_are_named_one_by_one() {
    for entry in OPEN {
        assert_eq!(
            entry.class,
            Class::Open,
            "{}: an OPEN entry with class {:?} — the open list is for known violations, and \
             everything in it must say so",
            entry.file,
            entry.class
        );
        assert!(
            entry.why.contains("fix:"),
            "{}: an OPEN entry must carry the exact fix (a sentence containing `fix:`), or the \
             next reader cannot close it",
            entry.file
        );
    }
}

/// The justifications must stay honest: no entry may be filed as mechanics on the
/// strength of its wording while being a content decision.
#[test]
fn the_table_never_calls_a_content_list_mechanics() {
    // Words that only make sense for a story-level judgement. They are the vocabulary of
    // the rule, not of the code, so this is a fixed list in a *test* — which is where a
    // fixed list is allowed: it classifies this file's own table, not a story.
    const CONTENT: &[&str] = &[
        "sentiment", "positive", "negative", "relevant", "newsworthy", "clickbait",
        "opinion", "topic word", "entity type", "classif", "score the story",
    ];
    for entry in ALLOWED {
        let why = entry.why.to_lowercase();
        for word in CONTENT {
            assert!(
                !why.contains(word),
                "{}: the justification mentions {word:?} — a vocabulary that decides that is not \
                 mechanics, whatever the sentence says. Either the list goes, or the entry \
                 belongs in OPEN with the fix.",
                entry.file
            );
        }
    }
}

// ---------------------------------------------------------------------------
// the scan itself
// ---------------------------------------------------------------------------

/// One token of a source file: a string literal (what a vocabulary is made of) or one
/// character of everything else.
enum Piece {
    Str { text: String, line: usize },
    Other(char),
}

/// The candidate vocabularies in one source file.
///
/// Runs are read from the *token* stream, not by looking for quote characters: a `'"'`
/// char literal, a `r#"…"#` raw string, a `br"…"` byte string and a lifetime `'a` are
/// four shapes a naive scanner gets wrong. (The first version of this file desynchronised
/// on `for tail in ["'s", "’s"]` in `atlas-graph` and swallowed seventeen hundred lines,
/// which reported one finding and hid a dozen — a scanner that silently skips a region is
/// worse than one that fails.)
fn runs(file: &str, source: &str) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut current: Vec<(String, usize)> = Vec::new();
    let mut separators_only = true;
    for piece in tokenize(&without_test_code(source)) {
        match piece {
            Piece::Str { text, line } => {
                if !current.is_empty() && !separators_only {
                    runs.push(finish(file, &mut current));
                }
                current.push((text, line));
                separators_only = true;
            }
            Piece::Other(ch) => {
                if !current.is_empty() && !(ch.is_whitespace() || ch == ',' || ch == '|') {
                    separators_only = false;
                }
            }
        }
    }
    if !current.is_empty() {
        runs.push(finish(file, &mut current));
    }
    runs
}

fn finish(file: &str, current: &mut Vec<(String, usize)>) -> Run {
    let elements = std::mem::take(current);
    let lowercase = elements
        .iter()
        .filter(|(text, _)| text.to_lowercase() == *text)
        .count();
    let words = elements.iter().filter(|(text, _)| is_word(text)).count();
    Run {
        file: file.to_string(),
        line: elements.first().map(|(_, line)| *line).unwrap_or(0),
        first: elements
            .first()
            .map(|(text, _)| text.clone())
            .unwrap_or_default(),
        count: elements.len(),
        lowercase,
        words,
    }
}

/// A word list's element: letters, spaces, hyphens, apostrophes and digits, two
/// characters or more. A format string, a URL, a JSON body or a path is not a word.
fn is_word(element: &str) -> bool {
    element.chars().count() >= 2
        && element.chars().all(|ch| {
            (ch.is_lowercase() && ch.is_alphabetic())
                || ch == ' '
                || ch == '-'
                || ch == '\''
                || ch.is_ascii_digit()
        })
}

/// One source file, comment-free, as a token stream.
///
/// Comments vanish (a comment between two literals is a separator, so it does not break
/// a run); strings keep their bytes; a char literal becomes one blank, because `'"'` is
/// not an opening quote and `'\''` is not an escape.
fn tokenize(source: &str) -> Vec<Piece> {
    let chars: Vec<char> = source.chars().collect();
    let mut out: Vec<Piece> = Vec::new();
    let mut line = 1usize;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();
        if ch == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && next == Some('*') {
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
        // A raw string: `r"…"`, `r#"…"#`, and the `b`-prefixed form.
        let raw_at = if ch == 'r' {
            Some(i + 1)
        } else if ch == 'b' && next == Some('r') {
            Some(i + 2)
        } else {
            None
        };
        if let Some(after_prefix) = raw_at {
            let mut j = after_prefix;
            let mut hashes = 0usize;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                let start_line = line;
                j += 1;
                let mut text = String::new();
                loop {
                    match chars.get(j) {
                        None => break,
                        Some('"') if (1..=hashes).all(|k| chars.get(j + k) == Some(&'#')) => {
                            j += 1 + hashes;
                            break;
                        }
                        Some('"') => {
                            text.push('"');
                            j += 1;
                        }
                        Some(&here) => {
                            if here == '\n' {
                                line += 1;
                            }
                            text.push(here);
                            j += 1;
                        }
                    }
                }
                out.push(Piece::Str {
                    text,
                    line: start_line,
                });
                i = j;
                continue;
            }
        }
        // A normal string, with escapes.
        if ch == '"' {
            let start_line = line;
            let mut text = String::new();
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => {
                        if let Some(escaped) = chars.get(i + 1) {
                            text.push('\\');
                            text.push(*escaped);
                        }
                        i += 2;
                    }
                    '"' => {
                        i += 1;
                        break;
                    }
                    '\n' => {
                        line += 1;
                        text.push('\n');
                        i += 1;
                    }
                    other => {
                        text.push(other);
                        i += 1;
                    }
                }
            }
            out.push(Piece::Str {
                text,
                line: start_line,
            });
            continue;
        }
        // A char literal or a lifetime. `'\''` is one token; `'a` is code.
        if ch == '\'' {
            if next == Some('\\') {
                let mut j = i + 2;
                if j < chars.len() {
                    j += 1;
                }
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                i = (j + 1).min(chars.len());
                out.push(Piece::Other(' '));
                continue;
            }
            if chars.get(i + 2) == Some(&'\'') {
                i += 3;
                out.push(Piece::Other(' '));
                continue;
            }
            out.push(Piece::Other('\''));
            i += 1;
            continue;
        }
        if ch == '\n' {
            line += 1;
        }
        out.push(Piece::Other(ch));
        i += 1;
    }
    out
}

/// `source` with every `#[cfg(…test…)]` item blanked out (newlines kept, so line numbers
/// survive): test fixtures are full of word lists and they decide nothing about a story.
fn without_test_code(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut blank = vec![false; chars.len()];
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '#' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 1;
            let mut depth = 0usize;
            let mut end = None;
            while j < chars.len() {
                match chars[j] {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if let Some(end) = end {
                let attribute: String = chars[i..=end].iter().collect();
                if attribute.contains("cfg(") && mentions_test(&attribute) {
                    let mut k = end + 1;
                    while chars.get(k).map(|ch| ch.is_whitespace()).unwrap_or(false) {
                        k += 1;
                    }
                    let stop = item_end(&chars, k);
                    for flag in blank.iter_mut().take(stop.min(chars.len())).skip(i) {
                        *flag = true;
                    }
                    i = stop;
                    continue;
                }
            }
        }
        i += 1;
    }
    let mut out = String::with_capacity(source.len());
    for (index, ch) in chars.iter().enumerate() {
        if blank[index] && *ch != '\n' {
            out.push(' ');
        } else {
            out.push(*ch);
        }
    }
    out
}

fn mentions_test(attribute: &str) -> bool {
    let mut word = String::new();
    for ch in attribute.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            word.push(ch);
        } else {
            if word == "test" {
                return true;
            }
            word.clear();
        }
    }
    word == "test"
}

/// The index just past the block that starts at `open`.
fn block_end(chars: &[char], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < chars.len() {
        if let Some(after) = skip_atom(chars, i) {
            i = after;
            continue;
        }
        match chars[i] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// The index just past the statement that starts at `from` (its `;`, or the end).
fn statement_end(chars: &[char], from: usize) -> usize {
    let mut i = from;
    while i < chars.len() {
        if let Some(after) = skip_atom(chars, i) {
            i = after;
            continue;
        }
        if chars[i] == ';' {
            return i + 1;
        }
        i += 1;
    }
    i
}

/// If a string, raw string, char literal or comment starts at `at`, the index just past
/// it — the shapes whose contents must not be read as braces or semicolons.
fn skip_atom(chars: &[char], at: usize) -> Option<usize> {
    let ch = *chars.get(at)?;
    let next = chars.get(at + 1).copied();
    if ch == '/' && next == Some('/') {
        let mut i = at + 2;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        return Some(i);
    }
    if ch == '/' && next == Some('*') {
        let mut depth = 1usize;
        let mut i = at + 2;
        while i < chars.len() && depth > 0 {
            if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                depth += 1;
                i += 1;
            } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                depth -= 1;
                i += 1;
            }
            i += 1;
        }
        return Some(i.min(chars.len()));
    }
    let raw_at = if ch == 'r' {
        Some(at + 1)
    } else if ch == 'b' && next == Some('r') {
        Some(at + 2)
    } else {
        None
    };
    if let Some(mut j) = raw_at {
        let mut hashes = 0usize;
        while chars.get(j) == Some(&'#') {
            hashes += 1;
            j += 1;
        }
        if chars.get(j) == Some(&'"') {
            j += 1;
            while j < chars.len() {
                if chars[j] == '"' && (1..=hashes).all(|k| chars.get(j + k) == Some(&'#')) {
                    return Some(j + 1 + hashes);
                }
                j += 1;
            }
            return Some(chars.len());
        }
    }
    if ch == '"' {
        let mut i = at + 1;
        while i < chars.len() {
            match chars[i] {
                '\\' => i += 2,
                '"' => return Some(i + 1),
                _ => i += 1,
            }
        }
        return Some(chars.len());
    }
    if ch == '\'' {
        if next == Some('\\') {
            let mut i = at + 2;
            if i < chars.len() {
                i += 1;
            }
            while i < chars.len() && chars[i] != '\'' {
                i += 1;
            }
            return Some((i + 1).min(chars.len()));
        }
        if chars.get(at + 2) == Some(&'\'') {
            return Some(at + 3);
        }
    }
    None
}

/// The index just past the *item* that starts at `from` — the thing an attribute is
/// attached to.
///
/// `#[cfg(test)] mod tests { … }` is one item whose attribute is followed by a name, not
/// by a brace: reading only a statement (`to the next `;``) would blank the attribute and
/// then stop inside the module, leaving the rest of the test fixture in the scan. That is
/// how the first version of this file missed `atlas-topics`'s fixtures while reporting
/// `atlas-ingest`'s.
fn item_end(chars: &[char], from: usize) -> usize {
    let mut i = from;
    while i < chars.len() {
        if let Some(after) = skip_atom(chars, i) {
            i = after;
            continue;
        }
        match chars[i] {
            '{' => return block_end(chars, i),
            ';' => return i + 1,
            // `const X: [&str; 3] = &[…]`, `static …`, `let …`: the item ends at the
            // next `;` that is not inside brackets or braces of its own.
            '=' => return statement_end(chars, i),
            _ => i += 1,
        }
    }
    i
}

// ---------------------------------------------------------------------------
// the scan's own proof: it must see a word list and not see the rest
// ---------------------------------------------------------------------------

#[test]
fn the_scan_catches_a_word_list_and_ignores_legitimate_shapes() {
    let found = |source: &str| {
        runs("planted.rs", source)
            .into_iter()
            .filter(Run::is_candidate)
            .count()
    };

    // Every one of these is the shape this file exists to stop: lower-case words, four or
    // more, the way a filter on prose would be written. If the scan cannot see the first
    // of them, the table below proves nothing.
    for planted in [
        r#"const SENT: &[&str] = &["advance", "agreement", "gain", "growth", "win"];"#,
        r#"let TOPIC = ["chip", "nvidia", "semiconductor", "foundry"];"#,
        r#"matches!(kind, "chip" | "nvidia" | "semiconductor" | "foundry")"#,
        "const A: &[&str] = &[\n    \"chip\",\n    \"nvidia\",\n    \"semiconductor\",\n    \"foundry\",\n];",
    ] {
        assert_eq!(
            found(planted), 1,
            "the scan did not see the vocabulary in:\n{planted}\nA rule that cannot see the \
             attack it names is a comment, not a test."
        );
    }

    // …and these are the shapes it must not fire on, or the table would be a wall of noise
    // and the next person would switch the rule off.
    let quiet = [
        // a format string is not a vocabulary
        r#"let row = format!("{:<24} {:>5} {:>9}", name, n, k);"#,
        // URLs, paths and codes are not words
        r#"let probes = ["/api/health", "http://127.0.0.1:8787/x", "/../etc/passwd", "GET /"];"#,
        // a type vocabulary in capitals is a schema, not a filter on prose
        r#"matches!(kind, "PERSON" | "ORG" | "LOC" | "TOPIC" | "EVENT")"#,
        // fewer than four literals: too small to be a classification vocabulary, and the
        // boundary is stated rather than implied (see the report's open items)
        r#"let two = ["chip", "nvidia"];"#,
        // test code decides nothing
        "#[cfg(test)]\nmod tests {\n    const WORDS: &[&str] = &[\"chip\", \"nvidia\", \"semiconductor\", \"foundry\"];\n}",
        "#[cfg(test)]\nconst WORDS: &[&str] = &[\"chip\", \"nvidia\", \"semiconductor\", \"foundry\"];",
    ];
    for source in quiet {
        assert_eq!(
            found(source), 0,
            "the scan fired on something legitimate:\n{source}\nA rule that fires on a format \
             string gets deleted, and then it protects nothing."
        );
    }

    // The desynchronisation this file was rewritten to fix: an apostrophe-shaped literal
    // (`"'s"`) once made the scanner skip a thousand lines and report one finding where
    // there were twelve. A real list *after* it must still be found.
    assert_eq!(
        found(r#"let tails = ["'s", "\u{2019}s"]; const L: &[&str] = &["alpha", "beta", "gamma", "delta"];"#),
        1,
        "a literal holding an apostrophe hid the list after it"
    );
    // A comment between two elements keeps the run together; a `;` ends it.
    assert_eq!(
        found("const A: &[&str] = &[\"alpha\", /* x */ \"beta\", \"gamma\", \"delta\"];"),
        1
    );
    assert_eq!(
        found("let a = [\"alpha\", \"beta\"]; let b = [\"gamma\", \"delta\"];"),
        0
    );
}
