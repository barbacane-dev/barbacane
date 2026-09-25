//! SHA-256, computed by the host (capability `hash`).
//!
//! Hashing inside a plugin costs fuel for every byte, and the budget of a call
//! does not grow with the request body, so hashing a body of a few hundred
//! kilobytes in WASM runs out of fuel. `sha256` has the host hash the bytes
//! instead. Declare the capability in `plugin.toml`:
//!
//! ```toml
//! [capabilities]
//! host_functions = ["hash"]
//! ```
//!
//! Outside WASM (in a plugin's unit tests) the same functions hash in-process,
//! with the same results.

use sha2::{Digest, Sha256};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    fn host_sha256(data_ptr: i32, data_len: i32, out_ptr: i32) -> i32;
}

/// The SHA-256 digest of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    #[cfg(target_arch = "wasm32")]
    {
        let mut out = [0u8; 32];
        // SAFETY: both pointers name live buffers of the lengths given.
        let rc = unsafe {
            host_sha256(
                data.as_ptr() as i32,
                data.len() as i32,
                out.as_mut_ptr() as i32,
            )
        };
        if rc == 0 {
            return out;
        }
        // The host refuses only ranges outside the plugin's memory, which a
        // slice never is. Hash in-process rather than return a wrong digest.
    }
    Sha256::digest(data).into()
}

/// The SHA-256 digest of `data`, as 64 lowercase hex digits.
pub fn sha256_hex(data: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(64);
    for byte in sha256(data) {
        hex.push(DIGITS[(byte >> 4) as usize] as char);
        hex.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_published_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn hex_is_the_digest_in_lowercase_pairs() {
        let data = b"any input";
        let digest = sha256(data);
        let hex = sha256_hex(data);
        assert_eq!(hex.len(), 64);
        assert!(hex
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
        for (i, byte) in digest.iter().enumerate() {
            assert_eq!(&hex[2 * i..2 * i + 2], format!("{byte:02x}"));
        }
    }

    #[test]
    fn digests_every_byte_value() {
        // A digest byte of 0x00, 0x0f, 0xf0 and 0xff must all encode as two
        // digits; hashing enough inputs makes each nibble appear.
        let mut seen = [false; 16];
        for i in 0u32..64 {
            for c in sha256_hex(&i.to_le_bytes()).bytes() {
                let nibble = if c.is_ascii_digit() {
                    c - b'0'
                } else {
                    c - b'a' + 10
                };
                seen[nibble as usize] = true;
            }
        }
        assert!(seen.iter().all(|s| *s));
    }
}
