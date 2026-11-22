//! How big a fetch may be, and what counts as a page rather than a file.
//!
//! Four numbers and two predicates, in one place so a caller can find the ceiling
//! without reading the transport: [`DEFAULT_MAX_BYTES`] (one article page),
//! [`HARD_MAX_BYTES`] (the ceiling no caller can raise), [`MAX_HEADER_BYTES`] (a
//! header block no news page needs) and [`read_capped`] (the read that enforces the
//! body cap while it happens; [`read_capped_error`] is the same read for a caller
//! that has to word its failure), plus [`clamp_bytes`] and [`is_document_type`].
//!
//! The decisions, stated: a body is read up to `cap + 1` bytes so that a truncated
//! body is *known* to be truncated, and never more, so a hostile response cannot
//! make this app allocate what it did not ask for. A content type that is not a
//! page or a feed is not read at all — an image, a PDF or a video is a body whose
//! bytes would cost bandwidth and tell the caller nothing.
//!
//! It does not: open a socket, decode text, or decide whether the *content* is
//! news. Nothing here looks at a host name or a publisher.

use std::io::Read;

/// `collector.fetch_page_http(max_bytes=600000)` — one article page.
pub const DEFAULT_MAX_BYTES: usize = 600_000;

/// `collector.MAX_FETCH_BYTES` — the ceiling a caller cannot raise.
pub const HARD_MAX_BYTES: usize = 4_000_000;

/// A response header block larger than this is not a news page.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;

/// `collector._DOCUMENT_TYPES` — what can be a page or a feed.
const DOCUMENT_TYPES: &[&str] = &["xml", "html", "text/", "json"];

/// `max_bytes` clamped to [`HARD_MAX_BYTES`]; 0 or less means [`DEFAULT_MAX_BYTES`].
pub fn clamp_bytes(max_bytes: usize) -> usize {
    match max_bytes {
        0 => DEFAULT_MAX_BYTES,
        n => n.min(HARD_MAX_BYTES),
    }
}

/// `collector`'s `r.read(max_bytes + 1)[:max_bytes]`, as a reader-agnostic helper.
///
/// The extra byte is how a truncated body is *known* to be truncated, and the cap
/// is why a hostile response cannot make the caller allocate more than it asked
/// for. Returns `(bytes, truncated)`.
///
/// A cut in the middle of a multi-byte character is not repaired here: the decoder
/// turns the half character into one replacement character, which is what a browser
/// shows for the same bytes.
///
/// The cap counts bytes *after* the transport has decoded them, which is the only
/// place it can count them: a gzip body the site sent without being asked is
/// decompressed by the transport, and cutting the decoded stream is what keeps a
/// decompression bomb to `cap` bytes of memory.
pub fn read_capped<R: Read>(reader: R, cap: usize) -> Result<(Vec<u8>, bool), String> {
    read_capped_error(reader, cap).map_err(|e| format!("cannot read the body ({})", e))
}

/// [`read_capped`], keeping the failure's `ErrorKind`.
///
/// The caller that words the failure ([`crate::reason::body_failure`]) needs the kind
/// to say *which* phase gave up — "timed out while reading the page" and "the
/// connection was closed before the answer was complete" send the model to different
/// places — and a sentence is not a type to branch on.
pub fn read_capped_error<R: Read>(
    reader: R,
    cap: usize,
) -> Result<(Vec<u8>, bool), std::io::Error> {
    let mut buf: Vec<u8> = Vec::with_capacity(cap.min(64 * 1024));
    let limit = (cap as u64).saturating_add(1);
    reader.take(limit).read_to_end(&mut buf)?;
    let truncated = buf.len() > cap;
    if truncated {
        buf.truncate(cap);
    }
    Ok((buf, truncated))
}

/// `collector._document_type_ok()`: can this content type be a page or a feed?
/// An empty header counts as yes, because plenty of servers send none.
pub fn is_document_type(content_type: &str) -> bool {
    let low = content_type.trim().to_ascii_lowercase();
    if low.is_empty() {
        return true;
    }
    DOCUMENT_TYPES.iter().any(|marker| low.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::charset::decode;

    #[test]
    fn the_cap_is_enforced_and_reported() {
        let body = vec![b'x'; 100_000];
        let (bytes, truncated) = read_capped(&body[..], 10).unwrap();
        assert_eq!(bytes.len(), 10);
        assert!(truncated);
        // Exactly at the cap is not truncation: nothing was cut off.
        let (bytes, truncated) = read_capped(&b"0123456789"[..], 10).unwrap();
        assert_eq!(bytes.len(), 10);
        assert!(!truncated);
        let (bytes, truncated) = read_capped(&b"short"[..], 10).unwrap();
        assert_eq!(bytes, b"short".to_vec());
        assert!(!truncated);
        let (bytes, truncated) = read_capped(&b""[..], 10).unwrap();
        assert!(bytes.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn a_multi_byte_character_cut_by_the_cap_becomes_one_replacement_char() {
        // "aé" is three bytes; a cap of two cuts the two-byte é in half.
        let body = "aé".as_bytes().to_vec();
        assert_eq!(body.len(), 3);
        let (bytes, truncated) = read_capped(&body[..], 2).unwrap();
        assert!(truncated);
        assert_eq!(bytes, vec![0x61, 0xC3]);
        let text = decode(&bytes, "utf-8");
        assert_eq!(text, "a\u{FFFD}");
        assert_eq!(text.chars().count(), 2);
        // The same with a four-byte character, cut after two bytes.
        let emoji = "😀".as_bytes().to_vec();
        let (bytes, truncated) = read_capped(&emoji[..], 2).unwrap();
        assert!(truncated);
        assert_eq!(decode(&bytes, "utf-8"), "\u{FFFD}");
        // And a cap of one byte inside a two-byte character.
        let (bytes, truncated) = read_capped(&body[..], 1).unwrap();
        assert!(truncated);
        assert_eq!(decode(&bytes, "utf-8"), "a");
    }

    #[test]
    fn the_cap_defaults_and_is_never_above_the_hard_ceiling() {
        assert_eq!(clamp_bytes(0), DEFAULT_MAX_BYTES);
        assert_eq!(clamp_bytes(1234), 1234);
        assert_eq!(clamp_bytes(HARD_MAX_BYTES + 1), HARD_MAX_BYTES);
        assert_eq!(clamp_bytes(usize::MAX), HARD_MAX_BYTES);
    }

    #[test]
    fn only_document_types_are_read() {
        for ctype in [
            "",
            "text/html; charset=utf-8",
            "text/plain",
            "application/xhtml+xml",
            "application/rss+xml",
            "application/atom+xml",
            "application/xml",
            "application/json",
        ] {
            assert!(is_document_type(ctype), "{:?} should be read", ctype);
        }
        for ctype in [
            "image/png",
            "application/pdf",
            "audio/mpeg",
            "video/mp4",
            "application/octet-stream",
        ] {
            assert!(!is_document_type(ctype), "{:?} should not be read", ctype);
        }
    }
}
