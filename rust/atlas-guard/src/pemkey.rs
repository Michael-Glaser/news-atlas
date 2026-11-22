//! `pemkey` — PEM-armoured private keys, out of a text before it is logged.
//!
//! Decides where a `-----BEGIN … PRIVATE KEY-----` block starts and where it
//! ends, and replaces the whole block with `[redacted private key]`. A block with
//! no `-----END` is redacted to the end of the text: a truncated key is still a
//! key, and guessing where it stopped would leave the rest behind.
//!
//! The armour is read **case-insensitively**. RFC 7468 writes `-----BEGIN …-----`
//! in capitals, and the OpenSSH and PKCS#8 tools follow it, but GPG armour and
//! hand-typed or re-cased text are lowercase, and a key that has been through a
//! `tr` or a case-folding editor is still a key. A capital-only comparison left
//! `-----begin rsa private key-----` completely intact (measured, hits 0).
//!
//! It does not decide what a *credential* looks like beyond this one shape (that
//! is `redact`), it does not validate a key, it does not parse PEM, and it cannot
//! recognise a key body that was logged *without* its armour — that is a shape
//! with no marker to read. It reads the armour a person (or a tool) copies into a
//! log by accident.

/// The marker a redacted block becomes.
pub(crate) const PLACEHOLDER: &str = "[redacted private key]";

/// Does `text` start with `needle` at `i`, ignoring ASCII case?
fn starts_with_fold(text: &str, i: usize, needle: &str) -> bool {
    match text.get(i..i.saturating_add(needle.len())) {
        Some(head) => head.eq_ignore_ascii_case(needle),
        None => false,
    }
}

/// The byte offset of the next `-----END` (any case) at or after `from`.
///
/// The same fold as the `BEGIN` marker: a block whose footer was re-cased is a
/// block. A footer that is not found leaves the caller redacting to the end of
/// the text, which is the safe direction.
fn find_armor_end(text: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(rel) = text.get(at..).and_then(|tail| tail.find("-----")) {
        let dash = at + rel;
        if starts_with_fold(text, dash + 5, "END") {
            return Some(dash);
        }
        at = dash + 5;
    }
    None
}

/// Replace every PEM private-key block in `text`, counting the blocks.
///
/// Returns the text and how many blocks were replaced.
pub(crate) fn strip_private_keys(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut hits = 0usize;
    let mut i = 0usize;
    while i < text.len() {
        if !starts_with_fold(text, i, "-----BEGIN") {
            match text.get(i..).and_then(|rest| rest.chars().next()) {
                Some(ch) => {
                    out.push(ch);
                    i += ch.len_utf8();
                    continue;
                }
                None => break,
            }
        }
        let header_end = match text.get(i..).and_then(|rest| rest.find('\n')) {
            Some(rel) => i + rel,
            None => text.len(),
        };
        let header = match text.get(i..header_end) {
            Some(head) => head.to_ascii_uppercase(),
            None => String::new(),
        };
        if !header.contains("PRIVATE KEY") {
            // Some other armour (`CERTIFICATE`, `PUBLIC KEY`, `SIGNATURE`): it is
            // public material, and the marker is copied on unchanged so the scan
            // resumes after it rather than re-testing every byte.
            match text.get(i..).and_then(|rest| rest.chars().next()) {
                Some(ch) => {
                    out.push(ch);
                    i += ch.len_utf8();
                }
                None => break,
            }
            continue;
        }
        let end = match find_armor_end(text, header_end) {
            Some(abs) => match text.get(abs..).and_then(|tail| tail.find('\n')) {
                Some(n) => abs + n,
                None => text.len(),
            },
            None => text.len(),
        };
        out.push_str(PLACEHOLDER);
        hits += 1;
        i = end;
    }
    (out, hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_replaced_and_the_text_around_it_survives() {
        let body = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEabcdef\n-----END RSA PRIVATE KEY-----\nafter";
        let (out, hits) = strip_private_keys(body);
        assert_eq!(hits, 1);
        assert!(out.contains(PLACEHOLDER), "{}", out);
        assert!(!out.contains("MIIEabcdef"), "{}", out);
        assert!(out.contains("before"), "{}", out);
        assert!(out.contains("after"), "{}", out);
    }

    #[test]
    fn an_unterminated_block_is_a_key_to_the_end_of_the_text() {
        let body = "log line\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk";
        let (out, hits) = strip_private_keys(body);
        assert_eq!(hits, 1);
        assert!(!out.contains("b3BlbnNzaC1rZXk"), "{}", out);
        assert!(out.contains(PLACEHOLDER));
        assert!(out.starts_with("log line"), "{}", out);
    }

    #[test]
    fn a_public_certificate_is_not_a_private_key() {
        let body = "-----BEGIN CERTIFICATE-----\nMIIBszCCAVmgAwIBAgIU\n-----END CERTIFICATE-----";
        let (out, hits) = strip_private_keys(body);
        assert_eq!(hits, 0, "{}", out);
        assert_eq!(out, body);
    }

    #[test]
    fn two_blocks_are_two_hits() {
        let one = "-----BEGIN PRIVATE KEY-----\na\n-----END PRIVATE KEY-----";
        let body = format!("{}\nand\n{}", one, one);
        let (out, hits) = strip_private_keys(&body);
        assert_eq!(hits, 2, "{}", out);
        assert_eq!(out.matches(PLACEHOLDER).count(), 2, "{}", out);
    }

    #[test]
    fn the_armour_is_read_in_any_case() {
        // A key that went through a case-folding editor, a GPG block, or a hand
        // retyped header: the marker is the same marker. Comparing only capitals
        // left the whole block intact (measured, hits 0).
        for body in [
            "-----begin rsa private key-----\nMIIEow\n-----end rsa private key-----\ntail",
            "-----Begin OpenSSH Private Key-----\nb3BlbnNzaC1rZXk\n-----End OpenSSH Private Key-----",
            "-----BEGIN pgp private key block-----\nlQOYBF\n-----END pgp private key block-----",
        ] {
            let (out, hits) = strip_private_keys(body);
            assert_eq!(hits, 1, "{} gave {:?}", body, out);
            assert!(out.contains(PLACEHOLDER), "{} gave {:?}", body, out);
            assert!(!out.contains("MIIEow"), "{}", out);
            assert!(!out.contains("b3BlbnNzaC1rZXk"), "{}", out);
            assert!(!out.contains("lQOYBF"), "{}", out);
        }
        // Public armour stays, in any case.
        let (out, hits) =
            strip_private_keys("-----begin certificate-----\nMIIBsz\n-----end certificate-----");
        assert_eq!(hits, 0, "{:?}", out);
    }
}
