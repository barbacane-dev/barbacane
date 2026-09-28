//! SHA-256 of plugin memory, computed by the host (`host_sha256`, capability
//! `hash`).
//!
//! Hashing inside a plugin costs fuel for every byte, and a call's budget does
//! not grow with the request body, which travels outside the metered JSON. A
//! plugin hashing a body of more than a few hundred kilobytes therefore runs
//! out of fuel. The host hashes the plugin's memory natively instead, at a
//! fixed cost to the plugin.

use std::ops::Range;

use sha2::{Digest, Sha256};

/// Length of a SHA-256 digest, in bytes.
pub const DIGEST_LEN: usize = 32;

/// Why a range of plugin memory was refused. `host_sha256` returns -1 for
/// either, and writes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// A pointer or a length was negative. Plugin memory is bounded well below
    /// 2 GiB, so a negative `i32` never names a valid address.
    Negative,
    /// The range ends past the end of the plugin's memory.
    OutOfBounds,
}

/// The range `[ptr, ptr + len)` of a memory `memory_len` bytes long.
pub fn guest_range(memory_len: usize, ptr: i32, len: i32) -> Result<Range<usize>, RangeError> {
    if ptr < 0 || len < 0 {
        return Err(RangeError::Negative);
    }
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or(RangeError::OutOfBounds)?;
    if end > memory_len {
        return Err(RangeError::OutOfBounds);
    }
    Ok(start..end)
}

/// Hashes `[data_ptr, data_ptr + data_len)` of `memory` and writes the digest
/// at `out_ptr`.
///
/// Both ranges are checked before anything is written, so a refused call
/// leaves memory untouched. The input is hashed before the digest is written,
/// so the two ranges may overlap.
pub fn sha256_into(
    memory: &mut [u8],
    data_ptr: i32,
    data_len: i32,
    out_ptr: i32,
) -> Result<(), RangeError> {
    let data = guest_range(memory.len(), data_ptr, data_len)?;
    let out = guest_range(memory.len(), out_ptr, DIGEST_LEN as i32)?;
    let digest = Sha256::digest(&memory[data]);
    memory[out].copy_from_slice(&digest);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An independent SHA-256, so the tests do not check sha2 against itself.
    fn oracle(data: &[u8]) -> Vec<u8> {
        ring::digest::digest(&ring::digest::SHA256, data)
            .as_ref()
            .to_vec()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Memory holding `data` at offset 0 and room for a digest after it.
    fn memory_with(data: &[u8]) -> Vec<u8> {
        let mut memory = data.to_vec();
        memory.extend_from_slice(&[0u8; DIGEST_LEN]);
        memory
    }

    fn hash_at_start(data: &[u8]) -> Vec<u8> {
        let mut memory = memory_with(data);
        let out = data.len() as i32;
        sha256_into(&mut memory, 0, data.len() as i32, out).expect("in bounds");
        memory[data.len()..].to_vec()
    }

    // ── Known answers ─────────────────────────────────────────────────────

    #[test]
    fn matches_the_published_test_vectors() {
        // FIPS 180-2 examples and the empty message.
        let cases: &[(&[u8], &str)] = &[
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                hex(&hash_at_start(input)),
                *expected,
                "{:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn hashes_a_million_bytes() {
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&hash_at_start(&million_a)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn agrees_with_an_independent_implementation_across_block_boundaries() {
        // SHA-256 pads in 64-byte blocks; 55, 56, 63, 64 and 65 bytes are where
        // the padding changes shape.
        let data: Vec<u8> = (0..=u8::MAX).cycle().take(4 * 64 + 1).collect();
        for len in 0..data.len() {
            assert_eq!(
                hash_at_start(&data[..len]),
                oracle(&data[..len]),
                "length {len}"
            );
        }
    }

    #[test]
    fn agrees_with_an_independent_implementation_on_a_large_input() {
        let data: Vec<u8> = (0..20 * 1024 * 1024)
            .map(|i| (i * 31 % 251) as u8)
            .collect();
        assert_eq!(hash_at_start(&data), oracle(&data));
    }

    // ── Where the input and the digest are ────────────────────────────────

    #[test]
    fn hashes_a_range_in_the_middle_of_memory() {
        let mut memory = vec![0xAAu8; 256];
        memory[100..103].copy_from_slice(b"abc");
        sha256_into(&mut memory, 100, 3, 200).expect("in bounds");
        assert_eq!(memory[200..232], oracle(b"abc")[..]);
        assert!(
            memory[..100].iter().all(|b| *b == 0xAA),
            "bytes before the input are untouched"
        );
        assert!(
            memory[232..].iter().all(|b| *b == 0xAA),
            "bytes after the digest are untouched"
        );
    }

    #[test]
    fn an_empty_input_can_sit_anywhere_including_the_end() {
        let len = 64;
        for ptr in [0, 17, len] {
            let mut memory = vec![0u8; len as usize];
            sha256_into(&mut memory, ptr, 0, 0).expect("an empty range is in bounds");
            assert_eq!(memory[..32], oracle(b"")[..], "empty input at {ptr}");
        }
    }

    #[test]
    fn the_digest_may_overwrite_its_own_input() {
        let input = b"the digest lands on top of these bytes, all of them";
        let mut memory = input.to_vec();
        sha256_into(&mut memory, 0, input.len() as i32, 0).expect("in bounds");
        assert_eq!(memory[..32], oracle(input)[..]);
    }

    #[test]
    fn the_digest_may_end_exactly_at_the_end_of_memory() {
        let mut memory = vec![0u8; 100];
        sha256_into(&mut memory, 0, 10, 68).expect("68 + 32 == 100");
        assert_eq!(memory[68..], oracle(&[0u8; 10])[..]);
    }

    // ── Refused ranges ────────────────────────────────────────────────────

    fn refused(memory_len: usize, data_ptr: i32, data_len: i32, out_ptr: i32) -> RangeError {
        let mut memory: Vec<u8> = (0..memory_len).map(|i| i as u8).collect();
        let before = memory.clone();
        let err = sha256_into(&mut memory, data_ptr, data_len, out_ptr)
            .expect_err("the call must be refused");
        assert_eq!(memory, before, "a refused call writes nothing");
        err
    }

    #[test]
    fn a_negative_pointer_or_length_is_refused() {
        assert_eq!(refused(128, -1, 4, 64), RangeError::Negative);
        assert_eq!(refused(128, 0, -1, 64), RangeError::Negative);
        assert_eq!(refused(128, 0, 4, -1), RangeError::Negative);
        assert_eq!(refused(128, i32::MIN, 0, 64), RangeError::Negative);
    }

    #[test]
    fn an_input_past_the_end_of_memory_is_refused() {
        assert_eq!(refused(128, 0, 129, 64), RangeError::OutOfBounds);
        assert_eq!(refused(128, 100, 29, 0), RangeError::OutOfBounds);
        assert_eq!(refused(128, 129, 0, 0), RangeError::OutOfBounds);
    }

    #[test]
    fn a_digest_that_would_not_fit_is_refused() {
        assert_eq!(refused(128, 0, 4, 97), RangeError::OutOfBounds);
        assert_eq!(refused(128, 0, 4, 128), RangeError::OutOfBounds);
        // Memory too small to hold any digest, even for an empty input.
        assert_eq!(refused(31, 0, 0, 0), RangeError::OutOfBounds);
    }

    #[test]
    fn ranges_whose_end_overflows_are_refused() {
        assert_eq!(refused(128, i32::MAX, i32::MAX, 0), RangeError::OutOfBounds);
        assert_eq!(refused(128, 0, 4, i32::MAX), RangeError::OutOfBounds);
    }

    #[test]
    fn guest_range_names_exactly_the_bytes_asked_for() {
        assert_eq!(guest_range(10, 2, 3), Ok(2..5));
        assert_eq!(guest_range(10, 10, 0), Ok(10..10));
        assert_eq!(guest_range(10, 0, 10), Ok(0..10));
        assert_eq!(guest_range(10, 0, 11), Err(RangeError::OutOfBounds));
        assert_eq!(guest_range(0, 0, 0), Ok(0..0));
    }
}
