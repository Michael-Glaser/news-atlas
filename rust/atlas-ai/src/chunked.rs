//! `Transfer-Encoding: chunked`, as a state machine you can feed arbitrary bytes.
//!
//! Decisions: where a chunk size ends, what a chunk extension is (`;name=value`,
//! skipped), that a trailer header is skipped rather than interpreted, and that bad
//! framing is [`AiError::Protocol`] — never a silent truncation, because a chunk
//! size that is not hex means the stream is not what we were told it is.
//!
//! It does not: read from a socket (the caller feeds bytes), decide when a body is
//! complete (the terminating `0`-chunk says that), or cap the body — [`crate::http1`]
//! owns the response and its cap.

use crate::error::{clip, AiError};
use crate::http1::MAX_HEADER_BYTES;

/// Decodes `Transfer-Encoding: chunked` from arbitrary byte boundaries.
///
/// Push whatever the socket returned; the decoded body is appended to the vector
/// you pass and the return value says whether the terminating `0`-chunk (and its
/// trailers) was seen. Invalid framing is [`AiError::Protocol`], never a silent
/// truncation: a chunk size that is not hex means the stream is not what we were
/// told it is.
#[derive(Debug, Default)]
pub struct ChunkedDecoder {
    buf: Vec<u8>,
    state: ChunkState,
    /// Bytes still expected in the chunk being copied.
    want: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ChunkState {
    #[default]
    Size,
    Data,
    DataCrlf,
    Trailers,
    Done,
}

impl ChunkedDecoder {
    pub fn new() -> ChunkedDecoder {
        ChunkedDecoder::default()
    }

    /// Has the terminating chunk been consumed?
    pub fn finished(&self) -> bool {
        self.state == ChunkState::Done
    }

    /// Feed raw bytes; append decoded bytes to `out`. `Ok(true)` means finished.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) -> Result<bool, AiError> {
        if self.state == ChunkState::Done {
            return Ok(true);
        }
        self.buf.extend_from_slice(bytes);
        loop {
            match self.state {
                ChunkState::Size => {
                    let line = match take_line(&mut self.buf)? {
                        Some(line) => line,
                        None => return Ok(false),
                    };
                    let text = String::from_utf8_lossy(&line).to_string();
                    let size_text = text.split(';').next().unwrap_or("").trim();
                    if size_text.is_empty() {
                        return Err(AiError::Protocol("a chunk without a size".into()));
                    }
                    let size = usize::from_str_radix(size_text, 16).map_err(|_| {
                        AiError::Protocol(format!("chunk size {:?} is not hex", clip(&text, 20)))
                    })?;
                    if size == 0 {
                        self.state = ChunkState::Trailers;
                    } else {
                        self.want = size;
                        self.state = ChunkState::Data;
                    }
                }
                ChunkState::Data => {
                    if self.buf.is_empty() {
                        return Ok(false);
                    }
                    let take = self.want.min(self.buf.len());
                    out.extend_from_slice(&self.buf[..take]);
                    self.buf.drain(..take);
                    self.want -= take;
                    if self.want > 0 {
                        return Ok(false);
                    }
                    self.state = ChunkState::DataCrlf;
                }
                ChunkState::DataCrlf => {
                    if self.buf.len() < 2 {
                        return Ok(false);
                    }
                    let crlf: Vec<u8> = self.buf.drain(..2).collect();
                    if crlf.as_slice() != b"\r\n" {
                        return Err(AiError::Protocol(
                            "chunk data is not followed by CRLF".into(),
                        ));
                    }
                    self.state = ChunkState::Size;
                }
                ChunkState::Trailers => {
                    let line = match take_line(&mut self.buf)? {
                        Some(line) => line,
                        None => return Ok(false),
                    };
                    if line.is_empty() {
                        self.state = ChunkState::Done;
                        return Ok(true);
                    }
                    // A trailer header is skipped, never interpreted.
                }
                ChunkState::Done => return Ok(true),
            }
        }
    }
}

/// Take one `\n`-terminated line out of `buf`, stripping the `\r`.
/// `Ok(None)` means more bytes are needed.
fn take_line(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, AiError> {
    let pos = match buf.iter().position(|b| *b == b'\n') {
        Some(pos) => pos,
        None => {
            if buf.len() > MAX_HEADER_BYTES {
                return Err(AiError::Protocol(
                    "a chunked-framing line is over the cap".into(),
                ));
            }
            return Ok(None);
        }
    };
    let mut line: Vec<u8> = buf.drain(..=pos).collect();
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn chunked_decoder_survives_every_split() {
        let body = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        for split in 0..=body.len() {
            let mut decoder = ChunkedDecoder::new();
            let mut out = Vec::new();
            let mut done = decoder.push(&body[..split], &mut out).unwrap();
            if !done {
                done = decoder.push(&body[split..], &mut out).unwrap();
            }
            assert!(done, "split at {}", split);
            assert_eq!(out, b"Wikipedia", "split at {}", split);
        }
    }

    #[test]
    fn chunked_decoder_skips_trailers_and_extensions() {
        let mut decoder = ChunkedDecoder::new();
        let mut out = Vec::new();
        let done = decoder
            .push(b"5;ext=1\r\nhello\r\n0\r\nX-Trailer: 1\r\n\r\n", &mut out)
            .unwrap();
        assert!(done);
        assert_eq!(out, b"hello");
    }

    #[test]
    fn chunked_decoder_refuses_bad_framing() {
        let mut decoder = ChunkedDecoder::new();
        let mut out = Vec::new();
        let error = decoder.push(b"zz\r\nhello\r\n", &mut out).unwrap_err();
        assert_eq!(error.kind(), "protocol");
    }

}
