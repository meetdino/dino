//! A new device gets the account's key from one already signed in, the way Bitwarden's "log in
//! with device" works (<https://bitwarden.com/help/about-trusted-devices/>):
//!
//! 1. Each device has its own X25519 key pair; the server relays public keys.
//! 2. Both screens show a code made from both public keys. When they match, neither key was
//!    swapped on the way, so the person approving knows who they're giving the key to.
//! 3. The approving device encrypts the account key to the new one with NaCl's crypto_box
//!    (X25519 + XSalsa20-Poly1305). Unlike a sealed box, it's authenticated by the approving
//!    device's key, so the new device can check the grant came from the device whose code it saw.
//!
//! Six digits are about 20 bits: a code over the public keys alone could be matched by a server
//! that makes a million key pairs of its own and swaps in the one whose code comes out the same.
//! So the code also covers a fresh random nonce from each device, and the new device commits to
//! its key and nonce before it sees the approver's nonce (commit, then reveal, as in ZRTP's short
//! authentication string). Each side's nonce is fixed before the other's is known, so nobody,
//! the server in between included, can steer the code: a swapped key goes unnoticed one time in a
//! million, and every try needs the person to approve.
//!
//! The messages, in this order, all through the server:
//!
//! 1. New device → approver: the `Commitment` from `DeviceKeys::commit`. The new device keeps the
//!    `Reveal` that comes with it to itself for now.
//! 2. Approver → new device: its `Response` from `DeviceKeys::respond` (public key and a fresh
//!    nonce), made only once the commitment has arrived. The approver keeps the commitment.
//! 3. New device → approver: the `Reveal` (public key and nonce), sent only once the response has
//!    arrived, and only once per commitment: to try again, start over from 1.
//! 4. The approver checks the reveal against the commitment with `verify_reveal`, and refuses to
//!    go on when it doesn't match. Both screens then show
//!    `approval_code(account, &reveal, &response)`.
//! 5. When the person says the codes match, the approver sends `grant(&reveal.public, …)`, and
//!    the new device opens it with `accept(&grant, &response.public, …)`.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use blake2::digest::{Update, VariableOutput};
use crypto_box::aead::Aead;
use crypto_box::{Nonce, PublicKey, SalsaBox, SecretKey};
use rand_core::{CryptoRngCore, OsRng};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::{AccountKey, CryptoError};

/// Bytes in each device's approval nonce.
pub const NONCE_BYTES: usize = 32;

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

    /// Step 1, on the new device asking for `account`: a fresh nonce, the commitment to send now,
    /// and the reveal to send once the approver's response has arrived (never before).
    pub fn commit(&self, account: &str, rng: &mut impl CryptoRngCore) -> (Commitment, Reveal) {
        let nonce = random_nonce(rng);
        let public = self.secret.public_key();
        let hash = commitment_hash(account, public.as_bytes(), &nonce);
        (Commitment { hash: B64.encode(hash) }, Reveal { public: B64.encode(public.as_bytes()), nonce: B64.encode(nonce) })
    }

    /// Step 2, on the approving device, once a new device's commitment has arrived: this device's
    /// public key and a fresh nonce. Make a new one for every commitment.
    pub fn respond(&self, rng: &mut impl CryptoRngCore) -> Response {
        Response { public: self.public(), nonce: B64.encode(random_nonce(rng)) }
    }

    /// Gives `account` to the device asking with public key `new_device` (the `Reveal`'s, checked
    /// with `verify_reveal`, whose code the person matched).
    pub fn grant(&self, new_device: &str, account: &str, key: &AccountKey, rng: &mut impl CryptoRngCore) -> Result<Grant, CryptoError> {
        let theirs = public_key(new_device)?;
        let mut nonce = [0u8; 24];
        rng.fill_bytes(&mut nonce);
        let payload = Zeroizing::new(serde_json::to_vec(&Transfer { account: account.into(), kid: key.id.clone(), key: B64.encode(key.bytes()) }).expect("transfer"));
        let sealed = SalsaBox::new(&theirs, &self.secret).encrypt(Nonce::from_slice(&nonce), payload.as_slice()).map_err(|_| CryptoError::Invalid)?;
        Ok(Grant { from: self.public(), nonce: B64.encode(nonce), sealed: B64.encode(sealed) })
    }

    /// Opens a grant for `account`, when it came from `approver` (the `Response`'s public key,
    /// whose code the person matched).
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

/// Message 1, new device → approver: a hash binding the account, the new device's public key and
/// its nonce, which stay hidden until the `Reveal`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Commitment {
    pub hash: String,
}

impl Commitment {
    /// The commitment to `reveal` for `account`.
    pub fn of(account: &str, reveal: &Reveal) -> Result<Self, CryptoError> {
        let (public, nonce) = (public_bytes(&reveal.public)?, nonce_bytes(&reveal.nonce)?);
        Ok(Self { hash: B64.encode(commitment_hash(account, &public, &nonce)) })
    }
}

/// Message 2, approver → new device: the approver's public key and nonce.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub public: String,
    pub nonce: String,
}

/// Message 3, new device → approver: the public key and nonce the commitment was made to.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Reveal {
    pub public: String,
    pub nonce: String,
}

/// Step 4, on the approving device: whether `reveal` is what the new device committed to for
/// `account` before it saw this device's nonce. Show no code, and grant nothing, unless it is.
pub fn verify_reveal(account: &str, commitment: &Commitment, reveal: &Reveal) -> Result<(), CryptoError> {
    let expected = Commitment::of(account, reveal)?;
    let got = B64.decode(&commitment.hash).map_err(|_| CryptoError::Invalid)?;
    let want = B64.decode(&expected.hash).map_err(|_| CryptoError::Invalid)?;
    if got != want {
        return Err(CryptoError::Invalid);
    }
    Ok(())
}

/// The six digits both screens show, as `48-21-93`: from the account, and both devices' public
/// keys and nonces in their roles, so it's the same on both devices and changes if either key or
/// nonce is swapped. The approver calls it only after `verify_reveal`.
pub fn approval_code(account: &str, reveal: &Reveal, response: &Response) -> Result<String, CryptoError> {
    let (new_device, new_nonce) = (public_bytes(&reveal.public)?, nonce_bytes(&reveal.nonce)?);
    let (approver, approver_nonce) = (public_bytes(&response.public)?, nonce_bytes(&response.nonce)?);
    if new_device == approver {
        return Err(CryptoError::Invalid);
    }
    let parts: [&[u8]; 6] = [b"dino-sync approval v2", account.as_bytes(), &new_device, &new_nonce, &approver, &approver_nonce];
    let mut out = [0u8; 8];
    hash(&parts, &mut out);
    let n = u64::from_le_bytes(out) % 1_000_000;
    Ok(format!("{:02}-{:02}-{:02}", n / 10_000, n / 100 % 100, n % 100))
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

/// X25519 public keys of small order (with the top bit cleared, which X25519 ignores), as
/// libsodium lists them: a box with one of them has a shared secret anyone can compute.
const SMALL_ORDER: [[u8; 32]; 7] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49, 0xb8, 0x00],
    [0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83, 0xef, 0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f, 0x11, 0x57],
    [0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
    [0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
    [0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
];

/// A public key's 32 bytes, refusing small-order keys.
fn public_bytes(b64: &str) -> Result<[u8; 32], CryptoError> {
    let bytes = B64.decode(b64).map_err(|_| CryptoError::Invalid)?;
    let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| CryptoError::Invalid)?;
    let mut masked = bytes;
    masked[31] &= 0x7f;
    if SMALL_ORDER.contains(&masked) {
        return Err(CryptoError::Invalid);
    }
    Ok(bytes)
}

fn public_key(b64: &str) -> Result<PublicKey, CryptoError> {
    let bytes = public_bytes(b64)?;
    PublicKey::from_slice(&bytes).map_err(|_| CryptoError::Invalid)
}

fn nonce_bytes(b64: &str) -> Result<[u8; NONCE_BYTES], CryptoError> {
    let bytes = B64.decode(b64).map_err(|_| CryptoError::Invalid)?;
    bytes.as_slice().try_into().map_err(|_| CryptoError::Invalid)
}

fn random_nonce(rng: &mut impl CryptoRngCore) -> [u8; NONCE_BYTES] {
    let mut nonce = [0u8; NONCE_BYTES];
    rng.fill_bytes(&mut nonce);
    nonce
}

fn commitment_hash(account: &str, public: &[u8], nonce: &[u8]) -> [u8; 32] {
    let parts: [&[u8]; 4] = [b"dino-sync approval commit v2", account.as_bytes(), public, nonce];
    let mut out = [0u8; 32];
    hash(&parts, &mut out);
    out
}

/// BLAKE2b with `out.len()` bytes of output over `parts`, each prefixed with its length so no two
/// different lists of parts hash alike.
fn hash(parts: &[&[u8]], out: &mut [u8]) {
    let mut h = blake2::Blake2bVar::new(out.len()).expect("length");
    for part in parts {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part);
    }
    h.finalize_variable(out).expect("length");
}
