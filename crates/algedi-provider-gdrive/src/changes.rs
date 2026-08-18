//! `CloudProvider` impl for Google Drive. Change detection uses the
//! `changes.list` endpoint with a persisted `pageToken` (PROMPT-ALGEDI.md
//! sec. 5.1).

use crate::GDriveProvider;
use algedi_provider_trait::{ChangeCursor, CloudProvider, ProviderResult, RemoteChange, RemoteFile};
use async_trait::async_trait;
use std::path::Path;

#[async_trait]
impl CloudProvider for GDriveProvider {
    fn provider_id(&self) -> &'static str {
        "gdrive"
    }

    async fn list_changes(
        &self,
        _cursor: Option<&ChangeCursor>,
    ) -> ProviderResult<(Vec<RemoteChange>, ChangeCursor)> {
        // TODO: GET /changes?pageToken=... (or /changes/startPageToken when
        // cursor is None), paging until nextPageToken is absent, mapping
        // each entry to a RemoteChange. The final token is persisted by the
        // caller via StateDb::set_change_cursor.
        Ok((Vec::new(), String::new()))
    }

    async fn upload(&self, local_path: &Path, remote_parent_id: &str) -> ProviderResult<RemoteFile> {
        let file = self.api.upload_file(local_path, remote_parent_id).await?;
        Ok(to_remote_file(file))
    }

    async fn download(&self, remote_id: &str, dest_path: &Path) -> ProviderResult<()> {
        self.api.download_file(remote_id, dest_path).await
    }

    async fn delete(&self, remote_id: &str) -> ProviderResult<()> {
        self.api.delete_file(remote_id).await
    }

    async fn get_metadata(&self, remote_id: &str) -> ProviderResult<RemoteFile> {
        let file = self.api.get_file(remote_id).await?;
        Ok(to_remote_file(file))
    }

    fn web_url(&self, remote_id: &str) -> String {
        format!("https://drive.google.com/file/d/{remote_id}/view")
    }

    fn set_access_token(&self, access_token: String) {
        self.api.set_access_token(access_token);
    }
}

fn to_remote_file(f: crate::api::DriveFile) -> RemoteFile {
    RemoteFile {
        remote_id: f.id,
        name: f.name,
        parent_id: f.parents.into_iter().next(),
        is_folder: f.mime_type == "application/vnd.google-apps.folder",
        size: f.size.and_then(|s| s.parse().ok()).unwrap_or(0),
        content_hash: f.md5_checksum,
        modified_at: f
            .modified_time
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now),
    }
}
