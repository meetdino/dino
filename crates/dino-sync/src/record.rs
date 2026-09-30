//! What travels: one record per setting, and the requests that move batches of them.
//!
//! The wire is REST + JSON. Every type keeps fields it doesn't know in `extra` and writes them
//! back unchanged, so an older client never drops what a newer one added.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::hlc::Hlc;

/// Protocol version this crate speaks, sent as `DINO-Sync-Version`. 2: every record carries a
/// token (deletes too), sealed to its stamp and whether it's a delete.
pub const PROTOCOL: u32 = 2;

/// Largest encrypted value a record may carry, in bytes of its token.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;
/// Most records one push, or one page of a pull, may carry.
pub const MAX_BATCH: usize = 500;

/// Where a record lives: `("agents", "claude.mode")`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordId {
    pub collection: String,
    pub key: String,
}

impl RecordId {
    pub fn new(collection: impl Into<String>, key: impl Into<String>) -> Self {
        Self { collection: collection.into(), key: key.into() }
    }
}

/// One setting as the server stores it. `value` is a PASETO v4.local token (see `crypto`), so the
/// server sees only where it lives, when and by whom it was written, whether it's a delete, and
/// how big it is. The token is sealed to all of that but `seq` and `extra`: a server without the
/// account key can't restamp, move or delete a value (`AccountKey::open_record`,
/// `Store::apply_verified`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Record {
    #[serde(flatten)]
    pub id: RecordId,
    pub hlc: Hlc,
    /// The shape of the value inside; a client keeps a newer one as it is and never overwrites it.
    pub schema: u32,
    /// The sealed value, or for a delete a sealed tombstone.
    pub value: String,
    /// Deleted: `value` is a tombstone, kept so the delete reaches every device.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    /// Set by the server when it accepts the record; absent on the way up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Record {
    pub fn is_tombstone(&self) -> bool {
        self.deleted
    }
}

/// `POST /v1/sync`: records written on this device since its last push.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PushRequest {
    pub device_id: String,
    pub records: Vec<Record>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What the server did with a push. A record that lost to a newer one already stored isn't an
/// error: the device picks the winner up on its next pull.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PushResponse {
    /// The account's sequence after the push.
    pub seq: u64,
    pub accepted: Vec<RecordId>,
    #[serde(default)]
    pub superseded: Vec<RecordId>,
    #[serde(default)]
    pub rejected: Vec<Rejection>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Rejection {
    #[serde(flatten)]
    pub id: RecordId,
    pub error: SyncError,
}

/// `GET /v1/sync?since=<seq>`: everything accepted after `since`, oldest first, a page at a time.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PullResponse {
    /// The sequence of the last record in `records`, or `since` when there's nothing new.
    pub seq: u64,
    pub records: Vec<Record>,
    /// More pages follow: pull again from `seq`.
    #[serde(default)]
    pub more: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Sent down the WebSocket when the account's sequence moves. It carries no data: the device
/// pulls from its last sequence.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Nudge {
    Advanced { seq: u64 },
    /// The account's records were wiped ("Reset sync"): start over from 0.
    Reset,
    /// Anything a newer server sends.
    #[serde(other)]
    Unknown,
}

/// Why a record or request was refused.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SyncError {
    /// Its clock is further ahead of the server's than `crate::hlc::MAX_SKEW_MS`.
    FutureStamp { ahead_ms: u64 },
    TooLarge { bytes: usize, max: usize },
    TooManyRecords { count: usize, max: usize },
    /// The device was signed out or revoked.
    UnknownDevice,
    RateLimited { retry_after_s: u64 },
    /// The client speaks an older protocol than the server still serves.
    UpgradeRequired { min: u32 },
    Malformed { reason: String },
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::FutureStamp { ahead_ms } => write!(f, "this device's clock is {ahead_ms} ms ahead of the server's"),
            SyncError::TooLarge { bytes, max } => write!(f, "a setting of {bytes} bytes is over the {max} byte limit"),
            SyncError::TooManyRecords { count, max } => write!(f, "{count} settings in one push; the limit is {max}"),
            SyncError::UnknownDevice => write!(f, "this device was signed out"),
            SyncError::RateLimited { retry_after_s } => write!(f, "too many changes; try again in {retry_after_s} s"),
            SyncError::UpgradeRequired { min } => write!(f, "update dino: sync needs protocol {min}"),
            SyncError::Malformed { reason } => write!(f, "malformed: {reason}"),
            SyncError::Unknown => write!(f, "refused by the server"),
        }
    }
}

impl std::error::Error for SyncError {}

/// Checks a push before sending it (the server makes the same checks).
pub fn check_push(push: &PushRequest) -> Result<(), SyncError> {
    check_records(&push.records)
}

/// Checks a page of a pull before taking any of it: the server never sends more than a device
/// may push, so a page that's larger came from a server that's broken or hostile.
pub fn check_pull(pull: &PullResponse) -> Result<(), SyncError> {
    check_records(&pull.records)
}

fn check_records(records: &[Record]) -> Result<(), SyncError> {
    if records.len() > MAX_BATCH {
        return Err(SyncError::TooManyRecords { count: records.len(), max: MAX_BATCH });
    }
    for r in records {
        let bytes = r.value.len();
        if bytes > MAX_RECORD_BYTES {
            return Err(SyncError::TooLarge { bytes, max: MAX_RECORD_BYTES });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(value: String) -> Record {
        Record { id: RecordId::new("agents", "claude.mode"), hlc: Hlc { wall_ms: 1, counter: 0, device: "d".into() }, schema: 1, value, deleted: false, seq: Some(1), extra: Map::new() }
    }

    #[test]
    fn pulls_are_held_to_the_push_limits() {
        let ok = PullResponse { seq: 1, records: vec![record("v4.local.x".into()); MAX_BATCH], ..Default::default() };
        assert_eq!(check_pull(&ok), Ok(()));
        let many = PullResponse { records: vec![record("v4.local.x".into()); MAX_BATCH + 1], ..ok.clone() };
        assert_eq!(check_pull(&many), Err(SyncError::TooManyRecords { count: MAX_BATCH + 1, max: MAX_BATCH }));
        let big = PullResponse { records: vec![record("x".repeat(MAX_RECORD_BYTES + 1))], ..ok };
        assert_eq!(check_pull(&big), Err(SyncError::TooLarge { bytes: MAX_RECORD_BYTES + 1, max: MAX_RECORD_BYTES }));
    }
}
