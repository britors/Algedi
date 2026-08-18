//! Sync engine: diff/reconciliation, conflict handling, and local state
//! tracking. No network I/O lives here — that's `algedi-provider-*` — so
//! this crate can be exercised with a fake `CloudProvider` in tests.
//! See PROMPT-ALGEDI.md sec. 2 and 5.

pub mod conflict;
pub mod hash;
pub mod state_db;
pub mod sync_engine;
pub mod watcher;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type AccountId = Uuid;
pub type PairId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncStatus {
    Synced,
    Syncing,
    Conflict,
    Paused,
    Unknown,
}

impl SyncStatus {
    /// Matches the `status` strings used on the org.lyraos.Algedi1 D-Bus
    /// interface (PROMPT-ALGEDI.md sec. 8).
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncStatus::Synced => "synced",
            SyncStatus::Syncing => "syncing",
            SyncStatus::Conflict => "conflict",
            SyncStatus::Paused => "paused",
            SyncStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderPair {
    pub id: PairId,
    pub account_id: AccountId,
    pub local_path: std::path::PathBuf,
    pub remote_path: String,
    pub remote_folder_id: String,
    pub paused: bool,
}

pub use conflict::conflicting_file_name;
pub use hash::hash_file;
pub use state_db::StateDb;
pub use sync_engine::{SyncAction, SyncEngine};
pub use watcher::FolderWatcher;
