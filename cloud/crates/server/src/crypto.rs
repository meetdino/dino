//! Tokens, codes and the hashes they're stored as.

use base64::Engine;
use hmac::{Hmac, Mac};
use rand::{Rng, RngCore};
use sha2::{Digest, Sha256};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

/// A new secret token: 256 random bits, base64url, with a prefix saying what it is, so a leaked
/// one is recognisable (and secret scanners can match it).
pub fn token(prefix: &str) -> String {
    format!("{prefix}_{}", B64.encode(random_bytes::<32>()))
}

/// What a token is stored as. Tokens carry 256 bits of randomness, so a plain hash is enough.
pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn hmac(key: &[u8; 32], label: &str, data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    mac.update(label.as_bytes());
    mac.update(&[0]);
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// The refresh token that replaces `parent`. Derived rather than random, so a client that lost
/// the answer and asks again within the grace period gets the same replacement back; nothing
/// but the server key and the parent can produce it.
pub fn next_refresh(key: &[u8; 32], parent: &str) -> String {
    format!("dino_rt_{}", B64.encode(hmac(key, "refresh-rotation", parent.as_bytes())))
}

/// `S256` PKCE: base64url(SHA-256(verifier)) == challenge, compared in constant time.
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    use subtle::ConstantTimeEq;
    // RFC 7636: 43..128 characters from the unreserved set.
    if !(43..=128).contains(&verifier.len()) || !verifier.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)) {
        return false;
    }
    let computed = B64.encode(Sha256::digest(verifier.as_bytes()));
    computed.as_bytes().ct_eq(challenge.as_bytes()).into()
}

pub fn eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

/// A six-digit code for email sign-in.
pub fn email_code() -> String {
    format!("{:06}", rand::rngs::OsRng.gen_range(0..1_000_000u32))
}

/// The alphabet RFC 8628 §6.1 suggests for user codes: no vowels (no words), no look-alikes.
const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

/// A device-flow user code, `XXXX-XXXX` (20^8, about 34.5 bits; guesses are rate limited).
pub fn user_code() -> String {
    let mut rng = rand::rngs::OsRng;
    let mut s = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        s.push(USER_CODE_ALPHABET[rng.gen_range(0..USER_CODE_ALPHABET.len())] as char);
    }
    s
}

/// What someone typed, as a user code: case and separators don't matter.
pub fn normalize_user_code(input: &str) -> Option<String> {
    let chars: String = input.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
    (chars.len() == 8 && chars.bytes().all(|b| USER_CODE_ALPHABET.contains(&b))).then(|| format!("{}-{}", &chars[..4], &chars[4..]))
}

pub fn b64url(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_matches_rfc7636_example() {
        // RFC 7636 Appendix B.
        assert!(pkce_matches("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!pkce_matches("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXx", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!pkce_matches("short", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
    }

    #[test]
    fn user_codes_round_trip_and_reject_lookalikes() {
        for _ in 0..100 {
            let c = user_code();
            assert_eq!(normalize_user_code(&c.to_lowercase().replace('-', " ")).as_deref(), Some(c.as_str()));
        }
        assert_eq!(normalize_user_code("BCDF-GHJ0"), None);
        assert_eq!(normalize_user_code("ABCD-EFGH"), None);
    }

    #[test]
    fn rotation_is_deterministic_and_keyed() {
        let k1 = [1u8; 32];
        let k2 = [2u8; 32];
        assert_eq!(next_refresh(&k1, "dino_rt_a"), next_refresh(&k1, "dino_rt_a"));
        assert_ne!(next_refresh(&k1, "dino_rt_a"), next_refresh(&k1, "dino_rt_b"));
        assert_ne!(next_refresh(&k1, "dino_rt_a"), next_refresh(&k2, "dino_rt_a"));
    }
}
