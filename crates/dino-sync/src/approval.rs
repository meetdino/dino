//! A new device gets the account's key from one already signed in, the way Bitwarden's "log in
//! with device" works (<https://bitwarden.com/help/about-trusted-devices/>):
//!
//! 1. Each device has its own X25519 key pair; the server relays public keys.
//! 2. Both screens show a code made from both public keys. When they match, neither key was
//!    swapped on the way, so the person approving knows who they're giving the key to.
//! 3. The approving device encrypts the account key to the new one with NaCl's crypto_box
//!    (X25519 + XSalsa20-Poly1305). Unlike a sealed box, it's authenticated by the approving
//!    device's key, so the new device can check the grant came from the device whose code it saw.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use blake2::digest::{Update, VariableOutput};
use crypto_box::aead::Aead;
use crypto_box::{Nonce, PublicKey, SalsaBox, SecretKey};
use rand_core::{CryptoRngCore, OsRng};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::{AccountKey, CryptoError};

/// This device's key pair for approvals. The secret stays on the device.
pub struct DeviceKeys {
    secret: SecretKey,
}

impl DeviceKeys {
    pub fn generate() -> Self {
        Self::generate_with(&mut OsRng)
    }

    pub fn generate_with(rng: &mut impl CryptoRngCore) -> Self {
        Self { secret: SecretKey::generate(rng) }
    }

    pub fn from_secret(bytes: [u8; 32]) -> Self {
        Self { secret: SecretKey::from_bytes(bytes) }
    }

    pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.secret.to_bytes())
    }

    /// The public key, as the server and the other devices see it.
    pub fn public(&self) -> String {
        B64.encode(self.secret.public_key().as_bytes())
    }

    /// Gives `account` to the device asking with public key `new_device`.
    pub fn grant(&self, new_device: &str, account: &str, key: &AccountKey, rng: &mut impl CryptoRngCore) -> Result<Grant, CryptoError> {
        let theirs = public_key(new_device)?;
        let mut nonce = [0u8; 24];
        rng.fill_bytes(&mut nonce);
        let payload = Zeroizing::new(serde_json::to_vec(&Transfer { account: account.into(), kid: key.id.clone(), key: B64.encode(key.bytes()) }).expect("transfer"));
        let sealed = SalsaBox::new(&theirs, &self.secret).encrypt(Nonce::from_slice(&nonce), payload.as_slice()).map_err(|_| CryptoError::Invalid)?;
        Ok(Grant { from: self.public(), nonce: B64.encode(nonce), sealed: B64.encode(sealed) })
    }

    /// Opens a grant for `account`, when it came from `approver` (whose code the person matched).
    pub fn accept(&self, grant: &Grant, approver: &str, account: &str) -> Result<AccountKey, CryptoError> {
        if grant.from != approver {
            return Err(CryptoError::Invalid);
        }
        let theirs = public_key(approver)?;
        let nonce = B64.decode(&grant.nonce).ok().filter(|n| n.len() == 24).ok_or(CryptoError::Invalid)?;
        let sealed = B64.decode(&grant.sealed).map_err(|_| CryptoError::Invalid)?;
        let plain = Zeroizing::new(SalsaBox::new(&theirs, &self.secret).decrypt(Nonce::from_slice(&nonce), sealed.as_slice()).map_err(|_| CryptoError::Invalid)?);
        let t: Transfer = serde_json::from_slice(&plain).map_err(|_| CryptoError::Invalid)?;
        if t.account != account {
            return Err(CryptoError::Invalid);
        }
        let bytes = Zeroizing::new(B64.decode(&t.key).map_err(|_| CryptoError::Invalid)?);
        let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| CryptoError::Invalid)?;
        Ok(AccountKey::from_bytes(t.kid, bytes))
    }
}

/// What the approving device sends through the server.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Grant {
    /// The approving device's public key.
    pub from: String,
    pub nonce: String,
    pub sealed: String,
}

#[derive(Serialize, Deserialize)]
struct Transfer {
    account: String,
    kid: String,
    key: String,
}

fn public_key(b64: &str) -> Result<PublicKey, CryptoError> {
    let bytes = B64.decode(b64).map_err(|_| CryptoError::Invalid)?;
    PublicKey::from_slice(&bytes).map_err(|_| CryptoError::Invalid)
}

/// The six digits both screens show, as `48-21-93`: from the account and both public keys, in
/// their roles, so it's the same on both devices and changes if either key is swapped.
pub fn approval_code(account: &str, new_device: &str, approver: &str) -> String {
    let mut h = blake2::Blake2bVar::new(8).expect("length");
    for part in ["dino-sync approval v1", account, new_device, approver] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    let mut out = [0u8; 8];
    h.finalize_variable(&mut out).expect("length");
    let n = u64::from_le_bytes(out) % 1_000_000;
    format!("{:02}-{:02}-{:02}", n / 10_000, n / 100 % 100, n % 100)
}
