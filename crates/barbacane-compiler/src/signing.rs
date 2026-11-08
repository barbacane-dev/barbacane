//! Ed25519 keys that sign artifacts.
//!
//! A signing key is a PKCS#8 document, DER or PEM, in version 1 (as
//! `openssl genpkey -algorithm ed25519` writes it) or version 2 (as
//! [`generate_signing_key`] writes it). A data plane pins the matching public
//! key, hex-encoded, in `BARBACANE_TRUSTED_PUBKEY`.

use base64::Engine as _;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Generate a signing key, as a PKCS#8 v2 DER document.
pub fn generate_signing_key() -> Result<Vec<u8>, String> {
    let rng = ring::rand::SystemRandom::new();
    Ed25519KeyPair::generate_pkcs8(&rng)
        .map(|doc| doc.as_ref().to_vec())
        .map_err(|e| format!("generating an Ed25519 key: {e}"))
}

/// The public key of the signing key in `key_file`, hex-encoded, as
/// `BARBACANE_TRUSTED_PUBKEY` takes it.
pub fn signing_public_key(key_file: &[u8]) -> Result<String, String> {
    let key = load_signing_key(key_file)?;
    Ok(hex::encode(key.public_key().as_ref()))
}

/// Read a signing key from the bytes of a PKCS#8 file, DER or PEM.
pub(crate) fn load_signing_key(key_file: &[u8]) -> Result<Ed25519KeyPair, String> {
    let der = pkcs8_der(key_file)?;
    Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der)
        .map_err(|e| format!("not a PKCS#8 Ed25519 private key: {e}"))
}

/// The DER bytes of a PKCS#8 file: the file itself, or the body of a PEM
/// `PRIVATE KEY` block.
fn pkcs8_der(key_file: &[u8]) -> Result<Vec<u8>, String> {
    let pem_body = std::str::from_utf8(key_file).ok().and_then(|text| {
        text.trim()
            .strip_prefix("-----BEGIN PRIVATE KEY-----")?
            .strip_suffix("-----END PRIVATE KEY-----")
    });
    match pem_body {
        Some(body) => {
            let encoded: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| format!("the PEM block is not base64: {e}"))
        }
        None => Ok(key_file.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The PKCS#8 v1 private key of RFC 8410 section 10.3, as `openssl genpkey`
    /// writes one.
    const RFC8410_V1_DER: &str =
        "302e020100300506032b657004220420d4ee72dbf913584ad5b6d8f1f769f8ad3afe7c28cbf1d4fbe097a88f44755842";

    /// Its public key, from the same section.
    const RFC8410_PUBLIC: &str = "19bf44096984cdfe8541bac167dc3b96c85086aa30b6b6cb0c5c38ad703166e1";

    #[test]
    fn a_generated_key_loads_and_has_a_32_byte_public_key() {
        let key = generate_signing_key().unwrap();
        let public = signing_public_key(&key).unwrap();
        assert_eq!(public.len(), 64);
        assert_ne!(
            public,
            signing_public_key(&generate_signing_key().unwrap()).unwrap()
        );
    }

    #[test]
    fn a_version_1_der_key_loads() {
        let der = hex::decode(RFC8410_V1_DER).unwrap();
        assert_eq!(signing_public_key(&der).unwrap(), RFC8410_PUBLIC);
    }

    #[test]
    fn a_pem_key_loads() {
        let der = hex::decode(RFC8410_V1_DER).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(der)
        );
        assert_eq!(signing_public_key(pem.as_bytes()).unwrap(), RFC8410_PUBLIC);
    }

    #[test]
    fn anything_else_is_refused() {
        assert!(signing_public_key(b"not a key").is_err());
        assert!(
            signing_public_key(b"-----BEGIN PRIVATE KEY-----\n!!\n-----END PRIVATE KEY-----")
                .is_err()
        );
    }
}
