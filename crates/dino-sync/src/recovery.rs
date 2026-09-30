//! The recovery key: shown once when sync starts, for signing in when no other device is around.
//!
//! `D1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX`: 128 random bits in Crockford base32 (no I, L, O, U,
//! case and dashes ignored, 0/O and 1/I/L read alike) and a 10-bit checksum that catches typos.
//! Its shape follows 1Password's Secret Key (<https://agilebits.github.io/security-design/secretKey.html>).
//!
//! The account key is stored on the server wrapped with a key derived from it. The recovery key is
//! 128 random bits, not a password, so there's nothing for a slow hash like Argon2id to protect:
//! stretching only adds cost to guessing a low-entropy secret (why Bitwarden stretches master
//! passwords). HKDF-SHA256 (RFC 5869) is the right tool for a uniformly random secret, as
//! 1Password does with its Secret Key.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use blake2::digest::{Update, VariableOutput};
use hkdf::Hkdf;
use pasetors::keys::SymmetricKey;
use pasetors::token::UntrustedToken;
use pasetors::version4::V4;
use pasetors::Local;
use pasetors::version4::LocalToken;
use rand_core::{CryptoRngCore, OsRng};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::crypto::{AccountKey, CryptoError};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const PREFIX: &str = "D1";
const DATA_CHARS: usize = 26;
const CHECK_CHARS: usize = 2;

#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct RecoveryKey([u8; 16]);

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryKey(…)")
    }
}

impl RecoveryKey {
    pub fn generate() -> Self {
        Self::generate_with(&mut OsRng)
    }

    pub fn generate_with(rng: &mut impl CryptoRngCore) -> Self {
        let mut b = [0u8; 16];
        rng.fill_bytes(&mut b);
        Self(b)
    }

    /// As shown to the person: `D1-` and seven groups of four.
    pub fn display(&self) -> String {
        let n = u128::from_be_bytes(self.0);
        let mut chars: Vec<u8> = (0..DATA_CHARS).rev().map(|i| ALPHABET[((n >> (5 * i)) & 31) as usize]).collect();
        let check = checksum(&self.0);
        chars.extend([ALPHABET[(check >> 5) as usize & 31], ALPHABET[check as usize & 31]]);
        let groups: Vec<String> = chars.chunks(4).map(|c| String::from_utf8(c.to_vec()).expect("ascii")).collect();
        format!("{PREFIX}-{}", groups.join("-"))
    }

    /// Reads a recovery key as typed or pasted.
    pub fn parse(text: &str) -> Result<Self, CryptoError> {
        let mut cleaned: String = text.chars().filter(|c| !c.is_whitespace() && *c != '-').map(|c| c.to_ascii_uppercase()).collect();
        if !cleaned.starts_with(PREFIX) {
            return Err(CryptoError::BadRecoveryKey("it starts with D1"));
        }
        cleaned.drain(..PREFIX.len());
        if cleaned.len() != DATA_CHARS + CHECK_CHARS {
            return Err(CryptoError::BadRecoveryKey("it's 28 characters after D1"));
        }
        let mut digits = Vec::with_capacity(cleaned.len());
        for c in cleaned.chars() {
            let c = match c {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            let d = ALPHABET.iter().position(|a| *a as char == c).ok_or(CryptoError::BadRecoveryKey("a character isn't in its alphabet"))?;
            digits.push(d as u128);
        }
        if digits[0] > 7 {
            return Err(CryptoError::BadRecoveryKey("a character is mistyped"));
        }
        let n = digits[..DATA_CHARS].iter().fold(0u128, |n, d| (n << 5) | d);
        let bytes = n.to_be_bytes();
        let check = (digits[DATA_CHARS] << 5 | digits[DATA_CHARS + 1]) as u16;
        if check != checksum(&bytes) {
            return Err(CryptoError::BadRecoveryKey("a character is mistyped"));
        }
        Ok(Self(bytes))
    }

    fn wrapping_key(&self, account: &str) -> Zeroizing<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(Some(b"dino-sync recovery v1"), &self.0);
        let mut okm = Zeroizing::new([0u8; 32]);
        hk.expand(account.as_bytes(), okm.as_mut()).expect("32 bytes");
        okm
    }

    /// The account key, wrapped for the server to keep.
    pub fn wrap(&self, account: &str, key: &AccountKey) -> Result<String, CryptoError> {
        let k = SymmetricKey::<V4>::from(self.wrapping_key(account).as_ref()).map_err(|_| CryptoError::Invalid)?;
        let payload = Zeroizing::new(serde_json::to_string(&Wrapped { kid: key.id.clone(), key: B64.encode(key.bytes()) }).expect("wrapped"));
        LocalToken::encrypt(&k, payload.as_bytes(), None, Some(&assertion(account))).map_err(|_| CryptoError::Invalid)
    }

    /// The account key back from what the server kept.
    pub fn unwrap(&self, account: &str, wrapped: &str) -> Result<AccountKey, CryptoError> {
        let k = SymmetricKey::<V4>::from(self.wrapping_key(account).as_ref()).map_err(|_| CryptoError::Invalid)?;
        let token = UntrustedToken::<Local, V4>::try_from(wrapped).map_err(|_| CryptoError::Invalid)?;
        let trusted = LocalToken::decrypt(&k, &token, None, Some(&assertion(account))).map_err(|_| CryptoError::Invalid)?;
        let w: Wrapped = serde_json::from_str(trusted.payload()).map_err(|_| CryptoError::Invalid)?;
        let bytes = Zeroizing::new(B64.decode(&w.key).map_err(|_| CryptoError::Invalid)?);
        let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| CryptoError::Invalid)?;
        Ok(AccountKey::from_bytes(w.kid, bytes))
    }
}

#[derive(Serialize, Deserialize)]
struct Wrapped {
    kid: String,
    key: String,
}

fn assertion(account: &str) -> Vec<u8> {
    serde_json::to_vec(&("recovery", account)).expect("assertion")
}

/// 10 bits of BLAKE2b over the key.
fn checksum(bytes: &[u8; 16]) -> u16 {
    let mut h = blake2::Blake2bVar::new(2).expect("length");
    h.update(b"dino-sync recovery check v1");
    h.update(bytes);
    let mut out = [0u8; 2];
    h.finalize_variable(&mut out).expect("length");
    u16::from_le_bytes(out) & 0x3ff
}
