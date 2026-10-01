//! Settings sync for dino, shared by dinod and the dino-cloud server.
//!
//! - `record`: what travels, and the REST + JSON requests that move it.
//! - `hlc`: when a change was made, comparable across devices.
//! - `merge`: each setting on its own, the latest stamp wins.
//! - `settings`: dino's `settings.toml` as records and back.
//!
//! Values travel as plain JSON over TLS: settings aren't secrets, and secrets (API keys, tokens)
//! never sync. No network code: dinod and the server bring their own.

pub mod hlc;
pub mod merge;
pub mod record;
#[cfg(feature = "settings")]
pub mod settings;

pub use hlc::{Clock, FutureStamp, Hlc};
pub use merge::Store;
pub use record::{Nudge, PullResponse, PushRequest, PushResponse, Record, RecordId, SyncError};
