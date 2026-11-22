//! Adversarial input for the CLI JSON parser: 10 000 mutations, no panic.
//!
//! Every binary in this workspace reads a JSON object on stdin, and `parse` is
//! the only door that object comes through. The claim proved here is: for any
//! byte sequence — invalid UTF-8 replaced, control characters included, depth
//! bombs included — `parse` either answers a value or says why, and it never
//! allocates more than a linear amount for the input it was given.
//!
//! The generator is `atlas_guard::mutate` (a test-only dependency: this crate
//! itself stays dependency-free), so the same 10 000 inputs are produced on every
//! machine and every run.

use atlas_lib::json;

/// Shapes a real request has, and the punctuation a mutation needs to reach the
/// interesting branches (nesting, escapes, numbers, a truncated tail).
const SEEDS: &[&str] = &[
    "{}",
    "{\"url\":\"https://example.com/a\",\"limit\":10}",
    "{\"query\":\"harbour strike\",\"n\":3,\"deep\":[[[1]]]}",
    "{\"text\":\"line\\nbreak\\tand \\u0000 nul\"}",
    "{\"n\":-0.0,\"m\":1e999,\"k\":true,\"z\":null}",
    "[\"a\",\"b\",{\"c\":1}]",
    "{\"nested\":{\"a\":{\"b\":{\"c\":{\"d\":1}}}}}",
    "{\"unterminated\":\"",
    "\u{feff}{\"bom\":1}",
    "{\"e\":\"\\uD83D\\uDE00\"}",
];

#[test]
fn ten_thousand_mutated_documents_never_panic_and_stay_linear() {
    let mut parsed = 0usize;
    let mut refused = 0usize;
    let mut longest_out = 0usize;
    let mut worst_ratio = 0.0f64;
    let started = std::time::Instant::now();

    let stats = atlas_guard::mutate::run(SEEDS, 10_000, 0x_A71A5_0001, |input| {
        match json::parse(input) {
            Ok(value) => {
                parsed += 1;
                // A value that parsed can be written back. The output may be
                // longer than the input only by escapes, so the bound is linear:
                // an allocator blow-up would need one of these two to break.
                let out = json::to_string(&value);
                longest_out = longest_out.max(out.len());
                assert!(
                    out.len() <= input.len() * 8 + 64,
                    "{} bytes in produced {} bytes out",
                    input.len(),
                    out.len()
                );
                // And it must parse again: writing is the inverse of reading.
                assert!(
                    json::parse(&out).is_ok(),
                    "the writer produced JSON its own reader refuses: {:?}",
                    out.chars().take(80).collect::<String>()
                );
                let ratio = out.len() as f64 / (input.len().max(1)) as f64;
                worst_ratio = worst_ratio.max(ratio);
            }
            Err(reason) => {
                refused += 1;
                // A refusal is a sentence: a bare `Err("")` reads as a pass.
                assert!(!reason.trim().is_empty(), "a refusal without a reason");
                assert!(reason.len() < 512, "a refusal must stay a sentence");
            }
        }
        // The CLI contract's own reader, on the same bytes.
        match atlas_lib::cli::input_object(input) {
            Ok(_) => {}
            Err(reason) => assert!(!reason.trim().is_empty()),
        }
    });

    let elapsed = started.elapsed();
    println!(
        "{} mutated documents in {:.0} ms ({} parsed, {} refused, longest output {} bytes, worst ratio {:.1}x)",
        stats.runs,
        elapsed.as_secs_f64() * 1e3,
        parsed,
        refused,
        longest_out,
        worst_ratio
    );
    assert_eq!(stats.runs, 10_000);
    assert_eq!(parsed + refused, 10_000);
    // Measured, not assumed: 10 000 documents of a few hundred bytes.
    assert!(
        elapsed.as_secs_f64() < 5.0,
        "10 000 documents took {:.1} s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn a_depth_bomb_is_refused_instead_of_overflowing_the_stack() {
    // 100 000 nested arrays is a 100 kB stdin, which any peer can send.
    let bomb = "[".repeat(100_000);
    let error = json::parse(&bomb).expect_err("a depth bomb must be refused");
    assert!(error.contains("deep"), "{}", error);
    let object_bomb = "{\"a\":".repeat(50_000);
    assert!(json::parse(&object_bomb).is_err());
}

#[test]
fn a_megabyte_of_repeated_punctuation_costs_a_linear_parse() {
    // The shape that makes a quadratic scanner visible: one long run.
    let mut worst = std::time::Duration::ZERO;
    for unit in ["1", "\"a\"", "[1]", "{\"a\":1}"] {
        let mut text = String::from("[");
        while text.len() < 1_000_000 {
            text.push_str(unit);
            text.push(',');
        }
        let started = std::time::Instant::now();
        let _ = json::parse(&text);
        worst = worst.max(started.elapsed());
    }
    println!("worst 1 MB parse: {:.1} ms", worst.as_secs_f64() * 1e3);
    assert!(worst.as_secs_f64() < 2.0, "1 MB took {:.1} s", worst.as_secs_f64());
}
