//! `punycode` — RFC 3492 decoding, so an encoded name cannot hide.
//!
//! Decides what `xn--bcher-kva` reads as (`bücher`), including the bias
//! adaptation the encoding defines, so the gate compares the name a browser would
//! show against its blocklist instead of comparing raw label bytes.
//!
//! It does not encode, does not validate a domain and does not resolve anything:
//! one label in, the characters it claims to be — or `None` when the label is not
//! the encoding it pretends to be.

/// RFC 3492 punycode decode of one label (without the `xn--` prefix), so an
/// encoded name cannot hide behind a blocklist entry.
pub(crate) fn punycode_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output: Vec<char> = Vec::new();
    let mut start = 0usize;
    if let Some(pos) = input.rfind('-') {
        for ch in input.get(..pos)?.chars() {
            if !ch.is_ascii() {
                return None;
            }
            output.push(ch);
        }
        start = pos + 1;
    }
    let mut n: u32 = 128;
    let mut i: u32 = 0;
    let mut bias: u32 = 72;
    let mut pos = start;
    while pos < bytes.len() {
        let old_i = i;
        let mut weight: u32 = 1;
        let mut k: u32 = 36;
        loop {
            if pos >= bytes.len() {
                return None;
            }
            let c = bytes[pos];
            pos += 1;
            let digit = match c {
                b'a'..=b'z' => (c - b'a') as u32,
                b'A'..=b'Z' => (c - b'A') as u32,
                b'0'..=b'9' => (c - b'0') as u32 + 26,
                _ => return None,
            };
            i = i.checked_add(digit.checked_mul(weight)?)?;
            let t = if k <= bias {
                1
            } else if k >= bias + 26 {
                26
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            weight = weight.checked_mul(36 - t)?;
            k += 36;
        }
        let len = (output.len() as u32) + 1;
        bias = adapt_bias(i - old_i, len, old_i == 0);
        n = n.checked_add(i / len)?;
        i %= len;
        let ch = char::from_u32(n)?;
        if (i as usize) > output.len() {
            return None;
        }
        output.insert(i as usize, ch);
        i += 1;
    }
    Some(output.into_iter().collect())
}

fn adapt_bias(mut delta: u32, numpoints: u32, first: bool) -> u32 {
    if first {
        delta /= 700;
    } else {
        delta /= 2;
    }
    delta += delta / numpoints;
    let mut k = 0u32;
    while delta > 455 {
        delta /= 35;
        k += 36;
    }
    k + (36 * delta) / (delta + 38)
}
