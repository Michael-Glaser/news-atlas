//! Server-Sent Events, parsed from whatever byte boundaries the socket offers.
//!
//! The parser is a byte buffer plus a small state machine, not a line reader over
//! a `BufRead`: a chunk can split a field name, a `\r\n` pair, or a multi-byte
//! UTF-8 character, and all three happen with llama.cpp's token-by-token output.
//! Bytes are accumulated until a `\n` exists, so a split character is never
//! decoded on its own.
//!
//! Rules, and why they are these rules:
//!
//! * `data:` lines accumulate; one blank line dispatches them joined by `\n`
//!   (the SSE spec's rule, which matters because a JSON payload may be split).
//!   The accumulating payload is capped at [`MAX_FRAME_BYTES`] *while it grows*:
//!   a cap on one line bounds nothing about an event made of a million lines.
//! * `event:`, `id:` and `retry:` are read and ignored — the model stream carries
//!   no semantics in them, and pretending otherwise would be invented behaviour.
//! * a line starting with `:` is a comment (llama.cpp sends keep-alives this way)
//!   and is dropped without touching the accumulated data.
//! * `data: [DONE]` ends the stream; everything after it is ignored.
//! * at end of file, complete `data:` lines that never got their blank line are
//!   still dispatched (some proxies end without one), and bytes that do not form
//!   a whole line are **discarded** and reported as `truncated`. A half-written
//!   JSON payload is not a token: executing it would be executing a fragment.

use crate::error::AiError;

/// Cap on one SSE line. A single token delta is tiny; a line larger than this is
/// not a frame we should keep in memory.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Cap on one event's whole payload — every `data:` line of it, joined.
///
/// [`MAX_LINE_BYTES`] bounds a line, and a line is not an event: one undispatched
/// event can carry as many `data:` lines as the sender likes, and measured, 200 000
/// of them (~14 MB here, ~8 MB in the reported case) were accepted into one `String`
/// that only the blank line would ever release. 64 KiB is far more than a chat
/// delta or one tool-call fragment needs, and small enough that the parser's memory
/// is a number this crate chose.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// What one dispatched event carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// The `data:` payload of one event, joined by `\n`.
    Data(String),
    /// `data: [DONE]` — the model is finished talking.
    Done,
}

/// The tail of a stream: the events that were complete, and whether bytes were
/// thrown away because they never formed a line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SseFinish {
    pub events: Vec<SseEvent>,
    pub truncated: bool,
}

/// An incremental SSE parser.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
    /// Bytes the pending event's payload already holds, separators included, so the
    /// frame cap costs an addition per line instead of a join per line.
    data_bytes: usize,
    done: bool,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser::default()
    }

    /// Has `[DONE]` been seen?
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Feed bytes; get every event that is now complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, AiError> {
        if self.done {
            return Ok(Vec::new());
        }
        self.buf.extend_from_slice(bytes);
        let mut out: Vec<SseEvent> = Vec::new();
        loop {
            if self.done {
                self.buf.clear();
                break;
            }
            let pos = match self.buf.iter().position(|b| *b == b'\n') {
                Some(pos) => pos,
                None => {
                    if self.buf.len() > MAX_LINE_BYTES {
                        return Err(AiError::Protocol(format!(
                            "an SSE line is over the {} byte cap",
                            MAX_LINE_BYTES
                        )));
                    }
                    break;
                }
            };
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).to_string();
            self.line(&line, &mut out)?;
        }
        Ok(out)
    }

    /// End of stream: dispatch the complete data lines that never met a blank
    /// line, discard a trailing partial line, and say whether anything was lost.
    pub fn finish(&mut self) -> Result<SseFinish, AiError> {
        let mut events: Vec<SseEvent> = Vec::new();
        let mut truncated = false;
        if !self.buf.is_empty() {
            // A line without its newline: the impossible half of a frame.
            truncated = true;
            self.buf.clear();
        }
        if !self.data.is_empty() && !self.done {
            let joined = self.data.join("\n");
            self.data.clear();
            self.data_bytes = 0;
            if joined.trim() == "[DONE]" {
                self.done = true;
                events.push(SseEvent::Done);
            } else if !joined.trim().is_empty() {
                events.push(SseEvent::Data(joined));
            }
        }
        Ok(SseFinish { events, truncated })
    }

    /// One complete line. The only failure is the frame cap: a payload that is not
    /// going to fit is refused before it is copied, not after it is joined.
    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) -> Result<(), AiError> {
        if line.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        if field == "data" {
            let separator = if self.data.is_empty() { 0 } else { 1 };
            let grown = self.data_bytes + separator + value.len();
            if grown > MAX_FRAME_BYTES {
                // Dropped rather than kept: the sender, not the reader, chose this
                // size, and half a 14 MB event is worth nothing to anybody.
                self.data.clear();
                self.data_bytes = 0;
                return Err(AiError::Protocol(format!(
                    "an SSE event's payload is over the {} byte frame cap",
                    MAX_FRAME_BYTES
                )));
            }
            self.data_bytes = grown;
            self.data.push(value.to_string());
        }
        // `event`, `id`, `retry`: read, never invented into meaning.
        Ok(())
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if self.data.is_empty() {
            return;
        }
        let joined = self.data.join("\n");
        self.data.clear();
        self.data_bytes = 0;
        if joined.trim() == "[DONE]" {
            self.done = true;
            out.push(SseEvent::Done);
        } else if !joined.trim().is_empty() {
            out.push(SseEvent::Data(joined));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = concat!(
        ": keep-alive\n\n",
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Nach\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"richt\"}}]}\n\n",
        "data: [DONE]\n\n"
    );

    fn data_of(events: &[SseEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                SseEvent::Data(text) => Some(text.clone()),
                SseEvent::Done => None,
            })
            .collect()
    }

    #[test]
    fn a_whole_stream_is_three_events_and_done() {
        let mut parser = SseParser::new();
        let events = parser.feed(STREAM.as_bytes()).unwrap();
        assert_eq!(data_of(&events).len(), 3);
        assert_eq!(events.last(), Some(&SseEvent::Done));
        assert!(parser.is_done());
        assert!(!parser.finish().unwrap().truncated);
    }

    #[test]
    fn every_byte_boundary_gives_the_same_events() {
        // One byte at a time: the case a `read_to_string`-style parser gets wrong.
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        for byte in STREAM.as_bytes() {
            events.extend(parser.feed(&[*byte]).unwrap());
        }
        assert_eq!(data_of(&events).len(), 3);
        assert_eq!(events.last(), Some(&SseEvent::Done));

        // Irregular chunk sizes over the same stream must agree with byte-at-a-time.
        for size in [2usize, 3, 7, 13, 64, 512] {
            let mut parser = SseParser::new();
            let mut events = Vec::new();
            for chunk in STREAM.as_bytes().chunks(size) {
                events.extend(parser.feed(chunk).unwrap());
            }
            assert_eq!(data_of(&events).len(), 3, "chunk size {}", size);
            assert_eq!(events.last(), Some(&SseEvent::Done), "chunk size {}", size);
        }
    }

    #[test]
    fn a_multibyte_character_split_across_chunks_survives() {
        let payload = "data: {\"choices\":[{\"delta\":{\"content\":\"Grüße\"}}]}\n\n";
        let bytes = payload.as_bytes();
        let cut = payload.find('ü').unwrap() + 1; // inside the two-byte 'ü'
        let mut parser = SseParser::new();
        let mut events = parser.feed(&bytes[..cut]).unwrap();
        events.extend(parser.feed(&bytes[cut..]).unwrap());
        let data = data_of(&events);
        assert_eq!(data.len(), 1);
        assert!(data[0].contains("Grüße"), "{}", data[0]);
    }

    #[test]
    fn a_truncated_final_chunk_is_discarded_not_executed() {
        let cut = STREAM.find("[DONE]").unwrap() + 2;
        let partial = &STREAM.as_bytes()[..cut];
        let mut parser = SseParser::new();
        let events = parser.feed(partial).unwrap();
        assert_eq!(data_of(&events).len(), 3, "the complete frames still arrive");
        let finish = parser.finish().unwrap();
        assert!(finish.truncated, "the half-written frame is reported");
        assert_eq!(
            finish.events,
            Vec::new(),
            "nothing half-written is handed on as data"
        );
    }

    #[test]
    fn complete_data_lines_without_a_blank_line_still_arrive() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: {\"choices\":[]}\n").unwrap();
        assert!(events.is_empty());
        let finish = parser.finish().unwrap();
        assert_eq!(finish.events.len(), 1);
        assert!(!finish.truncated);
    }

    #[test]
    fn an_eof_without_done_is_reported_as_truncated() {
        let mut parser = SseParser::new();
        parser.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"half").unwrap();
        let finish = parser.finish().unwrap();
        assert!(finish.truncated);
        assert!(finish.events.is_empty());
    }

    #[test]
    fn multi_line_data_is_joined() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: one\ndata: two\n\n").unwrap();
        assert_eq!(events, vec![SseEvent::Data("one\ntwo".to_string())]);
    }

    #[test]
    fn one_undispatched_event_cannot_grow_without_a_frame_cap() {
        // `MAX_LINE_BYTES` bounds a *line*; the payload of one event is every
        // `data:` line that arrived before the blank line that dispatches it, and
        // nothing bounded that. Measured on the old parser: 200 000 lines, ~8 MB,
        // accepted into one undispatched event — memory the sender chooses.
        let line = b"data: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
        let mut parser = SseParser::new();
        let mut fed = 0usize;
        let mut result = Ok(Vec::new());
        for _ in 0..200_000 {
            fed += line.len();
            result = parser.feed(line);
            if result.is_err() {
                break;
            }
        }
        println!(
            "MEASURED: {} bytes of one undispatched event, result {:?}",
            fed,
            result.as_ref().map(|events| events.len())
        );
        match result {
            Err(AiError::Protocol(message)) => {
                assert!(
                    message.contains("frame"),
                    "the reader is told which cap was hit: {}",
                    message
                );
            }
            other => panic!(
                "one undispatched event must be capped at {} bytes, got {:?} after {} bytes",
                MAX_FRAME_BYTES, other, fed
            ),
        }
        // The cap fires while the payload grows, not after the whole stream.
        assert!(
            fed < 4 * MAX_FRAME_BYTES,
            "the cap must fire near the frame cap, not after {} bytes",
            fed
        );
    }

    #[test]
    fn a_frame_under_the_cap_still_dispatches() {
        // Half the cap, in many `data:` lines: a real multi-line payload.
        let mut parser = SseParser::new();
        let payload = "x".repeat(64);
        let mut events = Vec::new();
        let mut written = 0usize;
        while written < MAX_FRAME_BYTES / 2 {
            written += payload.len() + 1;
            events.extend(parser.feed(format!("data: {}\n", payload).as_bytes()).unwrap());
        }
        assert!(events.is_empty(), "nothing dispatches before the blank line");
        events.extend(parser.feed(b"\n").unwrap());
        assert_eq!(events.len(), 1);
        let text = match &events[0] {
            SseEvent::Data(text) => text,
            other => panic!("expected data, got {:?}", other),
        };
        assert!(
            text.len() <= MAX_FRAME_BYTES && text.len() >= MAX_FRAME_BYTES / 2,
            "{} bytes joined",
            text.len()
        );
    }

    #[test]
    fn bytes_after_done_are_ignored() {
        let mut parser = SseParser::new();
        let events = parser
            .feed(b"data: [DONE]\n\ndata: {\"late\":true}\n\n")
            .unwrap();
        assert_eq!(events, vec![SseEvent::Done]);
        assert!(parser.feed(b"data: more\n\n").unwrap().is_empty());
    }
}
