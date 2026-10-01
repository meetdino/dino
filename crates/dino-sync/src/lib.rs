//! Settings sync for dino, shared by dinod and the dino-cloud server.
//!
//! - `record`: what travels, and the REST + JSON requests that move it.
//! - `hlc`: when a change was made, comparable across devices.
//! - `merge`: each setting on its own, the latest stamp wins.
//! - `crypto`, `approval`, `recovery`: values are encrypted on the device; the server can't read them.
//! - `settings`: dino's `settings.toml` as records and back.
//!
//! No network code: dinod and the server bring their own.

pub mod approval;
pub mod crypto;
pub mod hlc;
pub mod merge;
pub mod record;
pub mod recovery;
pub mod settings;

pub use approval::{Commitment, DeviceKeys, Grant, Response, Reveal, approval_code, verify_reveal};
pub use crypto::{AccountKey, CryptoError};
pub use hlc::{Clock, FutureStamp, Hlc};
pub use merge::{ApplyError, Store};
pub use record::{Nudge, PullResponse, PushRequest, PushResponse, Record, RecordId, SyncError};
pub use recovery::RecoveryKey;
