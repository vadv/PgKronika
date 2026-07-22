//! Minimal SHA-256 (FIPS 180-4) for content-derived identity.
//!
//! Observation and lineage IDs must be reproducible by every writer, reader,
//! and rebuild path forever, so the digest function is part of the identity
//! contract itself. A local implementation keeps the crate dependency-free;
//! the tests below pin it to the FIPS 180-4 example vectors.

const BLOCK_LEN: usize = 64;

/// Fractional parts of the cube roots of the first 64 primes.
const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// Fractional parts of the square roots of the first 8 primes.
const H_INIT: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// Digest of the concatenation of `parts`, without materializing it.
pub(crate) fn digest_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut state = H_INIT;
    let mut buf = [0_u8; BLOCK_LEN];
    let mut buf_len = 0_usize;
    let mut total_len = 0_usize;

    for part in parts {
        total_len = total_len.wrapping_add(part.len());
        let mut rest = *part;
        if buf_len > 0 {
            let take = rest.len().min(BLOCK_LEN - buf_len);
            buf[buf_len..buf_len + take].copy_from_slice(&rest[..take]);
            buf_len += take;
            rest = &rest[take..];
            if buf_len == BLOCK_LEN {
                compress(&mut state, &buf);
                buf_len = 0;
            }
        }
        // A non-empty remainder means the carry buffer was flushed above.
        if !rest.is_empty() {
            let (blocks, tail) = rest.as_chunks::<BLOCK_LEN>();
            for block in blocks {
                compress(&mut state, block);
            }
            buf[..tail.len()].copy_from_slice(tail);
            buf_len = tail.len();
        }
    }

    // Padding: one 0x80 byte, zeros, then the big-endian bit length.
    let bit_len = (total_len as u64).wrapping_mul(8);
    buf[buf_len] = 0x80;
    if buf_len + 1 > BLOCK_LEN - 8 {
        buf[buf_len + 1..].fill(0);
        compress(&mut state, &buf);
        buf.fill(0);
    } else {
        buf[buf_len + 1..BLOCK_LEN - 8].fill(0);
    }
    buf[BLOCK_LEN - 8..].copy_from_slice(&bit_len.to_be_bytes());
    compress(&mut state, &buf);

    let mut out = [0_u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *chunk = word.to_be_bytes();
    }
    out
}

#[allow(
    clippy::many_single_char_names,
    reason = "a..h are the FIPS 180-4 working-variable names"
)]
fn compress(state: &mut [u32; 8], block: &[u8; BLOCK_LEN]) {
    let mut w = [0_u32; 64];
    for (slot, chunk) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *slot = u32::from_be_bytes(*chunk);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (&k, &wi) in K.iter().zip(&w) {
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(k)
            .wrapping_add(wi);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *word = word.wrapping_add(add);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0_u8; 32];
        for (slot, chunk) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
            let text = str::from_utf8(chunk).expect("hex fixture is ASCII");
            *slot = u8::from_str_radix(text, 16).expect("hex fixture digit");
        }
        out
    }

    #[test]
    fn fips_vector_empty_message() {
        assert_eq!(
            digest_parts(&[]),
            hex32("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }

    #[test]
    fn fips_vector_abc() {
        assert_eq!(
            digest_parts(&[b"abc"]),
            hex32("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn fips_vector_two_block_message() {
        assert_eq!(
            digest_parts(&[b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"]),
            hex32("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
        );
    }

    #[test]
    fn fips_vector_one_million_a() {
        let message = vec![b'a'; 1_000_000];
        assert_eq!(
            digest_parts(&[&message]),
            hex32("cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0")
        );
    }

    #[test]
    fn split_parts_digest_like_their_concatenation() {
        // Longer than two blocks, so splits cover carry fill, carry flush,
        // and whole-block paths.
        let whole: Vec<u8> = (0..150_u8).collect();
        let whole = whole.as_slice();
        let expected = digest_parts(&[whole]);
        // Every split point, including ones straddling the 64-byte block edge.
        for split in 0..whole.len() {
            assert_eq!(
                digest_parts(&[&whole[..split], &whole[split..]]),
                expected,
                "split at {split}"
            );
        }
        assert_eq!(digest_parts(&[b"", whole, b""]), expected);
    }
}
