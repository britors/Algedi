//! `CloudProvider` impl for OneDrive. Change detection uses the `/delta`
//! endpoint with a persisted `deltaLink` (PROMPT-ALGEDI.md sec. 5.1).

use crate::OneDriveProvider;
use algedi_provider_trait::{ChangeCursor, CloudProvider, ProviderResult, RemoteChange, RemoteFile};
use async_trait::async_trait;
use std::path::Path;

#[async_trait]
impl CloudProvider for OneDriveProvider {
    fn provider_id(&self) -> &'static str {
        "onedrive"
    }

    async fn list_changes(
        &self,
        _cursor: Option<&ChangeCursor>,
    ) -> ProviderResult<(Vec<RemoteChange>, ChangeCursor)> {
        // TODO: GET /me/drive/root/delta (or the stored deltaLink from
        // cursor), paging via @odata.nextLink until @odata.deltaLink is
        // returned; that deltaLink becomes the new cursor.
        Ok((Vec::new(), String::new()))
    }

    async fn upload(&self, local_path: &Path, remote_parent_id: &str) -> ProviderResult<RemoteFile> {
        let item = self.api.upload_item(local_path, remote_parent_id).await?;
        Ok(to_remote_file(item))
    }

    async fn download(&self, remote_id: &str, dest_path: &Path) -> ProviderResult<()> {
        self.api.download_item(remote_id, dest_path).await
    }

    async fn delete(&self, remote_id: &str) -> ProviderResult<()> {
        self.api.delete_item(remote_id).await
    }

    async fn get_metadata(&self, remote_id: &str) -> ProviderResult<RemoteFile> {
        let item = self.api.get_item(remote_id).await?;
        Ok(to_remote_file(item))
    }

    fn web_url(&self, remote_id: &str) -> String {
        format!("https://onedrive.live.com/?id={remote_id}")
    }

    fn set_access_token(&self, access_token: String) {
        self.api.set_access_token(access_token);
    }
}

fn to_remote_file(item: crate::api::DriveItem) -> RemoteFile {
    RemoteFile {
        remote_id: item.id,
        name: item.name,
        parent_id: item.parent_reference.map(|p| p.id),
        is_folder: item.folder.is_some(),
        size: item.size.unwrap_or(0),
        content_hash: item.file.and_then(|f| f.hashes).and_then(|h| h.quick_xor_hash),
        modified_at: item
            .last_modified
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now),
    }
}
