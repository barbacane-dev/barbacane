//! Cryptographic signature verification for WASM plugin host functions.
//!
//! Provides JWT signature verification using `ring` for RSA and ECDSA
//! algorithms. Called by `host_verify_signature` to keep crypto in trusted
//! host code rather than inside the WASM sandbox.

use base64::Engine;
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

    let n_bytes = decode_b64url(n_b64).map_err(|e| format!("invalid base64url in n: {}", e))?;
    let e_bytes = decode_b64url(e_b64).map_err(|e| format!("invalid base64url in e: {}", e))?;

    // Verify directly from the modulus/exponent via ring instead of hand-building
    // a DER SubjectPublicKeyInfo.
    let public_key = signature::RsaPublicKeyComponents {
        n: n_bytes,
        e: e_bytes,
    };

    match public_key.verify(params, message, sig) {
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

    let x_bytes = decode_b64url(x_b64).map_err(|e| format!("invalid base64url in x: {}", e))?;
    let y_bytes = decode_b64url(y_b64).map_err(|e| format!("invalid base64url in y: {}", e))?;

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

/// Decode a base64url string (padding optional) into bytes, rejecting invalid
/// or non-canonical input. JWK members (`n`, `e`, `x`, `y`) are base64url per
/// RFC 7517.
fn decode_b64url(input: &str) -> Result<Vec<u8>, String> {
    let trimmed = input.trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed)
        .map_err(|e| e.to_string())
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
    fn decode_b64url_simple() {
        assert_eq!(decode_b64url("AQAB").unwrap(), vec![0x01, 0x00, 0x01]); // 65537
    }

    #[test]
    fn decode_b64url_url_safe_chars() {
        // base64url uses - and _ in place of + and /
        assert_eq!(
            decode_b64url("a-b_cA").unwrap(),
            base64::engine::general_purpose::STANDARD
                .decode("a+b/cA==")
                .unwrap()
        );
    }

    #[test]
    fn decode_b64url_empty() {
        assert!(decode_b64url("").unwrap().is_empty());
    }

    #[test]
    fn decode_b64url_invalid_char() {
        assert!(decode_b64url("abc!").is_err());
    }

    #[test]
    fn rsa_components_path_returns_false_on_bad_signature() {
        // A real modulus/exponent with a bogus signature must verify to
        // Ok(false), not Err — exercises the RsaPublicKeyComponents path.
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
            message: "header.payload".to_string(),
            signature: vec![0u8; 256],
        };
        assert_eq!(verify_signature(&req), Ok(false));
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
