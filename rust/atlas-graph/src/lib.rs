//! atlas-graph — the relation layer of ATLAS: the engine *and* the tables it owns.
//!
//! This file is the engine. It works on rows it is handed. Since the migration of
//! the DB-bound half of `server/graph.py` the crate also reads and writes the
//! graph tables itself, through [`atlas_store`] — see [`graphdb`].
//!
//! # The engine (no I/O at all)
//!
//! * `edges`      — the weighted co-occurrence edges between entities.
//! * `why`        — the sentence and the evidence behind one entity pair.
//! * `threads`    — story-thread clustering, exactly as `server/graph.py` builds it.
//! * `neighbours` — the top-k related entities of one entity, with a reason.
//!
//! Ported behaviour is marked where it matters:
//!
//! * `edges`      ← `recompute_edges()` + `_keep_ids()`
//! * `why`        ← the evidence wording of `recompute_links()` (`readable()`
//!                  vocabulary), applied to an entity pair
//! * `threads`    ← `recompute_threads()`, `_thread_title()`, `_thread_words()`
//! * `neighbours` ← `bfs_subgraph()`'s neighbourhood, ranked
//! * junk-name filtering ← `is_junk_name()`, `_has_fragment_punct()`, `_TIME_RE`
//! * name folding ← `_canonical_name()`, `_merge_key()`, `_ok_name()`
//!
//! # The database half ([`graphdb`])
//!
//! `link_article`, `recompute` (edges, links, threads, prune), `subgraph`, `path`,
//! `links_for`, `profile` and `top_entities` — the operations of `server/graph.py`
//! that need SQLite. They open exactly one file, the database they are told to
//! open, and nothing else: no socket, no thread, no environment, no child
//! process, no port. Every write is one transaction.
//!
//! Ordering is deterministic everywhere — `(weight desc, id asc)` at every step —
//! so the same rows always produce byte-identical output and the UI never
//! reshuffles a graph that did not change. Where SQLite itself leaves a tie
//! open — `ORDER BY c DESC, en.count DESC LIMIT 24` has no third key — the
//! statement is kept identical to `server/graph.py`'s, so both sides see the same
//! rows in the same order; nothing is re-sorted in Rust that SQL already ordered.
//!
//! No article text is ever printed: errors carry a scrubbed reason, counters
//! carry ids, counts and milliseconds.

#![forbid(unsafe_code)]

pub mod graphdb;
pub mod graphlink;
pub mod normalise;
pub mod graphread;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use atlas_lib::json::{self, Json};

#[cfg(test)]
mod db_tests;
#[cfg(test)]
mod measure_tests;
#[cfg(test)]
mod normalise_tests;

// ---------------------------------------------------------------- constants
// (ported from server/graph.py, same names, same numbers)

/// `MAX_ENTITIES_PER_ARTICLE` — the per-article cap that keeps hubs from
/// swallowing the whole graph.
///
/// Re-measured against the model's own list (2026-11-22, the live `data/atlas.db`:
/// 1 149 linked articles, 27 102 pairs if nothing were capped):
///
/// * 83 articles carry more than 12 names, all of them the old extractor's 13–30-name
///   lists; the cap drops 591 of their entity rows and 10 237 of the 27 102 pairs
///   (37.8 %), and 372 of those 591 rows name something a second article also names —
///   so the cap does trim real relations, on *heuristic* lists.
/// * The model's list cannot reach the cap: `atlas-ingest`'s prompt asks for at most
///   eight names, and eight is under twelve, so every model-typed relation is written
///   (`db_tests::the_model_s_eight_names_never_reach_the_pair_cap` pins C(8,2) = 28).
///
/// Twelve stays: it never binds on an answer and trims only the extractor's 14-name
/// fallback by two. Raising it would spend the extra pairs on the lists that are still
/// built from shape rather than from the model, which is the half this cap is for.
const MAX_ENTITIES_PER_ARTICLE: usize = 12;
/// `MAX_PLACES_PER_ARTICLE` — places are context, never the whole budget.
const MAX_PLACES_PER_ARTICLE: usize = 3;
/// `TYPE_RANK["LOC"]`: from this rank up an entity is a place.
const RANK_LOC: u8 = 5;
/// `LEN(name) < 3` is rejected by `is_junk_name`.
pub(crate) const MIN_NAME_CHARS: usize = 3;
/// Evidence rows one `why` answer carries by default (`links_for(limit=10)`).
const MAX_EVIDENCE: usize = 10;
/// Topic names in one reason sentence.
const MAX_TOPICS: usize = 3;
/// `recompute_threads(min_score=0.20, max_size=40)`.
pub(crate) const DEFAULT_MIN_SCORE: f64 = 0.20;
pub(crate) const DEFAULT_MAX_SIZE: usize = 40;
/// `_BAD_CHARS` — a name containing one of these is page chrome, not a name.
const BAD_CHARS: &[char] = &[
    '|', '<', '>', '{', '}', '[', ']', '@', '€', '$', '%', '^', '=', '~', '`', ';', ':',
];

/// The subcommands this binary answers, in the order `/api/status` lists them.
///
/// `main.rs` is nothing but this table plus `atlas_lib::cli::serve`; the engine
/// and the database half both live behind [`run`] so a test can call them
/// without a process.
pub const SUBCOMMANDS: &[&str] = &[
    "edges",
    "why",
    "threads",
    "neighbours",
    "link_article",
    "recompute",
    "normalise",
    "subgraph",
    "path",
    "links_for",
    "profile",
    "top_entities",
];

/// Dispatch one subcommand: one JSON object in, one JSON object out.
pub fn run(cmd: &str, raw: &str) -> Result<String, String> {
    let input = atlas_lib::cli::input_object(raw)?;
    match cmd {
        // the pure engine
        "edges" => cmd_edges(&input),
        "why" => cmd_why(&input),
        "threads" => cmd_threads(&input),
        "neighbours" => cmd_neighbours(&input),
        // the half that owns its tables
        "link_article" => graphdb::cmd_link_article(&input),
        "recompute" => graphdb::cmd_recompute(&input),
        "normalise" => normalise::cmd_normalise(&input),
        "subgraph" => graphread::cmd_subgraph(&input),
        "path" => graphread::cmd_path(&input),
        "links_for" => graphread::cmd_links_for(&input),
        "profile" => graphread::cmd_profile(&input),
        "top_entities" => graphread::cmd_top_entities(&input),
        other => Err(format!("unknown subcommand: {}", other)),
    }
}

// ---------------------------------------------------------------- junk names
// The word sets below are **grammar**: function words, verbs, weekday and month names,
// connectives, corporate suffixes. They say how a sentence is built, never what a story is
// about, and they cannot decide whether a *thing* is a thing.
//
// They used to be joined by four vocabularies of things — `JUNK_L` (1209 names and page
// words), `JUNK_BASE` (83), `UI_JUNK` (77) and `OUTLETS` (69 publishers). Those decided
// which capitalised run could become an entity, which is the one job this app reserves for
// the model: 99 of this archive's 1435 articles carry, in the `publisher` column, one of
// those 69 publishers (13 distinct), and 42 articles' title or summary name one in the text
// (`cargo test --offline -p atlas-graph --lib measure -- --ignored --nocapture`) — under the
// old gate none of them could be an entity, and the list left no trace in the database to
// review. Measured before their removal:
// 0 of the stored entity names are in `JUNK_L`, so nothing in the archive changes; what
// changes is that the next story's names are decided by its text, not by a release.

/// Lower-cased word sets the name rules ask about.
pub(crate) struct Lex {
    /// `_STOP_L` — the multilingual function-word list.
    stop: HashSet<&'static str>,
    /// `TEXT_STOP` — the stop list `_thread_words()` filters headlines with.
    text_stop: HashSet<&'static str>,
    /// `_COMMON` — everyday words that are never a single-token name.
    common: HashSet<&'static str>,
    /// `_BLOCK_L` — verbs and headline filler.
    block: HashSet<&'static str>,
    /// `_DAYS_MONTHS` — weekday and month names in the six feed languages.
    days_months: HashSet<&'static str>,
    /// A name never starts or ends with one of these: the grammar lists above, plus the
    /// role words of `_EDGE_JUNK_EXTRA` (`ceo`, `dr`, `former`) and the connectives a
    /// multi-word name may keep in the middle.
    edge_grammar: HashSet<&'static str>,
    /// `_PROSE_L` — the words that only ever belong to running text.
    prose: HashSet<&'static str>,
    /// `_DETERMINERS` — `"Nvidia the SEC"` is prose, not a name.
    determiners: HashSet<&'static str>,
    /// `_CONJUNCTIONS` — `"Dell and Vertiv"` is a co-mention, not a name.
    conjunctions: HashSet<&'static str>,
    /// `CONNECTORS` — the only "middle" words a run may keep.
    connectors: HashSet<&'static str>,
}

fn word_set(words: &[&'static str]) -> HashSet<&'static str> {
    words.iter().copied().collect()
}

impl Lex {
    pub(crate) fn new() -> Lex {
        let days = word_set(lists::DAYS_MONTHS);
        let connectors = word_set(lists::CONNECTORS);
        let mut edge_grammar = word_set(lists::STOP_L);
        edge_grammar.extend(word_set(lists::BLOCK_L));
        edge_grammar.extend(days.iter().copied());
        edge_grammar.extend(connectors.iter().copied());
        edge_grammar.extend(word_set(lists::EDGE_JUNK_EXTRA));
        Lex {
            stop: word_set(lists::STOP_L),
            text_stop: word_set(lists::TEXT_STOP),
            common: word_set(lists::COMMON),
            block: word_set(lists::BLOCK_L),
            days_months: days,
            edge_grammar,
            prose: word_set(lists::COMMON),
            determiners: word_set(lists::DETERMINERS),
            conjunctions: word_set(lists::CONJUNCTIONS),
            connectors,
        }
    }
    /// `is_junk_name()` — is this string a name at all?
    ///
    /// Structure and grammar only: a minimum length, no timestamp, no fragment
    /// punctuation, no page-chrome characters, and not a function word. What is *not*
    /// here is a vocabulary of things or publishers — see [`Lex::ok_name`] for why, and
    /// for where that decision went instead.
    fn is_junk_name(&self, name: &str) -> bool {
        if name.is_empty() || name.chars().count() < MIN_NAME_CHARS {
            return true;
        }
        let low = squash_lower(name);
        if self.stop.contains(low.as_str()) {
            return true;
        }
        if has_time_marker(name) || has_fragment_punct(name) {
            return true;
        }
        for ch in name.chars() {
            if BAD_CHARS.contains(&ch) {
                return true;
            }
        }
        false
    }

    /// `_ok_name()` — the plausibility gate `link_article()` and
    /// `_purge_junk_entities()` apply before a name may become an entity row.
    ///
    /// Ported check by check, including the two that read like style rules but are the
    /// ones that keep headline sentences out of the graph: a real name keeps its capitals,
    /// and a determiner or conjunction inside a short run makes it prose.
    ///
    /// # What is not in here any more
    ///
    /// Four vocabularies of *things* were subtracted from this gate: `JUNK_L` (1209 words:
    /// outlets, months, navigation text, generic nouns), `JUNK_BASE` (83), `UI_JUNK` (77)
    /// and `OUTLETS` (69 publishers). They decided which capitalised run could become an
    /// entity, and an entity list is the one thing this app does not let a build decide: a
    /// publisher named in 99 of this archive's 1435 articles (13 publishers, measured on the
    /// `publisher` column) could never be an entity, and
    /// nothing in the database recorded that a list had said so. Measured before the
    /// removal: 0 of the 4539 stored names are in any of the four, so the archive's graph
    /// reads exactly as it did; the difference is that the next story's names are decided
    /// by its text.
    ///
    /// The half that a list was standing in for — is this capitalised run the *furniture*
    /// of a page or a thing the story names? — is answered where the text is:
    /// `atlas-ingest`'s `admit` refuses a single word the article itself writes in lower
    /// case ("Continue", "Subscribe", "Clique"), because the article is the evidence and a
    /// release is an opinion. What is left here is what a string can be judged by alone:
    /// length, shape, capitals, the grammar lists, and no possessive tail.
    pub(crate) fn ok_name(&self, name: &str) -> bool {
        if name.is_empty() || name.chars().count() < MIN_NAME_CHARS {
            return false;
        }
        if self.is_junk_name(name) {
            return false;
        }
        // `name.isupper()`: at least one cased character, and all of them upper.
        let cased: Vec<char> = name.chars().filter(|c| c.is_alphabetic()).collect();
        if !cased.is_empty() && cased.iter().all(|c| c.is_uppercase()) && name.chars().count() <= 4
        {
            return false;
        }
        let words: Vec<&str> = name.split_whitespace().collect();
        if words.len() > 5 {
            return false;
        }
        // a name never starts or ends with a preposition or article ("Nvidia For")
        let first = words[0].to_lowercase();
        let last = words[words.len() - 1].to_lowercase();
        if self.edge_grammar.contains(first.as_str()) || self.edge_grammar.contains(last.as_str())
        {
            return false;
        }
        for w in &words {
            let low = w
                .to_lowercase()
                .trim_matches(|c| c == '.' || c == ',' || c == '\'' || c == '’')
                .to_string();
            // "Nvidia the SEC": a determiner inside is prose
            if self.determiners.contains(low.as_str()) {
                return false;
            }
            // "Dell and Vertiv" is a co-mention, not a name
            if self.conjunctions.contains(low.as_str()) && words.len() <= 3 {
                return false;
            }
            if self.edge_grammar.contains(low.as_str())
                && !self.connectors.contains(low.as_str())
            {
                return false;
            }
            if w.contains("'s") || w.contains("’s") {
                return false;
            }
        }
        // a real name keeps its capitals: "Here’s My Top AI Stock" is a sentence
        let caps = words
            .iter()
            .filter(|w| w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false))
            .count();
        if words.len() >= 4 && caps < words.len() - 1 {
            return false;
        }
        true
    }

    /// The words a "why" line may name: `t not in _DAYS_MONTHS and t not in
    /// _BLOCK_L and t not in _PROSE_L and not is_junk_name(t)`.
    pub(crate) fn is_wording_word(&self, token: &str) -> bool {
        !self.days_months.contains(token)
            && !self.block.contains(token)
            && !self.prose.contains(token)
            && !self.is_junk_name(token)
    }
}

/// `" ".join(name.replace("’", "'").split()).lower()`.
fn squash_lower(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut first = true;
    for word in name.replace('\u{2019}', "'").split_whitespace() {
        if !first {
            out.push(' ');
        }
        first = false;
        out.push_str(word);
    }
    out.to_lowercase()
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// `\bword\b`, case-insensitive, for the ASCII time-zone markers of `_TIME_RE`.
///
/// Python's `\b` is Unicode-aware; byte-wise ASCII boundaries only differ for a
/// non-ASCII letter glued to `gmt`/`utc`/…, which no timestamp does.
fn bounded_token(hay: &[u8], word: &[u8]) -> bool {
    if word.is_empty() || hay.len() < word.len() {
        return false;
    }
    for i in 0..=(hay.len() - word.len()) {
        if !hay[i..i + word.len()].eq_ignore_ascii_case(word) {
            continue;
        }
        let before_ok = i == 0 || !is_word_byte(hay[i - 1]);
        let after = i + word.len();
        let after_ok = after == hay.len() || !is_word_byte(hay[after]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// `_TIME_RE` from server/graph.py:
/// `(?:\bgmt\b|\butc\b|\bcet\b|\bcest\b|\best\b|\bpst\b|\d{1,2}[:.]\d{2}|\+\d{1,2}\b)`.
fn has_time_marker(name: &str) -> bool {
    let b = name.as_bytes();
    for word in [b"gmt".as_slice(), b"utc", b"cet", b"cest", b"est", b"pst"] {
        if bounded_token(b, word) {
            return true;
        }
    }
    // \d{1,2}[:.]\d{2} — every start position is tried, like the regex engine
    // does, so "123:45" still matches on its "3:45".
    for i in 0..b.len() {
        if !b[i].is_ascii_digit() {
            continue;
        }
        for len in 1..=2usize {
            let sep = i + len;
            if sep >= b.len() {
                continue;
            }
            if !b[i..sep].iter().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if b[sep] != b':' && b[sep] != b'.' {
                continue;
            }
            let d0 = sep + 1;
            if d0 + 1 < b.len() && b[d0].is_ascii_digit() && b[d0 + 1].is_ascii_digit() {
                return true;
            }
        }
    }
    // \+\d{1,2}\b
    for i in 0..b.len() {
        if b[i] != b'+' {
            continue;
        }
        let mut j = i + 1;
        let mut digits = 0usize;
        while j < b.len() && b[j].is_ascii_digit() {
            digits += 1;
            j += 1;
        }
        if (1..=2).contains(&digits) && (j == b.len() || !is_word_byte(b[j])) {
            return true;
        }
    }
    false
}

/// `_has_fragment_punct()` — a period inside a name means it is a sentence
/// fragment, not an entity ("Brasil.", "Nvidia Corp.").
fn has_fragment_punct(name: &str) -> bool {
    for tok in name.split_whitespace() {
        if !tok.contains('.') {
            continue;
        }
        let trimmed =
            tok.trim_matches(|c| matches!(c, '"' | '\'' | '„' | '“' | '”' | '‘' | '’'));
        if !is_abbrev(trimmed) {
            return true;
        }
    }
    false
}

/// `_ABBREV_RE` — `^(?:[A-Za-zÄÖÜäöü]{1,3}\.)+$`.
fn is_abbrev(tok: &str) -> bool {
    let mut letters = 0usize;
    let mut groups = 0usize;
    for ch in tok.chars() {
        let is_letter = ch.is_ascii_alphabetic() || matches!(ch, 'Ä' | 'Ö' | 'Ü' | 'ä' | 'ö' | 'ü');
        if is_letter {
            letters += 1;
            if letters > 3 {
                return false;
            }
        } else if ch == '.' {
            if letters == 0 {
                return false;
            }
            letters = 0;
            groups += 1;
        } else {
            return false;
        }
    }
    groups > 0 && letters == 0
}

/// Characters `_canonical_name()` strips from both ends:
/// `" ,;:\"'„“”‘’«»…-—|/"`.
const CANON_EDGE_CHARS: &[char] = &[
    ' ', ',', ';', ':', '"', '\'', '„', '“', '”', '‘', '’', '«', '»', '…', '-', '—', '|', '/',
];

/// `_canonical_name()` — merge trivial variants: case noise, a trailing
/// possessive, an `Inc`/`Corp`/`Ltd` suffix.
///
/// Statement for statement from `server/graph.py`, including the corporate
/// suffix in the *middle* ending a name (`"Nvidia Corp NVDA"` → `"Nvidia"`)
/// and the three-pass loop that peels `"Nvidia Inc.'s"` down to `"Nvidia"`.
pub(crate) fn canonical_name(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let replaced = name
        .replace('\u{2019}', "'")
        .replace('\u{a0}', " ")
        .replace('\u{2013}', "-");
    let mut n: String = replaced
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .trim_matches(|c| CANON_EDGE_CHARS.contains(&c))
        .to_string();
    // a corporate suffix in the middle ends the name: "Nvidia Corp NVDA" -> "Nvidia"
    let toks: Vec<&str> = n.split_whitespace().collect();
    if toks.len() > 1 {
        for i in 0..toks.len() - 1 {
            let low = toks[i].trim_matches('.').to_lowercase();
            if lists::CORP_TOKENS.contains(&low.as_str()) {
                n = toks[..i].join(" ");
                break;
            }
        }
    }
    for _ in 0..3 {
        let m = strip_corp_suffix(&strip_possessive(&n))
            .trim_matches(|c| c == ' ' || c == ',' || c == '.')
            .to_string();
        if m.is_empty() || m == n {
            break;
        }
        n = m;
    }
    n
}

/// `_CORP_SUFFIX_RE.sub("", s)` — `[,\s]+(?:inc|corp|…|company)$`, ignore case.
fn strip_corp_suffix(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    for start in 0..chars.len() {
        if !(chars[start] == ',' || chars[start].is_whitespace()) {
            continue;
        }
        let mut j = start;
        while j < chars.len() && (chars[j] == ',' || chars[j].is_whitespace()) {
            j += 1;
        }
        if j == start {
            continue;
        }
        let tail: String = chars[j..].iter().collect();
        if lists::CORP_SUFFIXES
            .iter()
            .any(|suffix| tail.eq_ignore_ascii_case(suffix) || tail == *suffix)
        {
            return chars[..start].iter().collect();
        }
    }
    s.to_string()
}

/// `_POSSESSIVE_RE.sub("", s)` — `(?:['’]s)$`.
fn strip_possessive(s: &str) -> String {
    for tail in ["'s", "’s"] {
        if let Some(head) = s.strip_suffix(tail) {
            return head.to_string();
        }
    }
    s.to_string()
}

/// `_merge_key()` — the case-insensitive identity of a name.
pub(crate) fn merge_key(name: &str) -> String {
    canonical_name(name).to_lowercase()
}

mod lists {

/// Ported verbatim from server/graph.py (_STOP_L).
pub const STOP_L: &[&str] = &[
    "a", "aber", "according", "ad", "after", "agli", "ainda", "al", "alla", "alle", "allem",
    "allen", "aller", "alles", "allo", "als", "also", "amid", "among", "an", "anche", "and",
    "andere", "anderem", "anderen", "anderer", "anderes", "ao", "aos", "après", "aquel",
    "aquella", "as", "at", "até", "au", "auch", "auf", "aus", "aussi", "aux", "avec", "baixe",
    "before", "bei", "beim", "between", "bin", "bist", "but", "by", "ce", "ces", "cet", "cette",
    "che", "chi", "ci", "com", "come", "comme", "como", "con", "confira", "cosa", "cual",
    "cuando", "cui", "da", "dal", "dalla", "dalle", "dallo", "damit", "dann", "dans", "das",
    "de", "degli", "dei", "dein", "deine", "del", "della", "delle", "dello", "dem", "den",
    "denen", "denn", "depois", "der", "deren", "des", "despite", "después", "di", "die", "dies",
    "diese", "diesem", "diesen", "dieser", "dieses", "do", "doch", "donde", "dont", "dopo",
    "dort", "dos", "dove", "du", "durch", "during", "déjà", "e", "ecco", "ed", "ein", "eine",
    "einem", "einen", "einer", "eines", "einige", "el", "ela", "elas", "ele", "eles", "ella",
    "ellas", "elle", "elles", "ellos", "em", "en", "entre", "er", "era", "es", "esa", "esas",
    "ese", "eso", "esos", "essa", "esse", "essere", "est", "esta", "estas", "este", "esto",
    "estos", "está", "están", "et", "etwas", "eu", "euer", "eure", "fa", "fare", "first", "foi",
    "for", "from", "fue", "für", "gegen", "gewesen", "gli", "ha", "hab", "habe", "haben",
    "hanno", "hat", "hatte", "hatten", "he", "her", "here", "hier", "hin", "hinter", "his",
    "ho", "há", "i", "ich", "il", "ils", "im", "in", "indem", "ins", "into", "io", "isso",
    "ist", "isto", "it", "its", "ja", "je", "jede", "jedem", "jeden", "jeder", "jedes", "jene",
    "jetzt", "já", "kann", "kein", "keine", "können", "könnte", "la", "las", "le", "lee", "lei",
    "leia", "les", "leur", "leurs", "lo", "los", "lui", "ma", "machen", "mais", "man", "manche",
    "mas", "me", "mein", "meine", "mes", "mesmo", "meu", "mi", "mia", "mie", "minha", "mio",
    "mis", "mit", "mon", "muito", "muss", "musste", "muy", "my", "más", "na", "nach", "nas",
    "ne", "nei", "nel", "nella", "nelle", "nello", "new", "next", "nicht", "nichts", "no",
    "noch", "noi", "non", "nos", "not", "notre", "nous", "nuestra", "nuestro", "nun", "nur",
    "não", "nós", "o", "ob", "oder", "ohne", "on", "one", "onto", "or", "os", "ou", "our",
    "over", "où", "par", "para", "pas", "pela", "pelo", "per", "perché", "pero", "più", "plus",
    "por", "pour", "qual", "quale", "quali", "quand", "quando", "quanto", "que", "quella",
    "quelle", "quelli", "quello", "quem", "questa", "queste", "questi", "questo", "qui",
    "quien", "quoi", "sa", "saiba", "said", "sans", "says", "se", "según", "sehr", "sei",
    "sein", "seine", "seinem", "seinen", "seiner", "seines", "selbst", "selon", "sem", "ser",
    "ses", "seu", "she", "si", "sia", "sich", "sie", "sin", "sind", "so", "sobre", "solche",
    "soll", "sollte", "son", "sondern", "sono", "sonst", "sont", "sous", "su", "sua", "sue",
    "sugli", "sul", "suo", "suoi", "sur", "sus", "são", "ta", "também", "te", "tem", "temos",
    "ter", "tes", "than", "that", "the", "their", "then", "there", "these", "they", "this",
    "those", "ton", "tra", "très", "tu", "tua", "tue", "tuo", "tuoi", "tus", "two", "um", "uma",
    "un", "una", "unas", "und", "under", "une", "uno", "unos", "uns", "unser", "unsere",
    "unter", "upon", "usted", "ustedes", "veja", "ver", "vi", "via", "você", "vocês", "voi",
    "vom", "von", "vor", "vos", "votre", "vous", "war", "waren", "warum", "was", "we", "weg",
    "weil", "weiter", "welche", "welchem", "welchen", "welcher", "welches", "wenn", "wer",
    "werde", "werden", "while", "wie", "wieder", "will", "wir", "wird", "wirst", "with", "wo",
    "wollen", "wollte", "würde", "würden", "y", "ya", "yo", "you", "your", "zu", "zum", "zur",
    "zwar", "zwischen", "à", "él", "était", "être", "über",
];

/// Ported verbatim from server/graph.py (TEXT_STOP).
pub const TEXT_STOP: &[&str] = &[
    "a", "aber", "about", "according", "ad", "added", "after", "again", "agli", "ainda", "al",
    "all", "alla", "alle", "allem", "allen", "aller", "alles", "allo", "als", "also", "amid",
    "among", "an", "anche", "and", "andere", "anderem", "anderen", "anderer", "anderes", "any",
    "ao", "aos", "après", "aquel", "aquella", "are", "as", "at", "até", "au", "auch", "auf",
    "aus", "aussi", "aux", "avec", "back", "baixe", "because", "before", "bei", "beim",
    "between", "billion", "bin", "bist", "both", "but", "by", "can", "ce", "ces", "cet",
    "cette", "che", "chef", "chefs", "chi", "ci", "com", "come", "comme", "como", "con",
    "confira", "continued", "continues", "cosa", "could", "cual", "cuando", "cui", "da", "dal",
    "dalla", "dalle", "dallo", "damit", "dann", "dans", "das", "day", "days", "de", "degli",
    "dei", "dein", "deine", "del", "della", "delle", "dello", "dem", "den", "denen", "denn",
    "depois", "der", "deren", "des", "despite", "después", "di", "die", "dies", "diese",
    "diesem", "diesen", "dieser", "dieses", "do", "doch", "donde", "dont", "dopo", "dort",
    "dos", "dove", "du", "durch", "during", "déjà", "e", "each", "ecco", "ed", "ein", "eine",
    "einem", "einen", "einer", "eines", "einige", "el", "ela", "elas", "ele", "eles", "ella",
    "ellas", "elle", "elles", "ellos", "em", "en", "enable", "entre", "er", "era", "es", "esa",
    "esas", "ese", "eso", "esos", "essa", "esse", "essere", "est", "esta", "estas", "este",
    "esto", "estos", "está", "están", "et", "etwas", "eu", "euer", "eure", "even", "fa", "fare",
    "first", "five", "foi", "for", "four", "from", "fue", "für", "gegen", "get", "gets",
    "gewesen", "gli", "got", "ha", "hab", "habe", "haben", "had", "hanno", "has", "hat",
    "hatte", "hatten", "have", "he", "her", "here", "hier", "hin", "hinter", "his", "ho", "how",
    "há", "i", "ich", "il", "ils", "im", "in", "indem", "ins", "into", "io", "isso", "ist",
    "isto", "it", "its", "ja", "javascript", "je", "jede", "jedem", "jeden", "jeder", "jedes",
    "jene", "jetzt", "just", "já", "kann", "kein", "keine", "können", "könnte", "la", "las",
    "last", "le", "lee", "lei", "leia", "les", "less", "leur", "leurs", "like", "lo", "loading",
    "los", "lui", "ma", "machen", "made", "mais", "make", "man", "manche", "many", "mas", "may",
    "me", "mein", "meine", "mes", "mesmo", "meu", "mi", "mia", "mie", "might", "million",
    "minha", "mio", "mis", "mit", "mon", "month", "months", "more", "most", "much", "muito",
    "muss", "musste", "must", "muy", "my", "más", "na", "nach", "nas", "ne", "nei", "nel",
    "nella", "nelle", "nello", "new", "news", "next", "nicht", "nichts", "no", "noch", "noi",
    "non", "nos", "not", "notre", "nous", "now", "nuestra", "nuestro", "nun", "nur", "não",
    "nós", "o", "ob", "oder", "ohne", "on", "one", "only", "onto", "or", "os", "other", "ou",
    "our", "over", "own", "où", "par", "para", "pas", "pela", "pelo", "per", "percent",
    "perché", "pero", "più", "please", "plus", "por", "pour", "qual", "quale", "quali", "quand",
    "quando", "quanto", "que", "quella", "quelle", "quelli", "quello", "quem", "questa",
    "queste", "questi", "questo", "qui", "quien", "quoi", "report", "reported", "reports", "sa",
    "saiba", "said", "same", "sans", "say", "says", "se", "según", "sehr", "sei", "sein",
    "seine", "seinem", "seinen", "seiner", "seines", "selbst", "selon", "sem", "ser", "ses",
    "seu", "she", "should", "si", "sia", "sich", "sie", "sin", "sind", "so", "sobre", "solche",
    "soll", "sollte", "some", "son", "sondern", "sono", "sonst", "sont", "sous", "still",
    "stories", "story", "su", "sua", "such", "sue", "sugli", "sul", "suo", "suoi", "sur", "sus",
    "são", "ta", "também", "te", "tell", "tem", "temos", "ter", "tes", "than", "that", "the",
    "their", "them", "then", "there", "these", "they", "this", "those", "three", "through",
    "time", "times", "today", "told", "tomorrow", "ton", "too", "tra", "très", "tu", "tua",
    "tue", "tuo", "tuoi", "tus", "two", "um", "uma", "un", "una", "unas", "und", "undefined",
    "under", "une", "uno", "unos", "uns", "unser", "unsere", "unter", "upon", "usted",
    "ustedes", "veja", "ver", "very", "vi", "via", "você", "vocês", "voi", "vom", "von", "vor",
    "vos", "votre", "vous", "war", "waren", "warum", "was", "we", "week", "weeks", "weg",
    "weil", "weiter", "welche", "welchem", "welchen", "welcher", "welches", "wenn", "wer",
    "werde", "werden", "were", "what", "when", "where", "which", "while", "who", "why", "wie",
    "wieder", "will", "wir", "wird", "wirst", "with", "wo", "wollen", "wollte", "would",
    "writes", "wrote", "würde", "würden", "y", "ya", "year", "years", "yesterday", "yo", "you",
    "your", "zu", "zum", "zur", "zwar", "zwischen", "à", "él", "était", "être", "über",
];

/// Ported verbatim from server/graph.py (_COMMON).
pub const COMMON: &[&str] = &[
    "actualité", "actualités", "actualización", "african", "aggiornamento", "ainda", "aktie",
    "aktien", "aktuelle", "aktuelles", "alte", "amanhã", "american", "an", "analysis", "anlass",
    "anleger", "anni", "anno", "ano", "anos", "ans", "antwort", "apr", "arbeit", "argent",
    "asian", "atualização", "aug", "aujourd", "australian", "ayer", "azienda", "aziende", "año",
    "años", "bambini", "bambino", "bankers", "beitrag", "bericht", "besser", "best", "beste",
    "besten", "boom", "brazilian", "breaking", "british", "builders", "business", "businesses",
    "canadian", "candidates", "ceo", "ceos", "cfo", "children", "chinese", "citizens",
    "comments", "commuters", "companies", "company", "component", "components", "concerns",
    "coo", "court", "criança", "crianças", "crisis", "cto", "danish", "data", "daten", "day",
    "days", "deal", "dec", "demain", "dia", "dias", "dinero", "dinheiro", "doctors", "domani",
    "donne", "drivers", "drop", "dutch", "día", "días", "economists", "egyptian", "election",
    "elections", "empresa", "empresas", "enable", "enfant", "enfants", "entreprise",
    "entreprises", "envie", "error", "erste", "essen", "european", "evening", "exclusive",
    "executive", "executives", "fall", "falsche", "famiglia", "familia", "famille", "family",
    "família", "farmers", "feb", "femmes", "finance", "finnish", "firm", "firma", "firms",
    "first", "frage", "frauen", "french", "fri", "gallery", "geld", "german", "gestern",
    "gesundheit", "giorni", "giorno", "gobierno", "gouvernement", "government", "governo",
    "greek", "group", "growers", "große", "grund", "guerra", "guerre", "gut", "health", "here",
    "heure", "heures", "heute", "hier", "history", "hoje", "hombres", "homens", "hommes",
    "hora", "horas", "hour", "hours", "how", "hoy", "hui", "ieri", "image", "images", "immer",
    "indian", "industries", "industry", "inhalt", "iranian", "israeli", "italian", "jahr",
    "jahre", "jan", "japanese", "javascript", "jetzt", "jour", "jours", "judge", "jul", "jump",
    "jun", "kenyan", "kinder", "kleine", "konzern", "korean", "krankheit", "last", "lawmakers",
    "leader", "leaders", "leben", "legge", "lei", "leia", "letzte", "leute", "ley", "life",
    "live", "loading", "loi", "makers", "mar", "marché", "marchés", "market", "markets",
    "mañana", "men", "menschen", "mercado", "mercados", "mercati", "mercato", "mes", "mese",
    "mexican", "minister", "ministers", "ministre", "ministro", "mois", "mon", "monat",
    "monate", "month", "months", "more", "morgen", "most", "motley", "muitas", "muitos",
    "mujeres", "mulheres", "männer", "märkte", "mês", "möglich", "nan", "neue", "neues", "new",
    "news", "next", "nigerian", "niño", "niños", "noch", "norwegian", "noticia", "noticias",
    "notizia", "notizie", "notícia", "notícias", "nov", "null", "nurses", "nächste", "oct",
    "official", "officials", "oggi", "ontem", "opinion", "ora", "ore", "owners", "pace", "paix",
    "palestinian", "parents", "patients", "paz", "peace", "people", "personas", "persone",
    "personnes", "pessoas", "photo", "photos", "plan", "please", "plunge", "police",
    "policymaker", "policymakers", "policía", "polish", "politicians", "polizia", "polícia",
    "poucas", "poucos", "practice", "practices", "precio", "precios", "prediction",
    "predictions", "preis", "preise", "president", "presidente", "prezzi", "prezzo", "preço",
    "preços", "price", "prices", "prix", "probe", "producers", "präsident", "président", "read",
    "reason", "reasons", "record", "regierung", "regulators", "report", "reports",
    "researchers", "residents", "retailers", "richtige", "rise", "russian", "salud", "salute",
    "santé", "sat", "saúde", "scandal", "schlafen", "schon", "scientists", "sector", "sectors",
    "sehr", "semaine", "semana", "sep", "sept", "settimana", "share", "shares", "shoppers",
    "show", "showing", "skip", "sob", "sobre", "soldi", "spanish", "spokesman", "spokesperson",
    "sport", "stock", "stocks", "students", "studie", "study", "stunde", "stunden", "subscribe",
    "sun", "suppliers", "surge", "swedish", "swiss", "tag", "tage", "taiwanese", "talks",
    "teachers", "tech", "thema", "themen", "thu", "thur", "thurs", "time", "times", "today",
    "tomorrow", "top", "travellers", "treasury", "trending", "trinken", "tue", "tues",
    "turkish", "ukrainian", "undefined", "unternehmen", "uomini", "update", "veja", "vida",
    "video", "vie", "vita", "voters", "war", "warning", "watch", "wed", "week", "weeks", "welt",
    "what", "when", "where", "who", "why", "wichtige", "wieder", "woche", "wochen", "women",
    "world", "worst", "year", "years", "yesterday", "york", "zahlen", "zeit",
];

/// Ported verbatim from server/graph.py (_BLOCK_L).
pub const BLOCK_L: &[&str] = &[
    "about", "according", "accounting", "added", "advertise", "advertisement", "affari",
    "after", "again", "all", "also", "analysis", "analysts", "and", "annual", "any", "anzeige",
    "apr", "are", "aug", "autorità", "autos", "back", "be", "beat", "beats", "been", "before",
    "being", "best", "bigger", "biggest", "blow", "blows", "boom", "borsa", "boss", "bosses",
    "both", "breaking", "brief", "briefing", "briefs", "bring", "brings", "brought", "but",
    "buy", "buyers", "buys", "came", "can", "cent", "chart", "charts", "checks", "come",
    "comes", "concerns", "continued", "continues", "could", "critics", "customers", "cut",
    "cuts", "daily", "day", "days", "dec", "demand", "dept", "dip", "down", "drop", "drops",
    "each", "earnings", "editor", "editorial", "employees", "evening", "exclusive", "expect",
    "expected", "expects", "experts", "face", "faced", "faces", "fall", "falls", "feb",
    "finance", "financing", "fired", "first", "fool", "for", "forecast", "founders", "fri",
    "from", "further", "gain", "gains", "gallery", "gave", "get", "gets", "give", "given",
    "gives", "go", "goes", "got", "graphic", "graphics", "growth", "guidance", "had", "has",
    "have", "he", "her", "here", "his", "history", "hit", "hits", "hold", "holders", "holds",
    "hour", "hours", "how", "huge", "image", "images", "into", "investigated", "investigating",
    "investigation", "investigators", "investor", "investors", "ipo", "is", "it", "its", "jan",
    "javascript", "jul", "jump", "jumps", "jun", "just", "keep", "keeps", "last", "like",
    "live", "loading", "lose", "loses", "lost", "made", "major", "make", "makes", "many", "mar",
    "market", "markets", "massive", "may", "means", "mercato", "might", "migliaia", "minor",
    "minute", "minutes", "miss", "missed", "mon", "month", "monthly", "months", "more",
    "morning", "most", "motley", "much", "must", "new", "not", "notice", "notizie", "nov",
    "now", "null", "observer", "oct", "off", "only", "opinion", "or", "other", "our", "out",
    "outlook", "over", "peers", "per", "percent", "photo", "photos", "plan", "plans", "please",
    "plunge", "plunges", "practice", "practices", "predictions", "price", "prices", "probe",
    "probed", "probing", "profits", "public", "raise", "raised", "raises", "rally", "read",
    "reason", "reasons", "record", "report", "reports", "revenue", "rise", "rises", "rivals",
    "rout", "said", "sales", "sat", "say", "says", "see", "seen", "sees", "sell", "sellers",
    "selloff", "sells", "sep", "sept", "share", "shares", "sharp", "she", "should", "show",
    "showing", "since", "slight", "slump", "some", "source", "sources", "spokespeople", "staff",
    "steady", "still", "stock", "stocks", "stockwatch", "stories", "story", "subscribe",
    "subscribers", "sun", "supply", "surge", "surges", "take", "takes", "tech", "tell", "tells",
    "territory", "test", "tested", "tests", "than", "that", "their", "them", "then", "there",
    "these", "they", "this", "those", "threat", "threats", "thu", "thur", "thurs", "today",
    "told", "tomorrow", "took", "top", "total", "traders", "treasury", "tue", "tues",
    "uncertain", "undefined", "under", "underneath", "up", "update", "updates", "users", "very",
    "video", "videos", "want", "wants", "warn", "warned", "warning", "warns", "was", "watch",
    "we", "wed", "week", "weekly", "weeks", "weiterlesen", "went", "werbung", "were", "what",
    "when", "where", "which", "who", "why", "will", "win", "wins", "with", "won", "workers",
    "would", "writes", "wrote", "year", "years", "yesterday", "yet", "you", "your",
];

// --- the rest of the name vocabulary -------------------------------------
// `link_article()` and `_purge_junk_entities()` need the same word sets
// `is_junk_name()` uses, split the way `server/graph.py` splits them, so
// `_ok_name()` asks the questions it asks and not a different one. Generated
// from the Python module and asserted set-for-set in `db_tests`.

/// Ported verbatim from server/graph.py (_DAYS_MONTHS).
pub const DAYS_MONTHS: &[&str] = &[
    "abril", "agosto", "août", "april", "aprile", "august",
    "avril", "dezember", "dezembro", "dicembre", "diciembre", "dienstag",
    "dimanche", "dinsdag", "domenica", "domingo", "donderdag", "donnerstag",
    "décembre", "enero", "febbraio", "febrero", "februar", "fevereiro",
    "freitag", "friday", "février", "gennaio", "giovedì", "giugno",
    "janeiro", "januar", "janvier", "jeudi", "jueves", "juillet",
    "juin", "julho", "juli", "julio", "junho", "juni",
    "junio", "luglio", "lundi", "lunedì", "lunes", "maandag",
    "maerz", "maggio", "mai", "maio", "mardi", "mars",
    "martedì", "martes", "marzo", "março", "mayo", "mercoledì",
    "mercredi", "mittwoch", "miércoles", "monday", "montag", "märz",
    "november", "novembre", "novembro", "noviembre", "octobre", "octubre",
    "oktober", "ottobre", "outubro", "quarta", "quinta", "sabato",
    "samedi", "samstag", "saturday", "segunda", "september", "septembre",
    "septiembre", "setembro", "settembre", "sexta", "sonnabend", "sonntag",
    "sunday", "sábado", "terça", "thursday", "tuesday", "vendredi",
    "venerdì", "viernes", "vrijdag", "wednesday", "woensdag", "zaterdag",
    "zondag",
];


/// Ported verbatim from server/graph.py (CONNECTORS).
pub const CONNECTORS: &[&str] = &[
    "&", "al", "and", "at", "das", "de",
    "del", "der", "des", "di", "die", "du",
    "el", "en", "for", "in", "la", "le",
    "of", "on", "the", "und", "van", "von",
];

/// Ported verbatim from server/graph.py (_DETERMINERS).
pub const DETERMINERS: &[&str] = &[
    "a", "an", "das", "dem", "den", "der",
    "des", "die", "el", "gli", "il", "la",
    "las", "le", "les", "lo", "los", "the",
    "um", "uma", "un", "une",
];

/// Ported verbatim from server/graph.py (_CONJUNCTIONS).
pub const CONJUNCTIONS: &[&str] = &[
    "&", "and", "oder", "und",
];

/// Ported verbatim from server/graph.py (_CORP_TOKENS).
pub const CORP_TOKENS: &[&str] = &[
    "ag", "co", "company", "corp", "corporation", "gmbh",
    "inc", "limited", "llc", "ltd", "plc", "sa",
];

/// Ported verbatim from server/graph.py (the literal tail of _EDGE_JUNK).
pub const EDGE_JUNK_EXTRA: &[&str] = &[
    "acting", "analyst", "ceo", "ceos", "cfo", "chairman",
    "chairwoman", "coo", "cto", "deputy", "director", "dr",
    "every", "executive", "former", "founder", "if", "interim",
    "mr", "mrs", "ms", "prof", "senior", "sir",
    "to", "vice",
];

/// The alternatives of `_CORP_SUFFIX_RE`, in the regex's own order. Matched
/// case-insensitively against the tail of a name, preceded by `[,\s]+`.
pub const CORP_SUFFIXES: &[&str] = &[
    "inc", "inc.", "corp", "corp.", "corporation", "ltd", "ltd.", "limited", "llc", "l.l.c.",
    "gmbh", "gmbh.", "ag", "a.g.", "sa", "s.a.", "plc", "co", "co.", "company",
];
}

// ---------------------------------------------------------------- payload
// The input is what the database query already returned: plain rows, no SQL,
// no ORM, nothing that has to be re-fetched.

/// A sortable publication time.
///
/// Python orders `published` as SQLite TEXT, so the string is the primary key;
/// the epoch is parsed only for the temporal-proximity signal and stays `None`
/// for anything that is not ISO-8601.
#[derive(Clone, Debug)]
struct Ts {
    text: String,
    num: Option<f64>,
}

impl Ts {
    fn from_json(v: &Json) -> Option<Ts> {
        match v {
            Json::Num(n) if n.is_finite() => Some(Ts {
                text: json::num_str(*n),
                num: Some(*n),
            }),
            Json::Str(s) => Some(Ts {
                text: s.clone(),
                num: None,
            }),
            _ => None,
        }
    }

    fn epoch(&self) -> Option<f64> {
        match self.num {
            Some(n) => Some(n),
            None => parse_epoch(&self.text),
        }
    }
}

fn push_ts(out: &mut String, ts: &Ts) {
    match ts.num {
        Some(n) => out.push_str(&json::num_str(n)),
        None => json::escape_str(out, &ts.text),
    }
}

/// Ascending comparison of two publication times.
fn cmp_ts(a: &Ts, b: &Ts) -> Ordering {
    match (a.num, b.num) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        _ => Ordering::Equal,
    }
    .then_with(|| a.text.cmp(&b.text))
}

/// Descending comparison with `None` last — `ORDER BY published DESC` in SQLite
/// puts NULLs last, so an undated row never becomes the newest member.
fn cmp_ts_desc(a: Option<&Ts>, b: Option<&Ts>) -> Ordering {
    match (a, b) {
        (Some(p), Some(q)) => cmp_ts(q, p),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// One entity as it appears on an article row.
#[derive(Clone, Debug)]
pub(crate) struct EntRef {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) count: f64,
}

impl EntRef {
    fn new(id: u32) -> EntRef {
        EntRef {
            id,
            name: String::new(),
            kind: String::new(),
            count: 0.0,
        }
    }
}

/// One article row plus the ids the database says it is linked to.
#[derive(Clone, Debug)]
pub(crate) struct Article {
    pub(crate) id: u32,
    pub(crate) title: String,
    pub(crate) refs: Vec<EntRef>,
    pub(crate) tags: Vec<String>,
    pub(crate) ts: Option<Ts>,
    pub(crate) lat: Option<f64>,
    pub(crate) lon: Option<f64>,
    pub(crate) relevant: bool,
}

impl Article {
    fn has(&self, eid: u32) -> bool {
        self.refs.iter().any(|r| r.id == eid)
    }
}

fn as_u32(v: &Json) -> Option<u32> {
    let n = v.as_f64()?;
    if !(0.0..=4_294_967_295.0).contains(&n) || n.fract() != 0.0 {
        return None;
    }
    Some(n as u32)
}

fn need_id(obj: &Json, key: &str) -> Result<u32, String> {
    match obj.get(key).and_then(as_u32) {
        Some(v) => Ok(v),
        None => Err(format!(
            "missing or invalid '{}' (expected a non-negative integer)",
            key
        )),
    }
}

fn coord(item: &Json, key: &str, limit: f64) -> Option<f64> {
    let n = item.get(key).and_then(|v| v.as_f64())?;
    if n.is_finite() && n.abs() <= limit {
        Some(n)
    } else {
        None
    }
}

fn parse_refs(item: &Json, types: Option<&Json>) -> Vec<EntRef> {
    let mut out: Vec<EntRef> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    for key in ["entities", "entity_ids"] {
        let arr = match item.get(key).and_then(|v| v.as_arr()) {
            Some(a) => a,
            None => continue,
        };
        for entry in arr {
            let mut r = match entry {
                Json::Num(_) => match as_u32(entry) {
                    Some(id) => EntRef::new(id),
                    None => continue,
                },
                Json::Obj(_) => {
                    let id = match entry.get("id").and_then(as_u32) {
                        Some(v) => v,
                        None => continue,
                    };
                    let mut r = EntRef::new(id);
                    if let Some(n) = entry.get("name").and_then(|v| v.as_str()) {
                        r.name = n.to_string();
                    }
                    if let Some(k) = entry
                        .get("type")
                        .or_else(|| entry.get("kind"))
                        .and_then(|v| v.as_str())
                    {
                        r.kind = k.to_string();
                    }
                    if let Some(c) = entry.get("count").and_then(|v| v.as_f64()) {
                        r.count = c;
                    }
                    r
                }
                _ => continue,
            };
            if r.kind.is_empty() {
                if let Some(t) = types
                    .and_then(|t| t.get(&r.id.to_string()))
                    .and_then(|v| v.as_str())
                {
                    r.kind = t.to_string();
                }
            }
            // A missing kind stays missing: the model decides what a name is, and
            // an untyped entity ranks like `MISC` ([`type_rank`]) without being
            // relabelled as one.
            // `dict.fromkeys()`: the first appearance of an id wins, position kept.
            if seen.insert(r.id) {
                out.push(r);
            }
        }
    }
    out
}

fn parse_tags(item: &Json) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for key in ["tags", "topics"] {
        let arr = match item.get(key).and_then(|v| v.as_arr()) {
            Some(a) => a,
            None => continue,
        };
        for entry in arr {
            let name = match entry {
                Json::Str(s) => s.as_str(),
                Json::Obj(_) => match entry.get("name").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => continue,
                },
                _ => continue,
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            if seen.insert(name.to_string()) {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out
}

fn parse_articles(root: &Json) -> Result<Vec<Article>, String> {
    let items = match root.get("articles") {
        None | Some(Json::Null) => return Ok(Vec::new()),
        Some(Json::Arr(items)) => items,
        Some(_) => return Err("'articles' must be an array of objects".to_string()),
    };
    let types = root.get("entity_types");
    let mut out: Vec<Article> = Vec::with_capacity(items.len());
    for item in items {
        if !matches!(item, Json::Obj(_)) {
            return Err("each article must be a JSON object".to_string());
        }
        let id = match item.get("id").and_then(as_u32) {
            Some(v) => v,
            None => return Err("every article needs a numeric 'id'".to_string()),
        };
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let ts = item
            .get("ts")
            .or_else(|| item.get("published"))
            .and_then(Ts::from_json);
        let relevant = match item.get("relevant") {
            None | Some(Json::Null) => true,
            Some(Json::Bool(b)) => *b,
            Some(v) => v.as_f64().map(|n| n != 0.0).unwrap_or(true),
        };
        out.push(Article {
            id,
            title,
            refs: parse_refs(item, types),
            tags: parse_tags(item),
            ts,
            lat: coord(item, "lat", 90.0),
            lon: coord(item, "lon", 180.0),
            relevant,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------- timestamps

fn take_digits(b: &[u8], i: &mut usize, n: usize) -> Option<i64> {
    if *i + n > b.len() {
        return None;
    }
    let mut v = 0i64;
    for k in 0..n {
        let c = b[*i + k];
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as i64;
    }
    *i += n;
    Some(v)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// ISO-8601 to epoch seconds. Tolerant: a trailing timezone offset is applied,
/// anything unparsable yields `None` (the temporal signal is then simply absent).
fn parse_epoch(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let y = take_digits(b, &mut i, 4)?;
    if b.get(i) != Some(&b'-') {
        return None;
    }
    i += 1;
    let mo = take_digits(b, &mut i, 2)?;
    if b.get(i) != Some(&b'-') {
        return None;
    }
    i += 1;
    let d = take_digits(b, &mut i, 2)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = days_from_civil(y, mo, d) as f64 * 86_400.0;
    if matches!(b.get(i), Some(b'T') | Some(b't') | Some(b' ')) {
        i += 1;
        let hh = take_digits(b, &mut i, 2)?;
        let mut mm = 0i64;
        let mut ss = 0i64;
        if b.get(i) == Some(&b':') {
            i += 1;
            mm = take_digits(b, &mut i, 2)?;
            if b.get(i) == Some(&b':') {
                i += 1;
                ss = take_digits(b, &mut i, 2)?;
                if b.get(i) == Some(&b'.') {
                    i += 1;
                    let start = i;
                    while matches!(b.get(i), Some(c) if c.is_ascii_digit()) {
                        i += 1;
                    }
                    if i == start {
                        return None;
                    }
                }
            }
        }
        secs += (hh * 3600 + mm * 60 + ss) as f64;
        match b.get(i) {
            Some(b'Z') | Some(b'z') => {}
            Some(&sign) if sign == b'+' || sign == b'-' => {
                i += 1;
                let oh = take_digits(b, &mut i, 2)?;
                let mut om = 0i64;
                if b.get(i) == Some(&b':') {
                    i += 1;
                    om = take_digits(b, &mut i, 2)?;
                } else if matches!(b.get(i), Some(c) if c.is_ascii_digit()) {
                    om = take_digits(b, &mut i, 2)?;
                }
                let off = (oh * 3600 + om * 60) as f64;
                secs += if sign == b'+' { -off } else { off };
            }
            _ => {}
        }
    }
    Some(secs)
}

// ---------------------------------------------------------------- geometry

/// Great-circle distance in kilometres.
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0088;
    let p1 = lat1.to_radians();
    let p2 = lat2.to_radians();
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * R * h.clamp(0.0, 1.0).sqrt().asin()
}

/// Distance between the two entities' newest mentions, in seconds.
///
/// A closest-pair gap would always be zero — an edge exists precisely because
/// the two share an article — so proximity is measured on the newest story each
/// side has: entities in the same news cycle sit close together. Both lists are
/// ascending, so the newest mention is the last element.
fn latest_gap_s(a: &[f64], b: &[f64]) -> Option<f64> {
    match (a.last(), b.last()) {
        (Some(x), Some(y)) => Some((x - y).abs()),
        _ => None,
    }
}

// ---------------------------------------------------------------- entity caps

/// `TYPE_RANK` from server/graph.py — unknown types rank as `MISC`.
fn type_rank(kind: &str) -> u8 {
    // Two vocabularies meet here: the extractor's four-letter tags (`PERSON`, `ORG`, `LOC`,
    // `MISC`) and the words the model is asked for (`Person`, `Organization`, `Institution`,
    // `Country`, `City`, `Event`, `Product`, `Other`). A `Country` that fell through to the
    // catch-all rank was counted as a *person* by `keep_ids` and spent the people's room
    // instead of the places' — so the words are mapped to the same ranks, and the case is
    // folded rather than assumed (a model writes `person` as readily as `Person`).
    if kind.eq_ignore_ascii_case("person") {
        return 0;
    }
    if kind.eq_ignore_ascii_case("org")
        || kind.eq_ignore_ascii_case("organization")
        || kind.eq_ignore_ascii_case("institution")
    {
        return 1;
    }
    if kind.eq_ignore_ascii_case("event") {
        return 2;
    }
    if kind.eq_ignore_ascii_case("topic") {
        return 3;
    }
    if kind.eq_ignore_ascii_case("loc")
        || kind.eq_ignore_ascii_case("country")
        || kind.eq_ignore_ascii_case("city")
        || kind.eq_ignore_ascii_case("place")
    {
        return 5;
    }
    4
}

/// `_keep_ids()` — people/organisations first, places only as context, and a
/// per-article cap so a hub entity cannot swallow the whole graph.
pub(crate) fn keep_ids(refs: &[EntRef]) -> Vec<u32> {
    let mut people: Vec<u32> = Vec::new();
    let mut places: Vec<u32> = Vec::new();
    for r in refs {
        if type_rank(&r.kind) >= RANK_LOC {
            places.push(r.id);
        } else {
            people.push(r.id);
        }
    }
    let room = std::cmp::max(
        1,
        MAX_ENTITIES_PER_ARTICLE - places.len().min(MAX_PLACES_PER_ARTICLE),
    );
    let mut keep: Vec<u32> = people.into_iter().take(room).collect();
    keep.extend(places.into_iter().take(MAX_PLACES_PER_ARTICLE));
    // `sorted(keep)` in the Python pair loop: ascending, so every pair is (a<b).
    keep.sort_unstable();
    keep
}

// ---------------------------------------------------------------- edges

#[derive(Default)]
struct Agg {
    /// tag name -> how many of this entity's articles carry it
    tags: HashMap<String, u32>,
    /// publication epochs, ascending
    ts: Vec<f64>,
    lat: f64,
    lon: f64,
    geo: u32,
}

/// Tags both entities' coverage carries, strongest first, then by name.
fn shared_topics(a: Option<&Agg>, b: Option<&Agg>) -> Vec<(String, u32)> {
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        _ => return Vec::new(),
    };
    let mut out: Vec<(String, u32)> = Vec::new();
    for (tag, ca) in &a.tags {
        if let Some(cb) = b.tags.get(tag) {
            out.push((tag.clone(), (*ca).min(*cb)));
        }
    }
    out.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    out
}

/// The sentence the UI shows next to a connection
/// ("38 shared articles · semiconductors, China").
fn reason_sentence(shared: usize, topics: &[String]) -> String {
    if shared == 0 {
        return String::new();
    }
    let mut s = format!(
        "{} shared article{}",
        shared,
        if shared == 1 { "" } else { "s" }
    );
    if !topics.is_empty() {
        s.push_str(" · ");
        s.push_str(&topics.join(", "));
    }
    s
}

/// `edges` — weighted co-occurrence between entities.
///
/// The pair loop is driven by the posting lists *inside* an article: only
/// entities that share an article can become a pair, so the work is
/// `Σ |kept entities per article|²` (≤ 66 per article), never `n²` over all
/// entities. `pairs_considered` reports exactly that number.
///
/// `weight` is the Python weight: the number of articles the two entities share
/// after `_keep_ids()`. The topic, temporal and geographic signals are reported
/// next to it, not folded into it — they are new evidence, and adding them to
/// the weight would silently reorder a graph the Python side already stored.
fn cmd_edges(root: &Json) -> Result<String, String> {
    let arts = parse_articles(root)?;
    let mut aggs: HashMap<u32, Agg> = HashMap::new();
    let mut weights: HashMap<(u32, u32), u32> = HashMap::new();
    let mut pairs_considered: u64 = 0;

    for art in &arts {
        let keep = keep_ids(&art.refs);
        let epoch = art.ts.as_ref().and_then(|t| t.epoch());
        for &eid in &keep {
            let slot = aggs.entry(eid).or_default();
            for tag in &art.tags {
                *slot.tags.entry(tag.clone()).or_insert(0) += 1;
            }
            if let Some(e) = epoch {
                slot.ts.push(e);
            }
            if let (Some(lat), Some(lon)) = (art.lat, art.lon) {
                slot.lat += lat;
                slot.lon += lon;
                slot.geo += 1;
            }
        }
        for i in 0..keep.len() {
            for j in (i + 1)..keep.len() {
                pairs_considered += 1;
                *weights.entry((keep[i], keep[j])).or_insert(0) += 1;
            }
        }
    }
    for slot in aggs.values_mut() {
        slot.ts
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    }

    let mut rows: Vec<((u32, u32), u32)> = weights.into_iter().collect();
    // Deterministic: weight desc, then a asc, then b asc.
    rows.sort_by(|x, y| {
        y.1.cmp(&x.1)
            .then_with(|| x.0 .0.cmp(&y.0 .0))
            .then_with(|| x.0 .1.cmp(&y.0 .1))
    });

    let mut out = String::with_capacity(96 * rows.len() + 160);
    out.push_str("{\"edges\":[");
    for (n, ((a, b), weight)) in rows.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push_str("{\"a\":");
        out.push_str(&a.to_string());
        out.push_str(",\"b\":");
        out.push_str(&b.to_string());
        out.push_str(",\"weight\":");
        out.push_str(&weight.to_string());
        out.push_str(",\"type\":\"cooccur\",\"topics\":[");
        let topics = shared_topics(aggs.get(a), aggs.get(b));
        for (k, (name, _)) in topics.iter().take(MAX_TOPICS).enumerate() {
            if k > 0 {
                out.push(',');
            }
            json::escape_str(&mut out, name);
        }
        out.push_str("],\"gap_s\":");
        let gap = match (aggs.get(a), aggs.get(b)) {
            (Some(x), Some(y)) => latest_gap_s(&x.ts, &y.ts),
            _ => None,
        };
        match gap {
            Some(g) => out.push_str(&format!("{}", g.round() as i64)),
            None => out.push_str("null"),
        }
        out.push_str(",\"km\":");
        match (aggs.get(a), aggs.get(b)) {
            (Some(x), Some(y)) if x.geo > 0 && y.geo > 0 => {
                let km = haversine_km(
                    x.lat / x.geo as f64,
                    x.lon / x.geo as f64,
                    y.lat / y.geo as f64,
                    y.lon / y.geo as f64,
                );
                out.push_str(&json::fmt_score(km));
            }
            _ => out.push_str("null"),
        }
        out.push('}');
    }
    out.push_str("],\"count\":");
    out.push_str(&rows.len().to_string());
    out.push_str(",\"articles\":");
    out.push_str(&arts.len().to_string());
    out.push_str(",\"entities\":");
    out.push_str(&aggs.len().to_string());
    out.push_str(",\"pairs_considered\":");
    out.push_str(&pairs_considered.to_string());
    out.push('}');
    Ok(out)
}

/// `why` — the sentence and the evidence behind one entity pair.
///
/// Membership here is the *uncapped* article→entity list, exactly like
/// `recompute_links()` counts shared entities (`article_entities`), so the count
/// can legitimately be larger than the pair's `edges` weight, which applies
/// `_keep_ids()` first. Ported as-is; the difference is Python's, not this port's.
fn cmd_why(root: &Json) -> Result<String, String> {
    let a = need_id(root, "a")?;
    let b = need_id(root, "b")?;
    let arts = parse_articles(root)?;
    let limit = root
        .get("limit")
        .and_then(|v| v.as_usize())
        .unwrap_or(MAX_EVIDENCE);

    let mut shared: Vec<&Article> = Vec::new();
    let mut tag_counts: HashMap<&str, u32> = HashMap::new();
    if a != b {
        for art in &arts {
            if art.has(a) && art.has(b) {
                shared.push(art);
                for t in &art.tags {
                    *tag_counts.entry(t.as_str()).or_insert(0) += 1;
                }
            }
        }
    }
    // newest first, id desc on a tie — the order `links_for()` shows
    shared.sort_by(|x, y| {
        cmp_ts_desc(x.ts.as_ref(), y.ts.as_ref()).then_with(|| y.id.cmp(&x.id))
    });

    let mut topics: Vec<(String, u32)> = tag_counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    topics.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    let names: Vec<String> = topics
        .iter()
        .take(MAX_TOPICS)
        .map(|(n, _)| n.clone())
        .collect();
    let reason = reason_sentence(shared.len(), &names);

    let mut out = String::with_capacity(160 + 96 * shared.len().min(limit));
    out.push_str("{\"a\":");
    out.push_str(&a.to_string());
    out.push_str(",\"b\":");
    out.push_str(&b.to_string());
    out.push_str(",\"shared\":");
    out.push_str(&shared.len().to_string());
    out.push_str(",\"reason\":");
    json::escape_str(&mut out, &reason);
    out.push_str(",\"topics\":[");
    for (k, name) in names.iter().enumerate() {
        if k > 0 {
            out.push(',');
        }
        json::escape_str(&mut out, name);
    }
    out.push_str("],\"evidence\":[");
    for (k, art) in shared.iter().take(limit).enumerate() {
        if k > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        out.push_str(&art.id.to_string());
        out.push_str(",\"title\":");
        json::escape_str(&mut out, &art.title);
        out.push_str(",\"ts\":");
        match &art.ts {
            Some(ts) => push_ts(&mut out, ts),
            None => out.push_str("null"),
        }
        out.push('}');
    }
    out.push_str("],\"evidence_count\":");
    out.push_str(&shared.len().to_string());
    out.push('}');
    Ok(out)
}

// ---------------------------------------------------------------- threads

/// Disjoint-set with the union rules of `recompute_threads()`: a thread never
/// grows past `max_size`, and the larger set wins (on a tie the first root
/// stays, exactly like Python's `if size[ra] < size[rb]` swap).
pub(crate) struct Uf {
    parent: HashMap<u32, u32>,
    size: HashMap<u32, u32>,
}

impl Uf {
    fn new() -> Uf {
        Uf {
            parent: HashMap::new(),
            size: HashMap::new(),
        }
    }

    /// Every node the union-find knows, in no particular order.
    pub(crate) fn nodes(&self) -> Vec<u32> {
        self.parent.keys().copied().collect()
    }

    fn find(&mut self, x: u32) -> u32 {
        if !self.parent.contains_key(&x) {
            self.parent.insert(x, x);
            self.size.insert(x, 1);
        }
        let mut root = x;
        loop {
            let p = *self.parent.get(&root).unwrap_or(&root);
            if p == root {
                break;
            }
            root = p;
        }
        let mut cur = x;
        while cur != root {
            let next = *self.parent.get(&cur).unwrap_or(&root);
            self.parent.insert(cur, root);
            cur = next;
        }
        root
    }

    fn union(&mut self, a: u32, b: u32, max_size: u32) {
        let mut ra = self.find(a);
        let mut rb = self.find(b);
        if ra == rb {
            return;
        }
        let sa = *self.size.get(&ra).unwrap_or(&1);
        let sb = *self.size.get(&rb).unwrap_or(&1);
        if sa + sb > max_size {
            return;
        }
        if sa < sb {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent.insert(rb, ra);
        self.size.insert(ra, sa + sb);
    }
}

pub(crate) struct ThreadCtx<'a> {
    pub(crate) by_id: HashMap<u32, &'a Article>,
    /// Lower-cased place names (`geodata`) the caller fetched — a shared country
    /// is context, never the story, so places never title a thread.
    pub(crate) places: HashSet<String>,
    pub(crate) lex: &'a Lex,
}

impl<'a> ThreadCtx<'a> {
    /// A context for callers that already hold the rows — `recompute_threads`
    /// reads the members straight out of the database instead of a payload.
    pub(crate) fn new(
        by_id: HashMap<u32, &'a Article>,
        places: HashSet<String>,
        lex: &'a Lex,
    ) -> ThreadCtx<'a> {
        ThreadCtx { by_id, places, lex }
    }
}

/// `_thread_title()` — title a cluster by what its members actually share:
/// names, else shared headline words, else the first usable headline.
///
/// A title must be unique in the thread list, so it is shortened until it is;
/// ported as-is, including the `(%d)` size suffix of the last resort.
fn thread_title(members: &[u32], used: &mut HashSet<String>, ctx: &ThreadCtx) -> String {
    // COUNT(DISTINCT article_id) per entity ... HAVING c>=2
    let mut counts: HashMap<u32, (u32, String, String, f64)> = HashMap::new();
    for &aid in members {
        let art = match ctx.by_id.get(&aid) {
            Some(a) => *a,
            None => continue,
        };
        let mut seen: HashSet<u32> = HashSet::new();
        for r in &art.refs {
            if !seen.insert(r.id) {
                continue;
            }
            match counts.get_mut(&r.id) {
                Some(slot) => slot.0 += 1,
                None => {
                    counts.insert(r.id, (1, r.name.clone(), r.kind.clone(), r.count));
                }
            }
        }
    }
    // `ORDER BY c DESC, en.count DESC LIMIT 24` — SQL leaves ties open, so the
    // name breaks them here and the same rows always give the same title.
    let mut rows: Vec<(u32, f64, String, String)> = counts
        .into_iter()
        .filter(|(_, v)| v.0 >= 2)
        .map(|(_, v)| (v.0, v.3, v.1, v.2))
        .collect();
    rows.sort_by(|x, y| {
        y.0.cmp(&x.0)
            .then_with(|| y.1.partial_cmp(&x.1).unwrap_or(Ordering::Equal))
            .then_with(|| x.2.cmp(&y.2))
    });
    rows.truncate(24);

    let mut cands: Vec<(u32, String)> = Vec::new();
    for (c, _count, name, kind) in rows {
        if name.is_empty() || name.starts_with('#') || ctx.lex.is_junk_name(&name) {
            continue;
        }
        if kind == "LOC" || ctx.places.contains(name.to_lowercase().as_str()) {
            continue;
        }
        cands.push((c, name));
    }
    cands.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));

    let size = members.len() as u32;
    // dominant names first, then the closest thing to a majority
    let need = std::cmp::max(2, (0.3 * size as f64) as u32);
    let mut top: Vec<String> = cands
        .iter()
        .filter(|(c, _)| *c >= need)
        .map(|(_, n)| n.clone())
        .take(3)
        .collect();
    if top.is_empty() {
        top = cands.iter().take(2).map(|(_, n)| n.clone()).collect();
    }
    let mut ids: Vec<u32> = members.to_vec();
    ids.sort_unstable();
    choose_label(
        &top,
        size,
        used,
        || thread_words(members, ctx),
        || {
            ids.iter()
                .filter_map(|aid| ctx.by_id.get(aid).map(|a| (a.id, a.title.clone())))
                .collect()
        },
    )
}

/// The label decision both halves of the layer share: `threads` (rows fetched by
/// Python) and `recompute` (rows read straight out of `article_threads`'s
/// database) must title a cluster the same way, so the rule lives here once.
///
/// Both callbacks are lazy: the word fallback costs a pass over the headlines,
/// and the headline fallback is the last resort — a thread that has names never
/// pays for either.
pub(crate) fn choose_label<W, T>(
    top: &[String],
    size: u32,
    used: &HashSet<String>,
    words: W,
    titles: T,
) -> String
where
    W: FnOnce() -> Vec<String>,
    T: FnOnce() -> Vec<(u32, String)>,
{
    // a title has to be unique in the thread list: shorten it until it is
    for k in (1..=top.len()).rev() {
        let label = top[..k].join(" · ");
        if !used.contains(&label) {
            return label;
        }
    }
    if !top.is_empty() {
        // BUG: ported as-is from server/graph.py `_thread_title`. The suffix is
        // the number of *candidate* members, while `threads.size` ends up as the
        // number of those members that are still relevant — so a title can name
        // a size the thread does not have.
        return format!("{} ({})", top.join(" · "), size);
    }
    let words = words();
    if !words.is_empty() {
        let label = words.join(" · ");
        if !used.contains(&label) {
            return label;
        }
        return format!("{} ({})", label, size);
    }
    let mut titles = titles();
    titles.sort_by(|x, y| x.0.cmp(&y.0));
    for (_, title) in &titles {
        let title = title.trim();
        if title.chars().count() >= 12 && !used.contains(title) {
            return title.chars().take(80).collect();
        }
    }
    String::new()
}

/// Maximal runs of `[a-z]{min_len,}` — `_TITLE4_RE` over a lower-cased title.
fn ascii_word_runs(low: &str, min_len: usize) -> Vec<&str> {
    let b = low.as_bytes();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i].is_ascii_lowercase() {
            let start = i;
            while i < b.len() && b[i].is_ascii_lowercase() {
                i += 1;
            }
            if i - start >= min_len {
                out.push(&low[start..i]);
            }
        } else {
            i += 1;
        }
    }
    out
}

fn capitalize(w: &str) -> String {
    let mut chars = w.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `_thread_words()` — shared headline words as a fallback label.
pub(crate) fn thread_words(members: &[u32], ctx: &ThreadCtx) -> Vec<String> {
    let mut df: HashMap<String, u32> = HashMap::new();
    let mut ids: Vec<u32> = members.to_vec();
    ids.sort_unstable();
    for aid in ids {
        let art = match ctx.by_id.get(&aid) {
            Some(a) => *a,
            None => continue,
        };
        let low = art.title.to_lowercase();
        let mut seen: HashSet<&str> = HashSet::new();
        for w in ascii_word_runs(&low, 4) {
            if ctx.lex.text_stop.contains(w)
                || ctx.lex.stop.contains(w)
                || ctx.lex.common.contains(w)
                || ctx.lex.block.contains(w)
            {
                continue;
            }
            if !seen.insert(w) {
                continue;
            }
            *df.entry(w.to_string()).or_insert(0) += 1;
        }
    }
    let mut cand: Vec<(u32, String)> = df
        .into_iter()
        .filter(|(_, c)| *c >= 2)
        .map(|(w, c)| (c, w))
        .collect();
    cand.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
    cand.into_iter()
        .take(3)
        .map(|(_, w)| capitalize(&w))
        .collect()
}

/// `_COUNT_SUFFIX_RE` = ` \(\d+\)$`.
pub(crate) fn strip_count_suffix(label: &str) -> String {
    if !label.ends_with(')') {
        return label.to_string();
    }
    let open = match label.rfind(" (") {
        Some(k) => k,
        None => return label.to_string(),
    };
    let inner = &label[open + 2..label.len() - 1];
    if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
        label[..open].to_string()
    } else {
        label.to_string()
    }
}

/// `threads` — cluster article links into focused story threads.
fn cmd_threads(root: &Json) -> Result<String, String> {
    let min_score = root
        .get("min_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(DEFAULT_MIN_SCORE);
    let max_size = root
        .get("max_size")
        .and_then(|v| v.as_usize())
        .unwrap_or(DEFAULT_MAX_SIZE)
        .max(1) as u32;

    let lex = Lex::new();
    let arts = parse_articles(root)?;
    let mut by_id: HashMap<u32, &Article> = HashMap::new();
    for a in &arts {
        by_id.entry(a.id).or_insert(a);
    }
    let mut places: HashSet<String> = HashSet::new();
    if let Some(arr) = root.get("places").and_then(|v| v.as_arr()) {
        for p in arr {
            if let Some(s) = p.as_str() {
                let s = s.trim().to_lowercase();
                if !s.is_empty() {
                    places.insert(s);
                }
            }
        }
    }
    let ctx = ThreadCtx {
        by_id,
        places,
        lex: &lex,
    };

    // `SELECT a,b,score FROM article_links WHERE score>=? ORDER BY score DESC, a, b`
    let mut links: Vec<(f64, u32, u32)> = Vec::new();
    match root.get("links") {
        None | Some(Json::Null) => {}
        Some(Json::Arr(rows)) => {
            for row in rows {
                if !matches!(row, Json::Obj(_)) {
                    continue;
                }
                let a = match row.get("a").and_then(as_u32) {
                    Some(v) => v,
                    None => continue,
                };
                let b = match row.get("b").and_then(as_u32) {
                    Some(v) => v,
                    None => continue,
                };
                let score = row.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if !score.is_finite() || score < min_score || a == b {
                    continue;
                }
                links.push((score, a.min(b), a.max(b)));
            }
        }
        Some(_) => return Err("'links' must be an array of objects".to_string()),
    }
    links.sort_by(|x, y| {
        y.0.partial_cmp(&x.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| x.1.cmp(&y.1))
            .then_with(|| x.2.cmp(&y.2))
    });

    let mut uf = Uf::new();
    for (_, a, b) in &links {
        uf.union(*a, *b, max_size);
    }

    // `for node in sorted(parent): groups[find(node)].append(node)`
    let mut nodes: Vec<u32> = uf.parent.keys().copied().collect();
    nodes.sort_unstable();
    let mut groups: HashMap<u32, Vec<u32>> = HashMap::new();
    for node in nodes {
        let root = uf.find(node);
        groups.entry(root).or_default().push(node);
    }
    // deterministic order: biggest thread first, then by first member id
    let mut ordered: Vec<Vec<u32>> = groups.into_values().collect();
    ordered.sort_by(|x, y| y.len().cmp(&x.len()).then_with(|| x[0].cmp(&y[0])));

    let mut used: HashSet<String> = HashSet::new();
    let mut labels: Vec<String> = Vec::new();
    if let Some(arr) = root.get("used").and_then(|v| v.as_arr()) {
        for u in arr {
            if let Some(s) = u.as_str() {
                if used.insert(s.to_string()) {
                    labels.push(s.to_string());
                }
            }
        }
    }

    let mut threads: Vec<(String, Vec<u32>, Option<Ts>)> = Vec::new();
    for members in ordered {
        if members.len() < 2 {
            continue;
        }
        let mut members = members;
        members.sort_unstable();
        members.truncate(max_size as usize);
        let mut label = thread_title(&members, &mut used, &ctx);
        if label.is_empty() {
            continue;
        }
        if used.contains(&label) {
            // same label twice: number it
            let base = strip_count_suffix(&label);
            let mut n = 2usize;
            while used.contains(&label) {
                label = format!("{} ({})", base, n);
                n += 1;
            }
        }
        used.insert(label.clone());
        labels.push(label.clone());
        // BUG: ported as-is. `used.add(label)` happens before the article rows
        // are checked, so a cluster that ends up with fewer than two relevant
        // articles silently disappears *and* keeps its label reserved — the next
        // cluster with the same name gets numbered for no visible reason.
        // `AND relevant=1 ORDER BY published DESC, id DESC`
        let mut picked: Vec<&Article> = members
            .iter()
            .filter_map(|id| ctx.by_id.get(id).copied())
            .filter(|a| a.relevant)
            .collect();
        picked.sort_by(|x, y| {
            cmp_ts_desc(x.ts.as_ref(), y.ts.as_ref()).then_with(|| y.id.cmp(&x.id))
        });
        if picked.len() < 2 {
            continue;
        }
        let updated = picked[0].ts.clone();
        threads.push((label, picked.iter().map(|a| a.id).collect(), updated));
    }

    let mut out = String::with_capacity(160 + 96 * threads.len());
    out.push_str("{\"threads\":[");
    for (n, (label, ids, updated)) in threads.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        let title: String = label.chars().take(200).collect();
        out.push_str("{\"title\":");
        json::escape_str(&mut out, &title);
        out.push_str(",\"size\":");
        out.push_str(&ids.len().to_string());
        out.push_str(",\"updated\":");
        match updated {
            Some(ts) => push_ts(&mut out, ts),
            None => out.push_str("null"),
        }
        out.push_str(",\"articles\":[");
        for (k, id) in ids.iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            out.push_str(&id.to_string());
        }
        out.push_str("]}");
    }
    out.push_str("],\"count\":");
    out.push_str(&threads.len().to_string());
    out.push_str(",\"labels\":[");
    for (n, label) in labels.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        json::escape_str(&mut out, label);
    }
    out.push_str("]}");
    Ok(out)
}

// ---------------------------------------------------------------- neighbours

/// Shared articles and shared tag names for one entity pair.
fn pair_evidence(arts: &[Article], a: u32, b: u32) -> (usize, Vec<String>) {
    if a == b {
        return (0, Vec::new());
    }
    let mut shared = 0usize;
    let mut tags: HashMap<&str, u32> = HashMap::new();
    for art in arts {
        if art.has(a) && art.has(b) {
            shared += 1;
            for t in &art.tags {
                *tags.entry(t.as_str()).or_insert(0) += 1;
            }
        }
    }
    let mut list: Vec<(String, u32)> = tags
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    list.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    (
        shared,
        list.iter().take(MAX_TOPICS).map(|(n, _)| n.clone()).collect(),
    )
}

/// `neighbours` — the top-k related entities of one entity, with a reason.
///
/// This is `bfs_subgraph(depth=1)`'s neighbourhood: the direct edges of one
/// entity, strongest first. Edges are keyed `(a,b,type)` in SQLite, so several
/// rows can describe the same pair; their weights are summed here, which is what
/// the stored `weight=weight+1` upsert would have produced.
fn cmd_neighbours(root: &Json) -> Result<String, String> {
    let center = need_id(root, "id")?;
    let k = root.get("k").and_then(|v| v.as_usize()).unwrap_or(12).max(1);
    let lex = Lex::new();

    let mut weights: HashMap<u32, f64> = HashMap::new();
    match root.get("edges") {
        None | Some(Json::Null) => {}
        Some(Json::Arr(rows)) => {
            for row in rows {
                if !matches!(row, Json::Obj(_)) {
                    continue;
                }
                let a = match row.get("a").and_then(as_u32) {
                    Some(v) => v,
                    None => continue,
                };
                let b = match row.get("b").and_then(as_u32) {
                    Some(v) => v,
                    None => continue,
                };
                if a == b {
                    continue;
                }
                // `weight REAL DEFAULT 1` in the schema
                let w = row.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0);
                if !w.is_finite() {
                    continue;
                }
                if a == center {
                    *weights.entry(b).or_insert(0.0) += w;
                }
                if b == center {
                    *weights.entry(a).or_insert(0.0) += w;
                }
            }
        }
        Some(_) => return Err("'edges' must be an array of objects".to_string()),
    }

    let mut meta: HashMap<u32, (String, String)> = HashMap::new();
    if let Some(rows) = root.get("entities").and_then(|v| v.as_arr()) {
        for row in rows {
            if !matches!(row, Json::Obj(_)) {
                continue;
            }
            let id = match row.get("id").and_then(as_u32) {
                Some(v) => v,
                None => continue,
            };
            let name = row
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let kind = row
                .get("type")
                .or_else(|| row.get("kind"))
                .and_then(|v| v.as_str())
                .unwrap_or("MISC")
                .to_string();
            meta.entry(id).or_insert((name, kind));
        }
    }
    // Only needed for the reason line; the ranking works without it.
    let arts = parse_articles(root)?;

    let mut nb: Vec<(u32, f64)> = weights.into_iter().collect();
    // weight desc, then id asc — the UI never reshuffles
    nb.sort_by(|x, y| {
        y.1.partial_cmp(&x.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| x.0.cmp(&y.0))
    });
    let total = nb.len();
    let keep = nb.iter().take(k);

    let mut out = String::with_capacity(128 + 128 * k);
    out.push_str("{\"center\":");
    out.push_str(&center.to_string());
    out.push_str(",\"neighbours\":[");
    let mut n = 0usize;
    for (id, weight) in keep {
        let (name, kind) = match meta.get(id) {
            Some((name, kind)) => (name.as_str(), kind.as_str()),
            None => ("", "MISC"),
        };
        // `bfs_subgraph` drops junk names from the picture; a node without
        // metadata keeps its place rather than disappearing from the graph.
        if !name.is_empty() && lex.is_junk_name(name) {
            continue;
        }
        let reason = if arts.is_empty() {
            String::new()
        } else {
            let (shared, topics) = pair_evidence(&arts, center, *id);
            reason_sentence(shared, &topics)
        };
        if n > 0 {
            out.push(',');
        }
        n += 1;
        out.push_str("{\"id\":");
        out.push_str(&id.to_string());
        out.push_str(",\"name\":");
        json::escape_str(&mut out, name);
        out.push_str(",\"kind\":");
        json::escape_str(&mut out, kind);
        out.push_str(",\"weight\":");
        out.push_str(&json::fmt_score(*weight));
        out.push_str(",\"reason\":");
        json::escape_str(&mut out, &reason);
        out.push('}');
    }
    out.push_str("],\"count\":");
    out.push_str(&n.to_string());
    out.push_str(",\"total\":");
    out.push_str(&total.to_string());
    out.push('}');
    Ok(out)
}

// ---------------------------------------------------------------- tests
// The cases of tools/graph_check.py, ported onto the pure computation, plus
// determinism, escaping and the posting-list bound.

#[cfg(test)]
mod tests {
    use super::*;

    /// The model's own words and the extractor's tags have to rank the same, or a country
    /// the reader asked the model to name spends the people's room in `keep_ids` and the
    /// graph loses a person to a place nobody meant to draw as one.
    #[test]
    fn the_models_words_rank_like_the_extractors_tags() {
        for (kind, rank) in [
            ("PERSON", 0),
            ("Person", 0),
            ("person", 0),
            ("ORG", 1),
            ("Organization", 1),
            ("institution", 1),
            ("EVENT", 2),
            ("Event", 2),
            ("TOPIC", 3),
            ("LOC", 5),
            ("Country", 5),
            ("City", 5),
            ("place", 5),
            ("Product", 4),
            ("Other", 4),
            ("MISC", 4),
        ] {
            assert_eq!(type_rank(kind), rank, "{kind}");
        }
        assert!(type_rank("Country") >= RANK_LOC, "a country is a place, not a person");
    }

    // -- helpers -----------------------------------------------------------

    fn out(cmd: &str, payload: &str) -> String {
        match run(cmd, payload) {
            Ok(text) => text,
            Err(e) => panic!("{} failed: {}", cmd, e),
        }
    }

    fn out_json(cmd: &str, payload: &str) -> Json {
        json::parse(&out(cmd, payload)).expect("stdout must be valid JSON")
    }

    fn get<'a>(v: &'a Json, key: &str) -> &'a Json {
        v.get(key).unwrap_or_else(|| panic!("missing key '{}'", key))
    }

    fn num(v: &Json) -> f64 {
        v.as_f64().unwrap_or_else(|| panic!("not a number: {:?}", v))
    }

    fn text<'a>(v: &'a Json) -> &'a str {
        v.as_str().unwrap_or_else(|| panic!("not a string: {:?}", v))
    }

    fn list(v: &Json) -> &Vec<Json> {
        v.as_arr().unwrap_or_else(|| panic!("not an array: {:?}", v))
    }

    /// `(a, b, weight)` triples in output order.
    fn edge_pairs(text_out: &str) -> Vec<(u32, u32, u32)> {
        let root = json::parse(text_out).expect("valid json");
        list(get(&root, "edges"))
            .iter()
            .map(|e| {
                (
                    num(get(e, "a")) as u32,
                    num(get(e, "b")) as u32,
                    num(get(e, "weight")) as u32,
                )
            })
            .collect()
    }

    /// The six articles of tools/graph_check.py, with the entity ids the
    /// extractor gives them. Everything the port needs is here; nothing else is.
    const EDGE_ARTICLES: &str = r#"[
      {"id":1,"title":"Nvidia faces SEC accounting investigation",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG"},
                   {"id":2,"name":"Jensen Huang","type":"PERSON"},
                   {"id":3,"name":"SEC","type":"ORG"}],
       "tags":["semiconductors","accounting"],
       "ts":"2026-11-10T09:00:00Z","lat":40.7,"lon":-74.0},
      {"id":2,"title":"Nvidia shares slide as Jensen Huang meets regulators",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG"},
                   {"id":2,"name":"Jensen Huang","type":"PERSON"}],
       "tags":["semiconductors"],
       "ts":"2026-11-10T11:30:00Z","lat":38.9,"lon":-77.0},
      {"id":3,"title":"Anthropic listing fails to calm AI jitter",
       "entities":[{"id":6,"name":"Anthropic","type":"ORG"},
                   {"id":7,"name":"Claude","type":"ORG"}],
       "tags":["artificial intelligence"],
       "ts":"2026-11-11T08:00:00Z","lat":37.7,"lon":-122.4},
      {"id":4,"title":"Brazil coffee harvest hits a record",
       "entities":[{"id":4,"name":"Brazil","type":"LOC"},
                   {"id":5,"name":"Coffee","type":"MISC"},
                   {"id":8,"name":"Minas Gerais","type":"LOC"}],
       "tags":["coffee","harvest"],
       "ts":"2026-11-12T07:15:00Z","lat":-19.9,"lon":-43.9},
      {"id":5,"title":"Brazil central bank holds interest rates",
       "entities":[{"id":4,"name":"Brazil","type":"LOC"}],
       "tags":["interest rates"],
       "ts":"2026-11-12T16:45:00Z","lat":-15.8,"lon":-47.9},
      {"id":6,"title":"Nvidia\u0027s suppliers brace for a slower quarter",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG"}],
       "tags":["semiconductors","suppliers"],
       "ts":"2026-11-13T10:00:00Z","lat":47.6,"lon":-122.3}
    ]"#;

    fn edge_payload() -> String {
        format!("{{\"articles\":{}}}", EDGE_ARTICLES)
    }

    /// The thread fixture: the same thirteen stories, their entity rows and the
    /// article links. The expected titles below are what `graph.recompute_threads()`
    /// produces for exactly these rows (checked against server/graph.py).
    const THREAD_ARTICLES: &str = r#"[
      {"id":1,"title":"Nvidia faces SEC accounting investigation","ts":"2026-11-10T09:00:00Z",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG","count":1},
                   {"id":2,"name":"Jensen Huang","type":"PERSON","count":1},
                   {"id":3,"name":"SEC","type":"ORG","count":1}]},
      {"id":2,"title":"Nvidia shares slide as Jensen Huang meets regulators","ts":"2026-11-10T11:30:00Z",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG","count":1},
                   {"id":2,"name":"Jensen Huang","type":"PERSON","count":1}]},
      {"id":3,"title":"Anthropic listing fails to calm AI jitter","ts":"2026-11-11T08:00:00Z",
       "entities":[{"id":6,"name":"Anthropic","type":"ORG","count":1},
                   {"id":7,"name":"Claude","type":"ORG","count":1}]},
      {"id":4,"title":"Brazil coffee harvest hits a record","ts":"2026-11-12T07:15:00Z",
       "entities":[{"id":4,"name":"Brazil","type":"LOC","count":1},
                   {"id":5,"name":"Coffee","type":"MISC","count":1},
                   {"id":8,"name":"Minas Gerais","type":"LOC","count":1}]},
      {"id":5,"title":"Brazil central bank holds interest rates","ts":"2026-11-12T16:45:00Z",
       "entities":[{"id":4,"name":"Brazil","type":"LOC","count":1}]},
      {"id":6,"title":"Nvidia\u0027s suppliers brace for a slower quarter","ts":"2026-11-13T10:00:00Z",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG","count":1}]},
      {"id":7,"title":"Coffee harvest hits a record in Brazil","ts":"2026-11-14T06:00:00Z","entities":[]},
      {"id":8,"title":"Coffee prices climb before the harvest","ts":"2026-11-14T12:00:00Z","entities":[]},
      {"id":9,"title":"Record harvest expected for coffee growers","ts":"2026-11-15T06:00:00Z","entities":[]},
      {"id":20,"title":"Nvidia guidance worries suppliers","ts":"2026-11-16T06:00:00Z",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG","count":1},
                   {"id":2,"name":"Jensen Huang","type":"PERSON","count":1}]},
      {"id":21,"title":"Jensen Huang warns on Nvidia demand","ts":"2026-11-16T09:00:00Z",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG","count":1},
                   {"id":2,"name":"Jensen Huang","type":"PERSON","count":1}]},
      {"id":22,"title":"Nvidia slips again","ts":"2026-11-17T06:00:00Z",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG","count":1},
                   {"id":2,"name":"Jensen Huang","type":"PERSON","count":1}]},
      {"id":23,"title":"Jensen Huang speaks","ts":"2026-11-17T09:00:00Z",
       "entities":[{"id":1,"name":"Nvidia","type":"ORG","count":1},
                   {"id":2,"name":"Jensen Huang","type":"PERSON","count":1}]}
    ]"#;

    const THREAD_LINKS: &str = r#"[
      {"a":1,"b":2,"score":0.62},{"a":2,"b":6,"score":0.41},{"a":4,"b":5,"score":0.30},
      {"a":7,"b":8,"score":0.35},{"a":8,"b":9,"score":0.33},{"a":3,"b":5,"score":0.10},
      {"a":20,"b":21,"score":0.5},{"a":22,"b":23,"score":0.5}
    ]"#;

    fn thread_payload(extra: &str) -> String {
        format!(
            "{{\"articles\":{},\"links\":{}{}}}",
            THREAD_ARTICLES, THREAD_LINKS, extra
        )
    }

    /// `[(title, size, updated, articles)]` in output order.
    fn threads_of(payload: &str) -> Vec<(String, u32, String, Vec<u32>)> {
        let root = out_json("threads", payload);
        list(get(&root, "threads"))
            .iter()
            .map(|t| {
                (
                    text(get(t, "title")).to_string(),
                    num(get(t, "size")) as u32,
                    match get(t, "updated") {
                        Json::Null => String::new(),
                        v => text(v).to_string(),
                    },
                    list(get(t, "articles"))
                        .iter()
                        .map(|a| num(a) as u32)
                        .collect(),
                )
            })
            .collect()
    }

    // -- 1. entity-name quality (graph_check #1..#3, #6) -------------------
    #[test]
    fn junk_names_are_rejected_by_shape() {
        let lex = Lex::new();
        // `is_junk_name` used to be a 1209-word list plus grammar. The list is gone (it
        // decided which capitalised run could become an entity, and the archive could not
        // show that it had), so what this test pins is the half that needs no dictionary:
        // a timestamp, a page-chrome character, fragment punctuation, a function word.
        //
        // "Read More", "Subscribe", "Advertisement", "Reuters" and "Yahoo Finance" used to
        // be on this list. They are *not* refused at this boundary any more, and that is
        // deliberate: whether a capitalised run is the page's furniture is answered where
        // the text is (`atlas_ingest::entities::extract`'s evidence rule, and `admit` for a
        // name a model offers), not by a build. A publisher the archive mentions is a thing
        // the story names — measured on the archive copy, 99 of 1435 articles carry one in
        // the `publisher` column and 42 name one in the title or summary, and under the old
        // gate none of them could ever be an entity.
        let bad = [
            "AM GMT+1",
            "GMT",
            "Brasil.",
            "12:30",
            "Nvidia | Reuters",
            "the",
            "and",
        ];
        for name in bad {
            assert!(lex.is_junk_name(name), "{:?} must be junk", name);
        }
        for name in [
            "Nvidia",
            "Jensen Huang",
            "Reuters",
            "Read More",
            "Tuesday",
            "Subscribe",
        ] {
            assert!(
                !lex.is_junk_name(name),
                "{name:?} is refused by a list — this boundary is shape only now; page \
                 furniture is the article's evidence to decide"
            );
        }
        assert!(
            !lex.is_junk_name("Durante"),
            "ported as-is: graph.is_junk_name(\"Durante\") is False"
        );
    }

    #[test]
    fn real_names_survive() {
        let lex = Lex::new();
        for name in [
            "Nvidia",
            "Jensen Huang",
            "Brazil",
            "Anthropic",
            "SEC",
            "Minas Gerais",
            "Coffee",
            "Claude",
        ] {
            assert!(!lex.is_junk_name(name), "{:?} is a name", name);
        }
    }

    #[test]
    fn names_need_three_characters() {
        let lex = Lex::new();
        assert!(lex.is_junk_name(""));
        assert!(lex.is_junk_name("ab"));
        assert!(lex.is_junk_name("  "));
        assert!(!lex.is_junk_name("abc"));
    }

    #[test]
    fn sentence_fragments_are_rejected() {
        let lex = Lex::new();
        assert!(lex.is_junk_name("Brasil."), "a trailing period is a fragment");
        assert!(has_fragment_punct("Nvidia Corp."));
        assert!(!has_fragment_punct("U.S."), "an abbreviation is not a fragment");
        assert!(!has_fragment_punct("Direktor Markus"));
    }

    #[test]
    fn time_markers_are_junk() {
        for probe in ["AM GMT+1", "GMT", "12:30", "10.05", "AM PST", "CEST"] {
            assert!(has_time_marker(probe), "{:?} carries a timestamp", probe);
        }
        assert!(!has_time_marker("Nvidia"));
        assert!(has_time_marker("123:45"), "the regex matches on its 3:45");
    }

    #[test]
    fn bad_characters_reject_a_name() {
        let lex = Lex::new();
        assert!(lex.is_junk_name("Nvidia | Reuters"));
        assert!(lex.is_junk_name("Read <More>"));
        assert!(lex.is_junk_name("Nvidia: SEC"));
    }

    // -- 2. per-article cap (graph_check #4, #5, #7, #12) ------------------

    fn refs(spec: &[(u32, &str)]) -> Vec<EntRef> {
        spec.iter()
            .map(|(id, kind)| EntRef {
                id: *id,
                name: format!("E{}", id),
                kind: kind.to_string(),
                count: 1.0,
            })
            .collect()
    }

    #[test]
    fn keep_ids_keeps_people_and_organisations_first() {
        // three places and ten people: places are context, people fill the budget
        let mut spec: Vec<(u32, &str)> = (1..=10).map(|i| (i, "ORG")).collect();
        spec.push((50, "LOC"));
        spec.push((51, "LOC"));
        spec.push((52, "LOC"));
        let keep = keep_ids(&refs(&spec));
        assert_eq!(keep.len(), 12, "9 people + 3 places");
        assert!(keep.contains(&1) && keep.contains(&9));
        assert!(!keep.contains(&10), "the cap keeps the first nine people");
        assert_eq!(keep, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 50, 51, 52]);
    }

    #[test]
    fn keep_ids_caps_places_at_three() {
        let spec: Vec<(u32, &str)> = vec![
            (1, "LOC"),
            (2, "LOC"),
            (3, "LOC"),
            (4, "LOC"),
            (5, "LOC"),
        ];
        assert_eq!(keep_ids(&refs(&spec)), vec![1, 2, 3]);
    }

    #[test]
    fn keep_ids_room_grows_without_places() {
        // no places at all: the whole budget goes to people/organisations
        let spec: Vec<(u32, &str)> = (1..=20).map(|i| (i, "ORG")).collect();
        assert_eq!(keep_ids(&refs(&spec)).len(), 12);
    }

    #[test]
    fn unknown_types_are_not_places() {
        // TYPE_RANK.get(typ, 4) — an unknown type ranks as MISC, not as LOC
        let spec: Vec<(u32, &str)> = vec![(7, "SOMETHING")];
        assert_eq!(keep_ids(&refs(&spec)), vec![7]);
    }

    // -- 3. edges (graph_check #8..#12) -----------------------------------

    #[test]
    fn edges_match_python_weights_and_order() {
        let text_out = out("edges", &edge_payload());
        assert_eq!(
            edge_pairs(&text_out),
            vec![
                (1, 2, 2),
                (1, 3, 1),
                (2, 3, 1),
                (4, 5, 1),
                (4, 8, 1),
                (5, 8, 1),
                (6, 7, 1),
            ],
            "same weights and same ordering as recompute_edges()"
        );
        let root = json::parse(&text_out).unwrap();
        assert_eq!(num(get(&root, "count")), 7.0, "links were built");
        assert_eq!(num(get(&root, "articles")), 6.0);
    }

    #[test]
    fn the_same_story_is_connected() {
        // articles 1 and 2 both carry Nvidia (1) and Jensen Huang (2)
        let root = out_json("edges", &edge_payload());
        let hit = list(get(&root, "edges"))
            .iter()
            .find(|e| num(get(e, "a")) == 1.0 && num(get(e, "b")) == 2.0)
            .expect("edge 1-2");
        assert_eq!(num(get(hit, "weight")), 2.0);
        assert_eq!(text(get(hit, "type")), "cooccur");
    }

    #[test]
    fn a_lone_shared_entity_invents_no_edge() {
        // articles 4 and 5 share only Brazil: one entity cannot pair with itself
        let root = out_json("edges", &edge_payload());
        let mut with_brazil = 0;
        for e in list(get(&root, "edges")) {
            let (a, b) = (num(get(e, "a")) as u32, num(get(e, "b")) as u32);
            assert_ne!(a, b, "no self edges");
            if a == 4 || b == 4 {
                with_brazil += 1;
            }
        }
        assert_eq!(with_brazil, 2, "Brazil pairs only with its co-mentions");
    }

    #[test]
    fn edges_carry_topic_temporal_and_geographic_evidence() {
        let root = out_json("edges", &edge_payload());
        let pair = list(get(&root, "edges"))
            .iter()
            .find(|e| num(get(e, "a")) == 1.0 && num(get(e, "b")) == 2.0)
            .expect("edge 1-2")
            .clone();
        let topics: Vec<&str> = list(get(&pair, "topics")).iter().map(text).collect();
        assert_eq!(topics, vec!["semiconductors", "accounting"]);
        // Nvidia's newest story is 2026-11-13T10:00Z, Jensen Huang's 2026-11-10T11:30Z
        assert_eq!(num(get(&pair, "gap_s")), 253_800.0);
        let km = num(get(&pair, "km"));
        assert!(
            (1200.0..1500.0).contains(&km),
            "the two mean positions are ~1300 km apart, got {}",
            km
        );
    }

    #[test]
    fn edges_cap_the_pairs_per_article() {
        // twenty co-mentioned entities: twelve survive, so 66 pairs, not 190
        let ids: Vec<String> = (1..=20)
            .map(|i| format!("{{\"id\":{},\"type\":\"ORG\"}}", i))
            .collect();
        let payload = format!("{{\"articles\":[{{\"id\":1,\"entities\":[{}]}}]}}", ids.join(","));
        let root = out_json("edges", &payload);
        assert_eq!(num(get(&root, "pairs_considered")), 66.0);
        assert_eq!(num(get(&root, "entities")), 12.0);
    }

    #[test]
    fn edges_without_articles_is_empty() {
        let root = out_json("edges", "{}");
        assert!(list(get(&root, "edges")).is_empty());
        assert_eq!(num(get(&root, "count")), 0.0);
        assert_eq!(num(get(&root, "articles")), 0.0);
        assert_eq!(num(get(&root, "pairs_considered")), 0.0);
    }

    #[test]
    fn edges_reject_a_malformed_payload() {
        assert!(run("edges", "{\"articles\":\"nope\"}").is_err());
        assert!(run("edges", "{\"articles\":[{\"title\":\"no id\"}]}").is_err());
        assert!(run("edges", "not json").is_err());
    }

    #[test]
    fn edges_are_deterministic() {
        let a = out("edges", &edge_payload());
        let b = out("edges", &edge_payload());
        assert_eq!(a, b, "same rows must give byte-identical output");
    }

    #[test]
    fn entity_ids_and_a_type_map_are_enough() {
        let payload = r#"{"articles":[
            {"id":1,"entity_ids":[1,2],"tags":["x"]},
            {"id":2,"entity_ids":[1,2],"tags":["x"]}],
          "entity_types":{"1":"LOC"}}"#;
        let root = out_json("edges", &payload);
        assert_eq!(num(get(&root, "count")), 1.0, "one pair over two articles");
        let pair = &list(get(&root, "edges"))[0];
        assert_eq!(num(get(pair, "weight")), 2.0);
    }

    #[test]
    fn edges_survive_hostile_text() {
        let long = "x".repeat(10_000);
        let payload = format!(
            r#"{{"articles":[
                {{"id":1,"title":"\"quoted\" \n emoji 🚀","entities":[1,2],"tags":["🚀",""]}},
                {{"id":2,"title":"{long}","entities":[1,2],"tags":[]}}]}}"#,
            long = long
        );
        let root = out_json("edges", &payload);
        assert_eq!(num(get(&root, "count")), 1.0);
        let pair = &list(get(&root, "edges"))[0];
        assert_eq!(num(get(pair, "weight")), 2.0);
        assert_eq!(list(get(pair, "topics")).len(), 1, "an empty tag is dropped");
    }

    // -- 4. why (graph_check #24) -----------------------------------------

    #[test]
    fn why_produces_the_sentence_and_the_evidence() {
        let payload = format!("{{\"a\":1,\"b\":2,\"articles\":{}}}", EDGE_ARTICLES);
        let root = out_json("why", &payload);
        assert_eq!(num(get(&root, "shared")), 2.0);
        assert_eq!(
            text(get(&root, "reason")),
            "2 shared articles · semiconductors, accounting"
        );
        let ids: Vec<u32> = list(get(&root, "evidence"))
            .iter()
            .map(|e| num(get(e, "id")) as u32)
            .collect();
        assert_eq!(ids, vec![2, 1], "newest first, id desc on a tie");
        assert_eq!(num(get(&root, "evidence_count")), 2.0);
    }

    #[test]
    fn why_uses_a_singular_article() {
        let payload = format!("{{\"a\":1,\"b\":3,\"articles\":{}}}", EDGE_ARTICLES);
        let root = out_json("why", &payload);
        assert_eq!(num(get(&root, "shared")), 1.0);
        assert_eq!(
            text(get(&root, "reason")),
            "1 shared article · accounting, semiconductors"
        );
    }

    #[test]
    fn why_without_a_shared_article_has_no_sentence() {
        let payload = format!("{{\"a\":3,\"b\":4,\"articles\":{}}}", EDGE_ARTICLES);
        let root = out_json("why", &payload);
        assert_eq!(num(get(&root, "shared")), 0.0);
        assert_eq!(text(get(&root, "reason")), "", "no evidence, no sentence");
        assert!(list(get(&root, "evidence")).is_empty());
    }

    #[test]
    fn why_needs_both_ids() {
        assert!(run("why", "{\"b\":2}").is_err());
        assert!(run("why", "{\"a\":1}").is_err());
        assert!(run("why", "{\"a\":\"x\",\"b\":2}").is_err());
    }

    #[test]
    fn why_is_deterministic() {
        let payload = format!("{{\"a\":1,\"b\":2,\"articles\":{}}}", EDGE_ARTICLES);
        assert_eq!(out("why", &payload), out("why", &payload));
    }

    #[test]
    fn why_escapes_a_hostile_title() {
        let payload = r#"{"a":1,"b":2,"articles":[
            {"id":5,"title":"quote \" and newline \n and 🚀","entities":[1,2],
             "ts":"2026-11-10T09:00:00Z"}]}"#;
        let root = out_json("why", &payload);
        let ev = &list(get(&root, "evidence"))[0];
        let title = text(get(ev, "title"));
        assert!(title.contains("quote \" and newline"), "{}", title);
        assert!(title.contains('🚀'));
    }

    // -- 5. threads (graph_check #13..#20) --------------------------------

    #[test]
    fn threads_match_python_clustering() {
        let got = threads_of(&thread_payload(""));
        let want: Vec<(String, u32, String, Vec<u32>)> = vec![
            (
                "Nvidia · Jensen Huang".to_string(),
                3,
                "2026-11-13T10:00:00Z".to_string(),
                vec![6, 2, 1],
            ),
            (
                "Coffee · Harvest".to_string(),
                3,
                "2026-11-15T06:00:00Z".to_string(),
                vec![9, 8, 7],
            ),
            (
                "Brazil".to_string(),
                2,
                "2026-11-12T16:45:00Z".to_string(),
                vec![5, 4],
            ),
            (
                "Jensen Huang · Nvidia".to_string(),
                2,
                "2026-11-16T09:00:00Z".to_string(),
                vec![21, 20],
            ),
            (
                "Jensen Huang".to_string(),
                2,
                "2026-11-17T09:00:00Z".to_string(),
                vec![23, 22],
            ),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn threads_are_never_single_member() {
        let root = out_json("threads", &thread_payload(""));
        let rows = list(get(&root, "threads"));
        assert!(!rows.is_empty(), "threads were built");
        for t in rows {
            let size = num(get(t, "size")) as usize;
            assert!(size >= 2, "a thread always has at least two members");
            assert!(size <= 40, "the size cap holds");
            assert_eq!(size, list(get(t, "articles")).len(), "size matches its members");
            let title = text(get(t, "title"));
            assert!(!title.trim().is_empty(), "a thread has a title");
            assert!(title.chars().count() <= 200, "titles stay readable");
        }
        assert_eq!(
            num(get(&root, "count")) as usize,
            list(get(&root, "threads")).len(),
            "the count matches the list"
        );
    }

    #[test]
    fn threads_have_readable_titles() {
        for (title, ..) in threads_of(&thread_payload("")) {
            let low = title.to_lowercase();
            for bad in [
                "read more",
                "advertisement",
                "subscribe",
                "reuters",
                "yahoo finance",
                "motley fool",
                "undefined",
                "dienstag",
                "märz",
            ] {
                assert!(!low.contains(bad), "title {:?} carries {:?}", title, bad);
            }
            assert!(!title.contains('?'), "no placeholder in {:?}", title);
        }
    }

    #[test]
    fn a_thread_is_titled_by_what_it_shares() {
        let titles: Vec<String> = threads_of(&thread_payload(""))
            .into_iter()
            .map(|t| t.0.to_lowercase())
            .collect();
        assert!(
            titles.iter().any(|t| t.contains("nvidia")),
            "one thread names the shared company: {:?}",
            titles
        );
        assert!(
            titles
                .iter()
                .any(|t| t.contains("coffee") && t.contains("harvest")),
            "headline words title the entity-less thread: {:?}",
            titles
        );
    }

    #[test]
    fn threads_respect_the_size_cap() {
        let got = threads_of(&thread_payload(",\"max_size\":2"));
        assert!(!got.is_empty());
        for (_, size, _, ids) in &got {
            assert!(*size <= 2, "the cap is respected");
            assert_eq!(*size as usize, ids.len());
        }
    }

    #[test]
    fn threads_ignore_links_below_the_threshold() {
        let got = threads_of(&thread_payload(""));
        assert!(
            !got.iter().any(|t| t.3.contains(&3)),
            "article 3 sits below min_score and stays alone: {:?}",
            got
        );
        let strict = threads_of(&thread_payload(",\"min_score\":0.5"));
        assert!(strict.len() < got.len(), "{:?}", strict);
    }

    #[test]
    fn threads_shorten_a_label_that_is_already_used() {
        // {20,21} and {22,23} share the same two names: the second one shortens
        let titles: Vec<String> = threads_of(&thread_payload(""))
            .into_iter()
            .map(|t| t.0)
            .collect();
        assert_eq!(
            titles.iter().filter(|t| *t == "Jensen Huang · Nvidia").count(),
            1
        );
        assert_eq!(titles.iter().filter(|t| *t == "Jensen Huang").count(), 1);
    }

    #[test]
    fn threads_never_title_a_thread_with_a_place() {
        let payload = r#"{"places":["brasília"],
          "links":[{"a":1,"b":2,"score":0.4}],
          "articles":[
            {"id":1,"title":"One quiet morning in the capital","ts":"2026-11-10T09:00:00Z",
             "entities":[{"id":9,"name":"Brasília","type":"MISC","count":1}]},
            {"id":2,"title":"Another quiet morning downtown","ts":"2026-11-10T10:00:00Z",
             "entities":[{"id":9,"name":"Brasília","type":"MISC","count":1}]}]}"#;
        let got = threads_of(payload);
        assert_eq!(got.len(), 1);
        assert_ne!(got[0].0, "Brasília", "a place never titles a thread");
    }

    #[test]
    fn threads_are_deterministic() {
        let payload = thread_payload("");
        assert_eq!(out("threads", &payload), out("threads", &payload));
    }

    #[test]
    fn threads_without_links_build_nothing() {
        let root = out_json("threads", &format!("{{\"articles\":{}}}", THREAD_ARTICLES));
        assert_eq!(num(get(&root, "count")), 0.0);
        assert!(list(get(&root, "threads")).is_empty());
    }

    #[test]
    fn threads_reject_a_malformed_payload() {
        assert!(run("threads", "{\"links\":\"nope\"}").is_err());
        assert!(run("threads", "{\"links\":[{\"a\":1,\"b\":2,\"score\":0.9}],\"articles\":5}").is_err());
    }

    #[test]
    fn threads_cap_a_hostile_title() {
        let long = "y".repeat(10_000);
        let payload = format!(
            r#"{{"links":[{{"a":1,"b":2,"score":0.5}}],
                "articles":[
                  {{"id":1,"title":"{long}","ts":"2026-11-10T09:00:00Z","entities":[]}},
                  {{"id":2,"title":"🚀 launch \"again\"","ts":"2026-11-10T10:00:00Z","entities":[]}}]}}"#,
            long = long
        );
        let got = threads_of(&payload);
        assert_eq!(got.len(), 1);
        assert!(
            got[0].0.chars().count() <= 200,
            "the title is cut to 200 characters"
        );
        // and the whole answer still round-trips through the parser
        let _ = out_json("threads", &payload);
    }

    // -- 6. neighbours (graph_check #23, #25, #26) -------------------------

    const NEIGHBOUR_ENTITIES: &str = r#"[
      {"id":1,"name":"Nvidia","type":"ORG","count":12},
      {"id":2,"name":"Jensen Huang","type":"PERSON","count":9},
      {"id":3,"name":"SEC","type":"ORG","count":3},
      {"id":6,"name":"Anthropic","type":"ORG","count":4},
      {"id":7,"name":"Brasil.","type":"MISC","count":80}
    ]"#;

    fn neighbour_payload() -> String {
        format!(
            r#"{{"id":1,"entities":{ents},"articles":{arts},
                 "edges":[{{"a":1,"b":2,"weight":2}},{{"a":1,"b":3,"weight":1}},
                          {{"a":1,"b":6,"weight":1}},{{"a":1,"b":2,"weight":3}},
                          {{"a":2,"b":7,"weight":9}}]}}"#,
            ents = NEIGHBOUR_ENTITIES,
            arts = EDGE_ARTICLES
        )
    }

    fn neighbour_ids(root: &Json) -> Vec<u32> {
        list(get(root, "neighbours"))
            .iter()
            .map(|n| num(get(n, "id")) as u32)
            .collect()
    }

    #[test]
    fn neighbours_rank_by_weight_then_id() {
        let root = out_json("neighbours", &neighbour_payload());
        assert_eq!(neighbour_ids(&root), vec![2, 3, 6], "weight desc, id asc");
        let first = &list(get(&root, "neighbours"))[0];
        assert_eq!(
            num(get(first, "weight")),
            5.0,
            "two rows for the same pair are summed"
        );
        assert_eq!(text(get(first, "name")), "Jensen Huang");
        assert_eq!(text(get(first, "kind")), "PERSON");
        assert_eq!(
            text(get(first, "reason")),
            "2 shared articles · semiconductors, accounting"
        );
        assert_eq!(num(get(&root, "count")), 3.0);
        assert_eq!(num(get(&root, "total")), 3.0);
    }

    #[test]
    fn neighbours_drop_junk_names() {
        // entity 7 is "Brasil." — fragment punctuation, so the graph view never shows it.
        // (It was "Read More" while a 1209-word list decided names; that list is gone and
        // the shape rules are what is left, so the fixture uses a shape they refuse.)
        let payload = format!(
            r#"{{"id":2,"entities":{},"edges":[{{"a":2,"b":7,"weight":9}},
                                              {{"a":2,"b":1,"weight":1}}]}}"#,
            NEIGHBOUR_ENTITIES
        );
        let root = out_json("neighbours", &payload);
        assert_eq!(neighbour_ids(&root), vec![1]);
        assert_eq!(num(get(&root, "total")), 2.0, "the drop happens at render time");
    }

    #[test]
    fn neighbours_respect_k() {
        let payload = format!(
            r#"{{"id":1,"k":1,"entities":{},"edges":[{{"a":1,"b":2,"weight":2}},
                                                    {{"a":1,"b":3,"weight":1}}]}}"#,
            NEIGHBOUR_ENTITIES
        );
        let root = out_json("neighbours", &payload);
        assert_eq!(list(get(&root, "neighbours")).len(), 1);
        assert_eq!(num(get(&root, "total")), 2.0);
    }

    #[test]
    fn neighbours_work_without_article_rows() {
        let payload = format!(
            r#"{{"id":1,"entities":{},"edges":[{{"a":1,"b":6,"weight":2}}]}}"#,
            NEIGHBOUR_ENTITIES
        );
        let root = out_json("neighbours", &payload);
        let first = &list(get(&root, "neighbours"))[0];
        assert_eq!(text(get(first, "name")), "Anthropic");
        assert_eq!(text(get(first, "reason")), "", "no rows, no sentence");
    }

    #[test]
    fn neighbours_need_an_id_and_tolerate_junk_rows() {
        assert!(run("neighbours", "{}").is_err());
        let payload = r#"{"id":1,
            "entities":[{"id":2,"name":"🚀 \"Ünïcode\"","type":"ORG"}],
            "edges":[{"a":1,"b":2,"weight":1},{"a":"x","b":9,"weight":1},
                     {"a":1,"b":1,"weight":99}]}"#;
        let root = out_json("neighbours", &payload);
        let rows = list(get(&root, "neighbours"));
        assert_eq!(rows.len(), 1, "malformed rows and self edges are ignored");
        assert_eq!(text(get(&rows[0], "name")), "🚀 \"Ünïcode\"");
    }

    #[test]
    fn neighbours_are_deterministic() {
        let payload = neighbour_payload();
        assert_eq!(out("neighbours", &payload), out("neighbours", &payload));
    }

    // -- 7. the pair loop is bounded by the posting lists ------------------

    #[test]
    fn a_4000_entity_graph_never_builds_all_pairs() {
        // 400 articles x 12 entity ids drawn from 4000 entities. The naive scan
        // would be 4000*3999/2 = 7,998,000 pairs; the posting lists inside an
        // article give at most 400 * 66 = 26,400.
        let mut arts: Vec<String> = Vec::with_capacity(400);
        let mut expected: HashSet<(u32, u32)> = HashSet::new();
        for i in 0..400u32 {
            let mut ids: Vec<u32> = (0..12u32).map(|k| (i * 12 + k) % 4000).collect();
            ids.sort_unstable();
            for x in 0..ids.len() {
                for y in (x + 1)..ids.len() {
                    expected.insert((ids[x], ids[y]));
                }
            }
            let refs: Vec<String> = ids
                .iter()
                .map(|id| format!("{{\"id\":{},\"type\":\"ORG\"}}", id))
                .collect();
            arts.push(format!(
                "{{\"id\":{},\"title\":\"story {}\",\"entities\":[{}],\"tags\":[\"t{}\"],\
                 \"ts\":\"2026-11-10T09:00:00Z\"}}",
                i + 1,
                i,
                refs.join(","),
                i % 7
            ));
        }
        let payload = format!("{{\"articles\":[{}]}}", arts.join(","));

        let started = std::time::Instant::now();
        let text_out = out("edges", &payload);
        let elapsed = started.elapsed();

        let root = json::parse(&text_out).expect("valid json");
        let considered = num(get(&root, "pairs_considered")) as u64;
        assert_eq!(considered, 400 * 66, "only within-article pairs are visited");
        assert!(
            considered < 7_998_000 / 10,
            "far below the naive pair count, got {}",
            considered
        );
        assert_eq!(
            num(get(&root, "count")) as usize,
            expected.len(),
            "every co-mention is reported and none is invented"
        );
        assert!(num(get(&root, "entities")) <= 4000.0);
        assert!(
            elapsed.as_millis() < 5_000,
            "400 articles must stay trivial, took {:?}",
            elapsed
        );
        println!(
            "edges: 400 articles / 4000 entities -> {} edges from {} pair visits in {:?}",
            expected.len(),
            considered,
            elapsed
        );
        assert_eq!(text_out, out("edges", &payload), "stable at scale");
    }

    // -- 8. wire format ----------------------------------------------------

    #[test]
    fn every_subcommand_answers_one_json_object() {
        for (cmd, payload) in [
            ("edges", edge_payload()),
            ("why", format!("{{\"a\":1,\"b\":2,\"articles\":{}}}", EDGE_ARTICLES)),
            ("threads", thread_payload("")),
            ("neighbours", neighbour_payload()),
        ] {
            let text_out = out(&cmd, &payload);
            assert!(text_out.starts_with('{') && text_out.ends_with('}'));
            assert!(matches!(json::parse(&text_out), Ok(Json::Obj(_))));
            assert!(!text_out.contains('\n'), "one line on stdout");
        }
    }

    #[test]
    fn an_unknown_subcommand_is_an_error() {
        assert!(run("nope", "{}").is_err());
        assert!(run("edges", "[]").is_err(), "the input must be an object");
    }
}
