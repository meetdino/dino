//! Merging: each setting on its own, the latest stamp wins. Applying the same records in any order,
//! any number of times, leaves every device with the same state.
//!
//! The stored record is each setting's high-water mark: records are never removed (a delete is a
//! tombstone), and a record only replaces one with an earlier stamp, so a record seen again loses to
//! what's already here.

use std::collections::BTreeMap;

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

    /// Takes `incoming` if it's later than what's stored. True when it was taken.
    pub fn apply(&mut self, incoming: Record) -> bool {
        match self.records.get(&incoming.id) {
            Some(stored) if stored.hlc >= incoming.hlc => false,
            _ => {
                self.records.insert(incoming.id.clone(), incoming);
                true
            }
        }
    }

    /// Applies a batch, returning the ids that changed.
    pub fn apply_all(&mut self, incoming: impl IntoIterator<Item = Record>) -> Vec<RecordId> {
        incoming
            .into_iter()
            .filter_map(|r| {
                let id = r.id.clone();
                self.apply(r).then_some(id)
            })
            .collect()
    }

    /// `apply`, for a record from elsewhere: refused when it's stamped further ahead of `now_ms`
    /// than `crate::hlc::MAX_SKEW_MS` (a clock days ahead would otherwise win every conflict).
    pub fn apply_remote(&mut self, incoming: Record, now_ms: u64) -> Result<bool, FutureStamp> {
        if let Some(ahead_ms) = incoming.hlc.too_far_ahead(now_ms) {
            return Err(FutureStamp { ahead_ms });
        }
        Ok(self.apply(incoming))
    }

    /// `apply_remote` for a batch: the ids that changed here, and the records refused (the rest
    /// still apply).
    pub fn apply_all_remote(&mut self, incoming: impl IntoIterator<Item = Record>, now_ms: u64) -> (Vec<RecordId>, Vec<(RecordId, FutureStamp)>) {
        let (mut changed, mut refused) = (vec![], vec![]);
        for r in incoming {
            let id = r.id.clone();
            match self.apply_remote(r, now_ms) {
                Ok(true) => changed.push(id),
                Ok(false) => {}
                Err(e) => refused.push((id, e)),
            }
        }
        (changed, refused)
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
