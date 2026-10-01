//! Merging: each setting on its own, the latest stamp wins. Applying the same records in any order,
//! any number of times, leaves every device with the same state.
//!
//! A device takes records from the server with `apply_verified`: a record counts only once its
//! token opens under the account key with the stamp, schema and delete flag the record claims, so
//! the server (which has no key) can't forge a delete, restamp an old value to make it win, give
//! devices different values under one stamp, or lock a setting with a made-up schema. The server
//! itself, which can't open anything, merges with `apply_unverified`.
//!
//! The stored record is each setting's high-water mark: records are never removed (a delete is a
//! sealed tombstone), and a record only replaces one with an earlier stamp, so a token the server
//! replays loses to what's already here.

use std::collections::BTreeMap;

use crate::crypto::{AccountKey, CryptoError};
use crate::hlc::FutureStamp;
use crate::record::{Record, RecordId};

/// Why a local change wasn't taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// The stored record was written by a newer dino in a shape this one doesn't know; it's kept.
    NewerSchema { stored: u32, ours: u32 },
    /// The change is stamped earlier than what's stored (a clock that wasn't told about it).
    Stale,
}

/// Why a record from the server was refused by `Store::apply_verified`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// Its token doesn't open with the account key under what the record claims: forged, restamped,
    /// moved, or sealed with a key this device doesn't have.
    Crypto(CryptoError),
    /// Stamped further ahead of this device's clock than `crate::hlc::MAX_SKEW_MS`.
    FutureStamp(FutureStamp),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Crypto(e) => write!(f, "{e}"),
            ApplyError::FutureStamp(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ApplyError {}

impl From<CryptoError> for ApplyError {
    fn from(e: CryptoError) -> Self {
        ApplyError::Crypto(e)
    }
}

impl From<FutureStamp> for ApplyError {
    fn from(e: FutureStamp) -> Self {
        ApplyError::FutureStamp(e)
    }
}

/// The records one device (or the server) holds, one per setting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Store {
    records: BTreeMap<RecordId, Record>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: &RecordId) -> Option<&Record> {
        self.records.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Record> {
        self.records.values()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Takes `incoming` from the server if it's later than what's stored, once it has checked that
    /// its token opens with `key` for `account` under the record's own stamp, schema and delete
    /// flag, and that it isn't stamped further ahead of `now_ms` than `MAX_SKEW_MS`. Ok(true) when
    /// it was taken, Ok(false) when what's stored is as late or later (nothing to check then).
    ///
    /// Give the stamp of a record that wasn't refused to `Clock::observe` afterwards.
    pub fn apply_verified(&mut self, key: &AccountKey, account: &str, incoming: Record, now_ms: u64) -> Result<bool, ApplyError> {
        if let Some(ahead_ms) = incoming.hlc.too_far_ahead(now_ms) {
            return Err(FutureStamp { ahead_ms }.into());
        }
        if self.records.get(&incoming.id).is_some_and(|stored| stored.hlc >= incoming.hlc) {
            return Ok(false);
        }
        key.open_record(account, &incoming)?;
        Ok(self.apply_unverified(incoming))
    }

    /// Applies a batch from the server with `apply_verified`: the ids that changed here, and the
    /// records refused (the rest of the batch still applies).
    pub fn apply_all_verified(&mut self, key: &AccountKey, account: &str, incoming: impl IntoIterator<Item = Record>, now_ms: u64) -> (Vec<RecordId>, Vec<(RecordId, ApplyError)>) {
        let (mut changed, mut refused) = (vec![], vec![]);
        for r in incoming {
            let id = r.id.clone();
            match self.apply_verified(key, account, r, now_ms) {
                Ok(true) => changed.push(id),
                Ok(false) => {}
                Err(e) => refused.push((id, e)),
            }
        }
        (changed, refused)
    }

    /// Takes `incoming` if it's later than what's stored, without opening it. True when it was
    /// taken. Only for the server, which holds no key; a device uses `apply_verified`.
    pub fn apply_unverified(&mut self, incoming: Record) -> bool {
        match self.records.get(&incoming.id) {
            Some(stored) if stored.hlc >= incoming.hlc => false,
            _ => {
                self.records.insert(incoming.id.clone(), incoming);
                true
            }
        }
    }

    /// Applies a batch with `apply_unverified` (the server's side), returning the ids that changed.
    pub fn apply_all_unverified(&mut self, incoming: impl IntoIterator<Item = Record>) -> Vec<RecordId> {
        incoming.into_iter().filter_map(|r| {
            let id = r.id.clone();
            self.apply_unverified(r).then_some(id)
        }).collect()
    }

    /// A change made on this device by a dino that knows value shapes up to `known_schema`.
    pub fn write_local(&mut self, record: Record, known_schema: u32) -> Result<(), WriteError> {
        if let Some(stored) = self.records.get(&record.id) {
            if stored.schema > known_schema {
                return Err(WriteError::NewerSchema { stored: stored.schema, ours: known_schema });
            }
            if stored.hlc >= record.hlc {
                return Err(WriteError::Stale);
            }
        }
        self.records.insert(record.id.clone(), record);
        Ok(())
    }

    /// Whether this client can read the value it holds for `id`, or should keep it as it is.
    pub fn readable(&self, id: &RecordId, known_schema: u32) -> bool {
        self.records.get(id).is_some_and(|r| r.schema <= known_schema)
    }
}
