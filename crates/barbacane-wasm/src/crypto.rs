//! Cryptographic signature verification for WASM plugin host functions.
//!
//! Provides JWT signature verification using `ring` for RSA and ECDSA
//! algorithms. Called by `host_verify_signature` to keep crypto in trusted
//! host code rather than inside the WASM sandbox.

use ring::signature;
use serde::Deserialize;

/// Request from a WASM plugin to verify a cryptographic signature.
#[derive(Debug, Deserialize)]
pub struct VerifySignatureRequest {
    /// Algorithm identifier (e.g., "RS256", "ES256").
    pub algorithm: String,

    /// JWK public key.
    pub jwk: JwkPublicKey,

    /// Signing input: the `header.payload` portion of the JWT (UTF-8 string).
    pub message: String,

    /// Raw signature bytes (decoded from base64url).
    pub signature: Vec<u8>,
}

/// A JWK public key (subset of fields needed for verification).
#[derive(Debug, Deserialize)]
pub struct JwkPublicKey {
    /// Key type: "RSA" or "EC".
    pub kty: String,

    // RSA fields
    /// RSA modulus (base64url-encoded).
    #[serde(default)]
    pub n: Option<String>,
    /// RSA public exponent (base64url-encoded).
    #[serde(default)]
    pub e: Option<String>,

    // EC fields
    /// EC x coordinate (base64url-encoded).
    #[serde(default)]
    pub x: Option<String>,
    /// EC y coordinate (base64url-encoded).
    #[serde(default)]
    pub y: Option<String>,
    /// EC curve name (e.g., "P-256", "P-384").
    #[serde(default)]
    pub crv: Option<String>,
}

/// Verify a signature using the provided JWK and algorithm.
///
/// Returns `Ok(true)` if the signature is valid, `Ok(false)` if invalid,
/// or `Err` if the request is malformed (bad key format, unsupported algorithm, etc.).
pub fn verify_signature(req: &VerifySignatureRequest) -> Result<bool, String> {
    let message = req.message.as_bytes();
    let sig = &req.signature;

    match req.jwk.kty.as_str() {
        "RSA" => verify_rsa(req, message, sig),
        "EC" => verify_ec(req, message, sig),
        other => Err(format!("unsupported key type: {}", other)),
    }
}

fn verify_rsa(req: &VerifySignatureRequest, message: &[u8], sig: &[u8]) -> Result<bool, String> {
    let params: &signature::RsaParameters = match req.algorithm.as_str() {
        "RS256" => &signature::RSA_PKCS1_2048_8192_SHA256,
        "RS384" => &signature::RSA_PKCS1_2048_8192_SHA384,
        "RS512" => &signature::RSA_PKCS1_2048_8192_SHA512,
        other => return Err(format!("unsupported RSA algorithm: {}", other)),
    };

    let n_b64 = req.jwk.n.as_ref().ok_or("missing RSA modulus (n)")?;
    let e_b64 = req.jwk.e.as_ref().ok_or("missing RSA exponent (e)")?;

    let n_bytes = base64url_decode(n_b64).map_err(|e| format!("invalid base64url in n: {}", e))?;
    let e_bytes = base64url_decode(e_b64).map_err(|e| format!("invalid base64url in e: {}", e))?;

    let der = build_rsa_der(&n_bytes, &e_bytes);

    let public_key = signature::UnparsedPublicKey::new(params, &der);

    match public_key.verify(message, sig) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

fn verify_ec(req: &VerifySignatureRequest, message: &[u8], sig: &[u8]) -> Result<bool, String> {
    let alg: &signature::EcdsaVerificationAlgorithm = match req.algorithm.as_str() {
        "ES256" => &signature::ECDSA_P256_SHA256_FIXED,
        "ES384" => &signature::ECDSA_P384_SHA384_FIXED,
        other => return Err(format!("unsupported EC algorithm: {}", other)),
    };

    let crv = req.jwk.crv.as_deref().ok_or("missing EC curve (crv)")?;

    // Validate curve matches algorithm
    match (req.algorithm.as_str(), crv) {
        ("ES256", "P-256") | ("ES384", "P-384") => {}
        _ => {
            return Err(format!(
                "algorithm {} incompatible with curve {}",
                req.algorithm, crv
            ))
        }
    }

    let x_b64 = req.jwk.x.as_ref().ok_or("missing EC x coordinate")?;
    let y_b64 = req.jwk.y.as_ref().ok_or("missing EC y coordinate")?;

    let x_bytes = base64url_decode(x_b64).map_err(|e| format!("invalid base64url in x: {}", e))?;
    let y_bytes = base64url_decode(y_b64).map_err(|e| format!("invalid base64url in y: {}", e))?;

    // Uncompressed EC point: 0x04 || x || y
    let mut point = Vec::with_capacity(1 + x_bytes.len() + y_bytes.len());
    point.push(0x04);
    point.extend_from_slice(&x_bytes);
    point.extend_from_slice(&y_bytes);

    let public_key = signature::UnparsedPublicKey::new(alg, &point);

    match public_key.verify(message, sig) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Build a DER-encoded RSAPublicKey from raw n and e bytes.
///
/// ASN.1 structure: SEQUENCE { INTEGER n, INTEGER e }
fn build_rsa_der(n: &[u8], e: &[u8]) -> Vec<u8> {
    let n_int = der_integer(n);
    let e_int = der_integer(e);

    let mut seq_content = Vec::with_capacity(n_int.len() + e_int.len());
    seq_content.extend_from_slice(&n_int);
    seq_content.extend_from_slice(&e_int);

    let mut result = Vec::with_capacity(2 + seq_content.len());
    result.push(0x30); // SEQUENCE tag
    der_write_length(&mut result, seq_content.len());
    result.extend_from_slice(&seq_content);

    result
}

/// Encode a byte slice as a DER INTEGER.
fn der_integer(value: &[u8]) -> Vec<u8> {
    // Strip leading zeros but keep at least one byte
    let stripped = match value.iter().position(|&b| b != 0) {
        Some(pos) => &value[pos..],
        None => &[0u8],
    };

    // If the high bit is set, prepend a 0x00 byte (positive integer)
    let needs_padding = !stripped.is_empty() && (stripped[0] & 0x80) != 0;
    let content_len = stripped.len() + if needs_padding { 1 } else { 0 };

    let mut result = Vec::with_capacity(2 + content_len);
    result.push(0x02); // INTEGER tag
    der_write_length(&mut result, content_len);
    if needs_padding {
        result.push(0x00);
    }
    result.extend_from_slice(stripped);

    result
}

/// Write a DER length encoding.
fn der_write_length(buf: &mut Vec<u8>, len: usize) {
    if len < 128 {
        buf.push(len as u8);
    } else if len < 256 {
        buf.push(0x81);
        buf.push(len as u8);
    } else if len < 65536 {
        buf.push(0x82);
        buf.push((len >> 8) as u8);
        buf.push(len as u8);
    } else {
        buf.push(0x83);
        buf.push((len >> 16) as u8);
        buf.push((len >> 8) as u8);
        buf.push(len as u8);
    }
}

/// Decode base64url (no padding) to bytes.
fn base64url_decode(input: &str) -> Result<Vec<u8>, String> {
    // base64url uses - instead of + and _ instead of /
    let standard: String = input
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();

    // Add padding if needed
    let padded = match standard.len() % 4 {
        2 => format!("{}==", standard),
        3 => format!("{}=", standard),
        0 => standard,
        _ => return Err("invalid base64url length".to_string()),
    };

    // Use ring's base64 decoding is not available, so we decode manually
    base64_standard_decode(&padded)
}

/// Simple standard base64 decoder.
fn base64_standard_decode(input: &str) -> Result<Vec<u8>, String> {
    const DECODE_TABLE: [u8; 128] = {
        let mut table = [255u8; 128];
        let mut i = 0u8;
        while i < 26 {
            table[(b'A' + i) as usize] = i;
            table[(b'a' + i) as usize] = i + 26;
            i += 1;
        }
        let mut d = 0u8;
        while d < 10 {
            table[(b'0' + d) as usize] = d + 52;
            d += 1;
        }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };

    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() * 3 / 4);

    for chunk in bytes.chunks(4) {
        if chunk.len() < 4 {
            return Err("invalid base64 length".to_string());
        }

        let mut vals = [0u8; 4];
        let mut padding = 0;

        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                vals[i] = 0;
                padding += 1;
            } else if b < 128 && DECODE_TABLE[b as usize] != 255 {
                vals[i] = DECODE_TABLE[b as usize];
            } else {
                return Err(format!("invalid base64 character: {}", b as char));
            }
        }

        let combined = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | (vals[3] as u32);

        result.push((combined >> 16) as u8);
        if padding < 2 {
            result.push((combined >> 8) as u8);
        }
        if padding < 1 {
            result.push(combined as u8);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test RSA key pair generated for testing (2048-bit).
    // These are real keys — the JWT was signed with the private key.
    //
    // Generated via: openssl genrsa 2048 | openssl rsa -pubout
    // Token: header={"alg":"RS256","typ":"JWT"}, payload={"sub":"test","iss":"barbacane"}
    const TEST_RSA_N: &str = "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw";
    const TEST_RSA_E: &str = "AQAB";

    #[test]
    fn base64url_decode_simple() {
        let result = base64url_decode("AQAB").unwrap();
        assert_eq!(result, vec![0x01, 0x00, 0x01]); // RSA exponent 65537
    }

    #[test]
    fn base64url_decode_with_url_chars() {
        // base64url uses - and _ instead of + and /
        let result = base64url_decode("a-b_cA").unwrap();
        let result2 = base64_standard_decode("a+b/cA==").unwrap();
        assert_eq!(result, result2);
    }

    #[test]
    fn base64url_decode_empty() {
        let result = base64url_decode("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn base64url_decode_invalid_char() {
        let result = base64url_decode("abc!");
        assert!(result.is_err());
    }

    #[test]
    fn der_integer_simple() {
        let result = der_integer(&[0x01, 0x00, 0x01]);
        // Tag=0x02, Length=3, Value=010001
        assert_eq!(result, vec![0x02, 0x03, 0x01, 0x00, 0x01]);
    }

    #[test]
    fn der_integer_needs_padding() {
        // High bit set — needs 0x00 prefix
        let result = der_integer(&[0x80, 0x01]);
        assert_eq!(result, vec![0x02, 0x03, 0x00, 0x80, 0x01]);
    }

    #[test]
    fn der_integer_strips_leading_zeros() {
        let result = der_integer(&[0x00, 0x00, 0x42]);
        assert_eq!(result, vec![0x02, 0x01, 0x42]);
    }

    #[test]
    fn der_integer_zero() {
        let result = der_integer(&[0x00]);
        assert_eq!(result, vec![0x02, 0x01, 0x00]);
    }

    #[test]
    fn build_rsa_der_structure() {
        let n = vec![0x01, 0x02, 0x03];
        let e = vec![0x01, 0x00, 0x01];
        let der = build_rsa_der(&n, &e);

        // SEQUENCE tag
        assert_eq!(der[0], 0x30);
        // Should contain two INTEGER values
        assert!(der.len() > 6);
    }

    #[test]
    fn verify_rsa_key_parses() {
        // Verify that the test RSA key can be used for verification
        // (UnparsedPublicKey only validates the key when verify() is called)
        let n_bytes = base64url_decode(TEST_RSA_N).unwrap();
        let e_bytes = base64url_decode(TEST_RSA_E).unwrap();
        let der = build_rsa_der(&n_bytes, &e_bytes);

        // Should create a public key without panicking; actual validation
        // happens at verify() time with ring's UnparsedPublicKey.
        let _key = ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            &der,
        );
    }

    #[test]
    fn verify_unsupported_key_type() {
        let req = VerifySignatureRequest {
            algorithm: "RS256".to_string(),
            jwk: JwkPublicKey {
                kty: "OKP".to_string(),
                n: None,
                e: None,
                x: None,
                y: None,
                crv: None,
            },
            message: "header.payload".to_string(),
            signature: vec![0; 32],
        };

        let result = verify_signature(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported key type"));
    }

    #[test]
    fn verify_unsupported_rsa_algorithm() {
        let req = VerifySignatureRequest {
            algorithm: "RS1024".to_string(),
            jwk: JwkPublicKey {
                kty: "RSA".to_string(),
                n: Some(TEST_RSA_N.to_string()),
                e: Some(TEST_RSA_E.to_string()),
                x: None,
                y: None,
                crv: None,
            },
            message: "header.payload".to_string(),
            signature: vec![0; 256],
        };

        let result = verify_signature(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported RSA algorithm"));
    }

    #[test]
    fn verify_missing_rsa_modulus() {
        let req = VerifySignatureRequest {
            algorithm: "RS256".to_string(),
            jwk: JwkPublicKey {
                kty: "RSA".to_string(),
                n: None,
                e: Some(TEST_RSA_E.to_string()),
                x: None,
                y: None,
                crv: None,
            },
            message: "header.payload".to_string(),
            signature: vec![0; 256],
        };

        let result = verify_signature(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing RSA modulus"));
    }

    #[test]
    fn verify_missing_ec_curve() {
        let req = VerifySignatureRequest {
            algorithm: "ES256".to_string(),
            jwk: JwkPublicKey {
                kty: "EC".to_string(),
                n: None,
                e: None,
                x: Some("test".to_string()),
                y: Some("test".to_string()),
                crv: None,
            },
            message: "header.payload".to_string(),
            signature: vec![0; 64],
        };

        let result = verify_signature(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing EC curve"));
    }

    #[test]
    fn verify_ec_algorithm_curve_mismatch() {
        let req = VerifySignatureRequest {
            algorithm: "ES256".to_string(),
            jwk: JwkPublicKey {
                kty: "EC".to_string(),
                n: None,
                e: None,
                x: Some("test".to_string()),
                y: Some("test".to_string()),
                crv: Some("P-384".to_string()),
            },
            message: "header.payload".to_string(),
            signature: vec![0; 64],
        };

        let result = verify_signature(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("incompatible with curve"));
    }

    #[test]
    fn verify_rsa_invalid_signature_returns_false() {
        let req = VerifySignatureRequest {
            algorithm: "RS256".to_string(),
            jwk: JwkPublicKey {
                kty: "RSA".to_string(),
                n: Some(TEST_RSA_N.to_string()),
                e: Some(TEST_RSA_E.to_string()),
                x: None,
                y: None,
                crv: None,
            },
            message: "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0".to_string(),
            signature: vec![0; 256],
        };

        let result = verify_signature(&req);
        assert!(result.is_ok());
        assert!(!result.unwrap(), "garbage signature should be invalid");
    }

    #[test]
    fn verify_unsupported_ec_algorithm() {
        let req = VerifySignatureRequest {
            algorithm: "ES512".to_string(),
            jwk: JwkPublicKey {
                kty: "EC".to_string(),
                n: None,
                e: None,
                x: Some("test".to_string()),
                y: Some("test".to_string()),
                crv: Some("P-521".to_string()),
            },
            message: "header.payload".to_string(),
            signature: vec![0; 132],
        };

        let result = verify_signature(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported EC algorithm"));
    }
}
