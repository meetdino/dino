//! End-to-end encryption. The account's key is made on its first device and never leaves the
//! account's devices in the clear, so the server stores values it can't read.
//!
//! - Values: PASETO v4.local (XChaCha20 + BLAKE2b-MAC, <https://github.com/paseto-standard/paseto-spec>),
//!   with everything the record says about it as the implicit assertion: its place, its schema, its
//!   whole stamp and whether it's a delete. A value can't be moved to another account, setting or
//!   schema, restamped, or turned into a delete without failing to open, and a delete is a token
//!   too, so only a device with the key can make one. The footer names the key, for rotation.
//! - A new device gets the key from one already signed in (`approval`), or from the recovery key
//!   (`recovery`).

use pasetors::keys::SymmetricKey;
use pasetors::token::UntrustedToken;
use pasetors::version4::V4;
use pasetors::Local;
use pasetors::version4::LocalToken;
use rand_core::{CryptoRngCore, OsRng};
use serde::{Deserialize, Serialize};
use serde_json::Map;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::hlc::Hlc;
use crate::record::{Record, RecordId};

/// What a delete's token holds: PASETO payloads can't be empty, and the assertion already says
/// it's a delete.
const TOMBSTONE: &str = "deleted";

/// Why a value or key didn't open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// Written with a key this device doesn't have (rotated, or another account).
    UnknownKey(String),
    /// Tampered with, moved to another setting, or encrypted with another key.
    Invalid,
    /// Empty values can't be encrypted (PASETO forbids empty payloads); store a tombstone instead.
    Empty,
    BadRecoveryKey(&'static str),
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::UnknownKey(kid) => write!(f, "encrypted with key {kid}, which this device doesn't have"),
            CryptoError::Invalid => write!(f, "doesn't open: tampered with or not for this setting"),
            CryptoError::Empty => write!(f, "nothing to encrypt"),
            CryptoError::BadRecoveryKey(why) => write!(f, "not a recovery key: {why}"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// The account's key: 32 random bytes and the id values name it by.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct AccountKey {
    #[zeroize(skip)]
    pub id: String,
    bytes: [u8; 32],
}

impl std::fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountKey").field("id", &self.id).finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct Footer {
    kid: String,
}

impl AccountKey {
    pub fn generate() -> Self {
        Self::generate_with(&mut OsRng)
    }

    pub fn generate_with(rng: &mut impl CryptoRngCore) -> Self {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        let mut id = [0u8; 8];
        rng.fill_bytes(&mut id);
        Self { id: format!("k_{}", hex(&id)), bytes }
    }

    pub fn from_bytes(id: impl Into<String>, bytes: [u8; 32]) -> Self {
        Self { id: id.into(), bytes }
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    fn paseto(&self) -> SymmetricKey<V4> {
        SymmetricKey::<V4>::from(&self.bytes).expect("32 bytes")
    }

    /// A record at `id` in `account` stamped `hlc`, holding `value` (text: PASETO payloads are
    /// UTF-8) in the shape `schema`, or a delete when `value` is None.
    pub fn seal_record(&self, account: &str, id: RecordId, hlc: Hlc, schema: u32, value: Option<&str>) -> Result<Record, CryptoError> {
        let token = self.seal(account, &id, &hlc, schema, value)?;
        Ok(Record { id, hlc, schema, value: token, deleted: value.is_none(), seq: None, extra: Map::new() })
    }

    /// The value `record` holds (None: it's a delete), when its token was sealed with this key for
    /// exactly the account, place, stamp, schema and delete flag the record claims.
    pub fn open_record(&self, account: &str, record: &Record) -> Result<Option<String>, CryptoError> {
        self.open(account, &record.id, &record.hlc, record.schema, record.deleted, &record.value)
    }

    /// Encrypts `value` for the record at `id` in `account`, stamped `hlc`, with the value's
    /// `schema`; None seals a delete.
    pub fn seal(&self, account: &str, id: &RecordId, hlc: &Hlc, schema: u32, value: Option<&str>) -> Result<String, CryptoError> {
        let payload = match value {
            Some("") => return Err(CryptoError::Empty),
            Some(v) => v,
            None => TOMBSTONE,
        };
        let footer = serde_json::to_vec(&Footer { kid: self.id.clone() }).expect("footer");
        let assertion = assertion(account, id, hlc, schema, value.is_none());
        LocalToken::encrypt(&self.paseto(), payload.as_bytes(), Some(&footer), Some(&assertion)).map_err(|_| CryptoError::Invalid)
    }

    /// Opens a token sealed for the record at `id` in `account`, stamped `hlc`, with `schema`, as a
    /// delete or not: the value, or None for a delete.
    pub fn open(&self, account: &str, id: &RecordId, hlc: &Hlc, schema: u32, deleted: bool, token: &str) -> Result<Option<String>, CryptoError> {
        let kid = key_id(token)?;
        if kid != self.id {
            return Err(CryptoError::UnknownKey(kid));
        }
        let untrusted = UntrustedToken::<Local, V4>::try_from(token).map_err(|_| CryptoError::Invalid)?;
        let footer = untrusted.untrusted_footer().to_vec();
        let assertion = assertion(account, id, hlc, schema, deleted);
        let trusted = LocalToken::decrypt(&self.paseto(), &untrusted, Some(&footer), Some(&assertion)).map_err(|_| CryptoError::Invalid)?;
        Ok((!deleted).then(|| trusted.payload().to_string()))
    }
}

/// The key a value was sealed with, read from its (authenticated, unencrypted) footer.
pub fn key_id(token: &str) -> Result<String, CryptoError> {
    let untrusted = UntrustedToken::<Local, V4>::try_from(token).map_err(|_| CryptoError::Invalid)?;
    let footer: Footer = serde_json::from_slice(untrusted.untrusted_footer()).map_err(|_| CryptoError::Invalid)?;
    Ok(footer.kid)
}

/// The implicit assertion binding a value to everything its record claims about it:
/// `[account, collection, key, schema, wall_ms, counter, device, deleted]`. A JSON array, so no
/// separator can make two different records read the same.
pub(crate) fn assertion(account: &str, id: &RecordId, hlc: &Hlc, schema: u32, deleted: bool) -> Vec<u8> {
    serde_json::to_vec(&(account, &id.collection, &id.key, schema, hlc.wall_ms, hlc.counter, &hlc.device, deleted)).expect("assertion")
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
