//! SHA-1, because two columns and every cache file name already use it.
//!
//! `ingest.py` writes `hashlib.sha1(...)` in three places: the `articles.hash`
//! column, the duplicate lookup (`hash IN (?,?)`), and the text-cache file name.
//! An archive that has been running for months is full of those digests, so a
//! different one here would silently split duplicate detection in two.
//!
//! The workspace pins no hashing crate (and this is not a security decision —
//! it is a key), so SHA-1 is written out. It is `forbid(unsafe_code)` and
//! checked against the FIPS test vectors below.

use std::fmt::Write as _;

/// The SHA-1 digest of `data`, lowercase hex (40 characters).
pub fn sha1_hex(data: &[u8]) -> String {
    let mut state: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut message = Vec::with_capacity(data.len() + 72);
    message.extend_from_slice(data);
    let bits = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());

    let mut words = [0u32; 80];
    for block in message.chunks(64) {
        for index in 0..16 {
            let at = index * 4;
            words[index] = u32::from_be_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]]);
        }
        for index in 16..80 {
            words[index] = (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                .rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (state[0], state[1], state[2], state[3], state[4]);
        for (index, word) in words.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut out = String::with_capacity(40);
    for word in state {
        let _ = write!(out, "{:08x}", word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fips_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn a_long_body_hashes_without_a_panic() {
        let body = vec![b'x'; 1_000_000];
        assert_eq!(sha1_hex(&body).len(), 40);
    }
}
