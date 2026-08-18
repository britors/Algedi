//! Common interface implemented by each cloud adapter (algedi-provider-gdrive,
//! algedi-provider-onedrive). `algedi-core` depends only on this trait, never
//! on a concrete provider, so the sync engine can be tested against a fake
//! provider and new backends can be added without touching the core.
//! See PROMPT-ALGEDI.md sec. 2.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteFile {
    pub remote_id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub is_folder: bool,
    pub size: u64,
    pub content_hash: Option<String>,
    pub modified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
    Renamed { old_remote_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteChange {
    pub file: RemoteFile,
    pub kind: ChangeKind,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("authentication required or token expired")]
    AuthRequired,
    #[error("remote item not found: {0}")]
    NotFound(String),
    #[error("rate limited, retry after {retry_after_secs:?}s")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("network error: {0}")]
    Network(String),
    #[error("provider error: {0}")]
    Other(String),
}

pub type ProviderResult<T> = Result<T, ProviderError>;

/// Opaque cursor persisted by the caller (state_db) between polling cycles.
/// Google Drive: page token from `changes.list`.
/// OneDrive: delta link from `/delta`.
pub type ChangeCursor = String;

#[async_trait]
pub trait CloudProvider: Send + Sync {
    /// Short identifier, e.g. "gdrive" or "onedrive".
    fn provider_id(&self) -> &'static str;

    /// Lists remote changes since `cursor`. `None` means "from the start" and
    /// is used only for the initial full sync of a folder pair.
    async fn list_changes(
        &self,
        cursor: Option<&ChangeCursor>,
    ) -> ProviderResult<(Vec<RemoteChange>, ChangeCursor)>;

    async fn upload(&self, local_path: &Path, remote_parent_id: &str) -> ProviderResult<RemoteFile>;

    async fn download(&self, remote_id: &str, dest_path: &Path) -> ProviderResult<()>;

    async fn delete(&self, remote_id: &str) -> ProviderResult<()>;

    async fn get_metadata(&self, remote_id: &str) -> ProviderResult<RemoteFile>;

    /// Web URL for a file, used by the Nautilus "Ver no <provider>" action.
    fn web_url(&self, remote_id: &str) -> String;

    /// Swaps in a freshly refreshed access token, called by the daemon's
    /// token-refresh cycle before the current one expires. Implementations
    /// must apply this atomically (interior mutability) since the provider
    /// is shared via `Arc` across concurrent in-flight calls.
    fn set_access_token(&self, access_token: String);
}
