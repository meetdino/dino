//! What travels: one record per setting, and the requests that move batches of them.
//!
//! The wire is REST + JSON. Every type keeps fields it doesn't know in `extra` and writes them
//! back unchanged, so an older client never drops what a newer one added.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::hlc::Hlc;

/// Protocol version this crate speaks, sent as `DINO-Sync-Version`.
pub const PROTOCOL: u32 = 1;

/// Largest encrypted value a record may carry, in bytes of its token.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;
/// Most records one push may carry.
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
/// server sees only where it lives, when and by whom it was written, and how big it is.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Record {
    #[serde(flatten)]
    pub id: RecordId,
    pub hlc: Hlc,
    /// The shape of the value inside; a client keeps a newer one as it is and never overwrites it.
    pub schema: u32,
    /// None: deleted (a tombstone, kept so the delete reaches every device).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Set by the server when it accepts the record; absent on the way up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Record {
    pub fn is_tombstone(&self) -> bool {
        self.value.is_none()
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
    if push.records.len() > MAX_BATCH {
        return Err(SyncError::TooManyRecords { count: push.records.len(), max: MAX_BATCH });
    }
    for r in &push.records {
        let bytes = r.value.as_ref().map_or(0, String::len);
        if bytes > MAX_RECORD_BYTES {
            return Err(SyncError::TooLarge { bytes, max: MAX_RECORD_BYTES });
        }
    }
    Ok(())
}
