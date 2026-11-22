//! SHA-256, written here because the alternative is a crypto dependency for one
//! function.
//!
//! The model file is 6.5 GB and its integrity is the whole point of a pin: a
//! truncated download that still starts is worse than no download, because the
//! failure arrives later and looks like a model bug. `ring` is in the dependency
//! graph (through rustls) but only as a transitive crate this workspace may not
//! import; adding it as a direct dependency for one hash would mean a new entry in
//! the audited table, new features to reason about and a new licence, for 110 lines
//! of FIPS-180-4 that can be checked against the published vectors.
//!
//! It is streaming: `Hasher::update` takes a chunk, so a 6.5 GB file is hashed in
//! 1 MiB pieces and never in memory.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// A streaming SHA-256. One instance hashes one stream.
pub struct Hasher {
    state: [u32; 8],
    buffer: Vec<u8>,
    length: u64,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    pub fn new() -> Hasher {
        Hasher { state: H0, buffer: Vec::with_capacity(64), length: 0 }
    }

    /// Add a chunk. Any size; it does not have to be a multiple of the block size.
    pub fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        // Fill the block in progress first, so the buffer never grows without bound.
        if !self.buffer.is_empty() {
            let want = 64 - self.buffer.len();
            let take = want.min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == 64 {
                let block = self.buffer.clone();
                self.compress(&block);
                self.buffer.clear();
            }
        }
        let mut blocks = bytes.chunks_exact(64);
        for block in &mut blocks {
            self.compress(block);
        }
        self.buffer.extend_from_slice(blocks.remainder());
    }

    /// Finish and return the digest as 32 bytes.
    pub fn finish(mut self) -> [u8; 32] {
        let bit_length = self.length.wrapping_mul(8);
        self.buffer.push(0x80);
        while self.buffer.len() % 64 != 56 {
            self.buffer.push(0);
        }
        self.buffer.extend_from_slice(&bit_length.to_be_bytes());
        let blocks: Vec<u8> = std::mem::take(&mut self.buffer);
        for block in blocks.chunks_exact(64) {
            self.compress(block);
        }
        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// The digest as lowercase hex — the form `shasum -a 256` prints, so a person can
    /// compare the two by eye.
    pub fn finish_hex(self) -> String {
        hex(&self.finish())
    }

    fn compress(&mut self, block: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

/// Lowercase hex of any byte slice.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0xf) as u32, 16).unwrap_or('0'));
    }
    out
}

/// One call for a byte slice (tests, the small pinned files).
pub fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finish_hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 / the vectors everyone publishes: no digest is checked against
    /// itself here.
    #[test]
    fn the_published_vectors_hold() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    /// The streaming form must equal the one-shot form across the block boundary,
    /// whichever way the input is cut — that is the only way a 6.5 GB file is hashed.
    #[test]
    fn any_chunking_gives_the_same_digest() {
        let data: Vec<u8> = (0..5_000u32).map(|i| (i % 251) as u8).collect();
        let one_shot = sha256(&data);
        for chunk in [1usize, 7, 63, 64, 65, 1_024, 4_096] {
            let mut hasher = Hasher::new();
            for piece in data.chunks(chunk) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finish_hex(), one_shot, "chunk size {chunk}");
        }
    }

    /// A million 'a' is the fourth published vector, and it is longer than any block
    /// arithmetic mistake survives.
    #[test]
    fn a_million_a_is_the_published_digest() {
        let mut hasher = Hasher::new();
        let block = vec![b'a'; 1_000];
        for _ in 0..1_000 {
            hasher.update(&block);
        }
        assert_eq!(
            hasher.finish_hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
