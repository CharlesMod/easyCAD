//! PKCE (RFC 7636) with the S256 method.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// A random code verifier: 64 unreserved characters (the RFC allows 43 to 128).
pub fn verifier() -> String {
    random_string(64)
}

/// `BASE64URL(SHA256(verifier))` without padding.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A random `state` value for CSRF protection.
pub fn state() -> String {
    random_string(32)
}

fn random_string(n: usize) -> String {
    let mut rng = rand::rng();
    (0..n).map(|_| UNRESERVED[rng.random_range(0..UNRESERVED.len())] as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_is_valid() {
        let v = verifier();
        assert_eq!(v.len(), 64);
        assert!(v.bytes().all(|b| UNRESERVED.contains(&b)));
        assert_ne!(v, verifier());
    }
}
