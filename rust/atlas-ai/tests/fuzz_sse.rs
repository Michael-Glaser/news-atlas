//! Adversarial input for the SSE parser: 10 000 mutated streams, no panic.
//!
//! `/api/chat` relays the local model's server-sent events, so this parser reads
//! whatever the model process emits — a peer that is one bug away from sending
//! anything. The claim: `feed`/`finish` never panic, never grow the internal
//! buffer past the documented line cap, and every event they emit carries bounded
//! text. A stream that ends mid-frame is a `truncated: true`, not a hang.

use atlas_ai::sse::{SseEvent, SseParser, MAX_LINE_BYTES};

const SEEDS: &[&str] = &[
    "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
    "data: [DONE]\n\n",
    "event: message\ndata: one\ndata: two\n\n",
    ": keep-alive comment\n\ndata: x\n\n",
    "data: {\"unterminated\":\n",
    "\r\n\r\ndata: crlf\r\n\r\n",
    "data:\n\n\n\n",
    "id: 1\nretry: 0\ndata: {\"a\":1}\n\n",
];

#[test]
fn ten_thousand_mutated_streams_never_panic_and_stay_bounded() {
    let mut events_out = 0usize;
    let mut bytes_out = 0usize;
    let mut refusals = 0usize;
    let started = std::time::Instant::now();

    let stats = atlas_guard::mutate::run(SEEDS, 10_000, 0x_A71A5_0003, |stream| {
        // Feed the stream in irregular slices, because the framing is incremental
        // and the bugs live exactly at a slice boundary.
        let mut parser = SseParser::new();
        let bytes = stream.as_bytes();
        let mut at = 0usize;
        let mut step = 1usize;
        let mut this_text = 0usize;
        let mut this_events = 0usize;
        while at < bytes.len() {
            let take = step.min(bytes.len() - at);
            match parser.feed(&bytes[at..at + take]) {
                Ok(events) => {
                    for event in &events {
                        this_events += 1;
                        if let SseEvent::Data(text) = event {
                            this_text += text.len();
                            // The cap is the contract: a line longer than the cap
                            // is an error, not a buffer that grows for ever.
                            assert!(
                                text.len() <= MAX_LINE_BYTES,
                                "an event carried {} bytes, over the cap",
                                text.len()
                            );
                        }
                    }
                }
                Err(reason) => {
                    refusals += 1;
                    assert!(!reason.to_string().trim().is_empty());
                    // A refusal ends the stream; nothing is read after it.
                    break;
                }
            }
            at += take;
            step = step % 7 + 1;
        }
        if let Ok(finish) = parser.finish() {
            for event in &finish.events {
                this_events += 1;
                if let SseEvent::Data(text) = event {
                    this_text += text.len();
                    assert!(text.len() <= MAX_LINE_BYTES);
                }
            }
        }
        // What came out cannot exceed what went in: a decoder, not a factory.
        assert!(
            this_text <= bytes.len(),
            "{} bytes of data out of a {} byte stream",
            this_text,
            bytes.len()
        );
        // Nor can a stream produce an event every byte: a frame needs a blank
        // line, so two input bytes are the floor for a dispatched event.
        assert!(
            this_events <= bytes.len() / 2 + 1,
            "{} events out of a {} byte stream",
            this_events,
            bytes.len()
        );
        events_out += this_events;
        bytes_out += this_text;
    });

    let elapsed = started.elapsed();
    println!(
        "{} mutated streams in {:.0} ms ({} events, {} bytes of data, {} refusals)",
        stats.runs,
        elapsed.as_secs_f64() * 1e3,
        events_out,
        bytes_out,
        refusals
    );
    assert_eq!(stats.runs, 10_000);
    assert!(events_out > 0, "no event was ever produced");
    assert!(
        elapsed.as_secs_f64() < 10.0,
        "10 000 streams took {:.1} s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn one_enormous_line_is_refused_rather_than_buffered() {
    let mut parser = SseParser::new();
    // A peer that never sends a newline: the parser must stop at the cap.
    let huge = vec![b'a'; MAX_LINE_BYTES + 1];
    let outcome = parser.feed(&huge);
    match outcome {
        // The class matters, not the sentence: `Protocol` is "the framing was
        // not what was asked for", which is what an unending line is. What must
        // not happen is a buffer that keeps growing.
        Err(atlas_ai::AiError::Protocol(detail)) => assert!(detail.contains("cap"), "{}", detail),
        Err(other) => panic!("an over-long SSE line answered {:?}", other),
        Ok(events) => panic!("an over-long line produced {} events", events.len()),
    }
}

#[test]
fn a_stream_that_stops_mid_frame_is_reported_as_truncated() {
    let mut parser = SseParser::new();
    let events = parser
        .feed(b"data: {\"half\":")
        .expect("a partial line is buffered");
    assert!(events.is_empty());
    let finish = parser.finish().expect("finish never fails on a partial line");
    assert!(
        finish.truncated,
        "a partial line is lost input and must be said"
    );
}

#[test]
fn a_million_tiny_deltas_are_one_event_each_and_nothing_more() {
    // "A stream with 1 000 000 tiny deltas" is the attack; two numbers answer it.
    // The transport stops the stream first: one frame here is 47 bytes, so a
    // million of them is 47 MB against `atlas_ai::http1::DEFAULT_MAX_BODY` = 8 MiB —
    // the socket refuses the body before this parser could be handed it. What this
    // test pins is the parser's own side of the contract: one frame in, one event
    // out, in linear time, with no event that carries more than one delta's text.
    let payload = "{\"choices\":[{\"delta\":{\"content\":\"a\"}}]}";
    let frame = format!("data: {}\n\n", payload);
    let frame = frame.as_bytes();
    let mut parser = SseParser::new();
    let mut events = 0usize;
    let mut text = 0usize;
    let started = std::time::Instant::now();
    for _ in 0..100_000 {
        for event in parser.feed(frame).expect("a whole frame is never an error") {
            events += 1;
            if let SseEvent::Data(data) = event {
                text += data.len();
            }
        }
        assert!(!parser.is_done());
    }
    assert_eq!(events, 100_000, "one event per frame, not one per byte");
    assert_eq!(
        text,
        100_000 * payload.len(),
        "each event carries its one payload, and nothing else travels with it"
    );
    assert!(
        started.elapsed().as_secs_f64() < 5.0,
        "100 000 frames took {:.1} s",
        started.elapsed().as_secs_f64()
    );
    // The parser is not a factory: 100 000 frames cost the bytes they contain.
    parser.finish().expect("finish after whole frames");
}
