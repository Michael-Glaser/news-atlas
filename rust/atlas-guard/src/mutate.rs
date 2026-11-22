//! `mutate` — deterministic mutated input, so a fuzz test is a test.
//!
//! The rule is that model output, page text and CLI stdin are untrusted, and the
//! proof is a test that feeds a parser ten thousand hostile strings and asserts
//! it neither panics nor grows without bound. A test that cannot be re-run to the
//! byte is not a proof, and a random crate is a dependency this workspace does not
//! need — so the generator is here: an xorshift64* stream, a fixed corpus supplied
//! by the caller, and eight mutation operators that work on **bytes** before the
//! result is turned back into text (a parser must survive invalid UTF-8, since a
//! socket can hand it any octet).
//!
//! What it decides: which hostile input a test sees, and in which order. What it
//! does not: whether the parser is correct — the target asserts that, and neither
//! a panic nor a bound is swallowed here.
//!
//! ```ignore
//! let stats = mutate::run(&SEEDS, 10_000, 0xA71A5, |input| { parse(input); });
//! assert_eq!(stats.runs, 10_000);
//! ```

/// Fragments a real attacker's input is made of: markup, entities, quotes,
/// control characters, bidi overrides, a lone NUL, JSON punctuation, an
/// oversized repeat, a very long token.
pub const HOSTILE: &[&[u8]] = &[
    b"<script>alert(1)</script>",
    b"<html><body><p>",
    b"</p></body></html>",
    b"<!--",
    b"-->",
    b"<!DOCTYPE html>",
    b"<![CDATA[",
    b"]]>",
    b"&amp;",
    b"&notanentity;",
    b"&#x1F600;",
    b"&#999999999999;",
    b"&",
    b";",
    b"\"",
    b"'",
    b"\\",
    b"\\u0000",
    b"\\\\",
    b"{",
    b"}",
    b"[",
    b"]",
    b":",
    b",",
    b"\"data\":",
    b"\"arguments\":",
    b"null",
    b"true",
    b"-0.0",
    b"1e999",
    b"NaN",
    b"data: ",
    b"\n\n",
    b"\r\n\r\n",
    b"event: ",
    b"id: 1",
    b"\0",
    b"\x01\x02\x03",
    b"\x7f",
    "\u{202e}".as_bytes(),  // right-to-left override
    "\u{200b}".as_bytes(),  // zero width space
    "\u{feff}".as_bytes(),  // BOM
    b"\xff\xfe",          // invalid UTF-8
    b"\xc3",              // a truncated two-byte sequence
    b"http://",
    b"https://",
    b"javascript:",
    b"//",
    b"[::1]",
    b"127.0.0.1",
    b"@",
    b"%00",
    b"%0a",
    b"../",
    b"..\\",
    // a long token without a separator
    b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    // nested markup
    b"<div><div><div><div><div><div><div><div><div><div><div><div><div>",
    b"<div><div><div><div><div><div><div><div><div><div><div><div><div>",
    b"<div><div><div><div><div><div><div><div>",
];

/// What one [`run`] did. Counts, so a test can assert a bound and a report can
/// print a number instead of an adjective.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Inputs handed to the target.
    pub runs: usize,
    /// The longest input that was produced, in bytes.
    pub max_input: usize,
    /// Total bytes handed to the target — the cost the parser paid.
    pub total_input: usize,
}

/// xorshift64*, seeded: same stream on every machine and every run.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// A generator with a non-zero seed (`0` is nudged, a zero state is fixed).
    pub fn new(seed: u64) -> Rng {
        Rng {
            state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
        }
    }

    /// The next value of the stream.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..bound` (`bound == 0` gives 0).
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }

    /// A byte from the whole 0..=255 range, invalid UTF-8 included.
    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 33) as u8
    }

    /// True with probability `1 / n`.
    pub fn one_in(&mut self, n: usize) -> bool {
        n > 0 && self.below(n) == 0
    }
}

/// One mutation of `seed`, appended to `out`.
///
/// Operators, chosen by the stream: flip a bit, overwrite a byte, insert a
/// hostile fragment, delete a span, duplicate a span, splice another corpus seed,
/// truncate, or triple a character run (the shape that makes a naive parser
/// allocate).
pub fn mutate(rng: &mut Rng, corpus: &[&str], seed: &str, out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(seed.as_bytes());
    match rng.below(8) {
        0 => {
            if !out.is_empty() {
                let at = rng.below(out.len());
                out[at] ^= 1 << rng.below(8);
            }
        }
        1 => {
            if !out.is_empty() {
                let at = rng.below(out.len());
                out[at] = rng.byte();
            }
        }
        2 => {
            let fragment = HOSTILE[rng.below(HOSTILE.len())];
            let at = rng.below(out.len() + 1);
            out.splice(at..at, fragment.iter().copied());
        }
        3 => {
            if out.len() > 1 {
                let at = rng.below(out.len());
                let len = 1 + rng.below((out.len() - at).min(32));
                out.drain(at..at + len);
            }
        }
        4 => {
            if !out.is_empty() {
                let at = rng.below(out.len());
                let len = 1 + rng.below((out.len() - at).min(32));
                let copy: Vec<u8> = out[at..at + len].to_vec();
                let to = rng.below(out.len() + 1);
                out.splice(to..to, copy);
            }
        }
        5 => {
            if !corpus.is_empty() {
                let other = corpus[rng.below(corpus.len())].as_bytes();
                let at = rng.below(out.len() + 1);
                let take = 1 + rng.below(other.len().max(1));
                let slice = &other[..take.min(other.len())];
                out.splice(at..at, slice.iter().copied());
            }
        }
        6 => {
            if !out.is_empty() {
                let keep = rng.below(out.len());
                out.truncate(keep);
            }
        }
        _ => {
            if !out.is_empty() {
                let at = rng.below(out.len());
                let byte = out[at];
                out.splice(at..at, std::iter::repeat(byte).take(1 + rng.below(64)));
            }
        }
    }
    // A run of hostile fragments back to back: what a nested-markup bomb looks
    // like, and what a "many events in one line" SSE stream looks like.
    if rng.one_in(16) {
        let fragment = HOSTILE[rng.below(HOSTILE.len())];
        let times = 1 + rng.below(8);
        for _ in 0..times {
            out.extend_from_slice(fragment);
        }
    }
}

/// One mutated input as text. Invalid UTF-8 is replaced, not dropped: a parser
/// that reads a socket sees those bytes too.
pub fn mutated_text(rng: &mut Rng, corpus: &[&str], seed: &str) -> String {
    let mut bytes = Vec::new();
    mutate(rng, corpus, seed, &mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Run `target` on `rounds` mutated inputs, derived from `corpus`.
///
/// The whole point is what is *not* here: no `catch_unwind`, no swallowing. A
/// panic in `target` fails the test that called this, which is the assertion.
pub fn run<F: FnMut(&str)>(corpus: &[&str], rounds: usize, seed: u64, mut target: F) -> Stats {
    if corpus.is_empty() {
        return Stats::default();
    }
    let mut rng = Rng::new(seed);
    let mut stats = Stats::default();
    for round in 0..rounds {
        // Every corpus entry once, in order, then mutations of all of them: a
        // target that only survives mutations of one seed is not covered.
        let base = if round < corpus.len() {
            corpus[round]
        } else {
            corpus[rng.below(corpus.len())]
        };
        let text = mutated_text(&mut rng, corpus, base);
        stats.runs += 1;
        stats.max_input = stats.max_input.max(text.len());
        stats.total_input += text.len();
        target(&text);
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Rng::new(42).next_u64(), Rng::new(43).next_u64());
        // A zero seed is nudged rather than freezing the generator.
        assert_ne!(Rng::new(0).next_u64(), 0);
    }

    #[test]
    fn every_rng_value_stays_inside_its_range() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            assert!(rng.below(7) < 7);
            assert_eq!(rng.below(0), 0);
        }
    }

    #[test]
    fn a_mutation_keeps_the_seed_or_changes_it_like_a_mutation() {
        let corpus = ["hello world", "{\"a\":1}"];
        let mut rng = Rng::new(1);
        let mut changed = 0;
        for _ in 0..1000 {
            let text = mutated_text(&mut rng, &corpus, "hello world");
            if text != "hello world" {
                changed += 1;
            }
        }
        // Not every operator changes the text (a bit flip on a byte may be a
        // no-op after lossy conversion), but most must.
        assert!(changed > 500, "only {} of 1000 changed", changed);
    }

    #[test]
    fn a_mutation_is_bounded_by_the_operators_it_uses() {
        let corpus = ["x"];
        let mut rng = Rng::new(99);
        let mut longest = 0;
        for _ in 0..2000 {
            let text = mutated_text(&mut rng, &corpus, "x");
            longest = longest.max(text.len());
        }
        // 1 seed byte + one 64-byte hostile fragment + up to 8 more = well under
        // 1 kB. An unbounded generator would make the tests unmeasurable.
        assert!(longest < 1024, "longest mutation was {} bytes", longest);
    }

    #[test]
    fn run_hands_every_corpus_entry_to_the_target() {
        let corpus = ["a", "b", "c"];
        let mut seen = Vec::new();
        let stats = run(&corpus, 3, 5, |text| seen.push(text.to_string()));
        assert_eq!(stats.runs, 3);
        assert_eq!(seen.len(), 3);
        assert!(stats.total_input > 0);
    }
}
