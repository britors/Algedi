//! `CloudProvider` impl for OneDrive. Change detection uses the `/delta`
//! endpoint with a persisted `deltaLink` (PROMPT-ALGEDI.md sec. 5.1).

use crate::OneDriveProvider;
use algedi_provider_trait::{
    ChangeCursor, ChangeKind, CloudProvider, ProviderError, ProviderResult, RemoteChange,
    RemoteFile,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct DeltaPage {
    #[serde(default)]
    value: Vec<crate::api::DriveItem>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

#[async_trait]
impl CloudProvider for OneDriveProvider {
    fn provider_id(&self) -> &'static str {
        "onedrive"
    }

    async fn list_changes(
        &self,
        cursor: Option<&ChangeCursor>,
    ) -> ProviderResult<(Vec<RemoteChange>, ChangeCursor)> {
        let initial = cursor.is_none();
        let mut url = cursor
            .cloned()
            .unwrap_or_else(|| format!("{}/me/drive/root/delta", crate::api::GRAPH_BASE));
        let mut changes = Vec::new();
        loop {
            let page: DeltaPage = self.api.get_json(&url).await?;
            changes.extend(page.value.into_iter().map(|item| {
                let deleted = item.deleted.is_some();
                RemoteChange {
                    file: to_remote_file(item),
                    kind: if deleted {
                        ChangeKind::Deleted
                    } else if initial {
                        ChangeKind::Created
                    } else {
                        ChangeKind::Modified
                    },
                }
            }));
            if let Some(next) = page.next_link {
                url = next;
                continue;
            }
            let cursor = page.delta_link.ok_or_else(|| {
                ProviderError::Other(
                    "Graph delta response had neither @odata.nextLink nor @odata.deltaLink".into(),
                )
            })?;
            return Ok((changes, cursor));
        }
    }

    async fn upload(
        &self,
        local_path: &Path,
        remote_parent_id: &str,
    ) -> ProviderResult<RemoteFile> {
        let item = self.api.upload_item(local_path, remote_parent_id).await?;
        Ok(to_remote_file(item))
    }

    async fn create_folder(
        &self,
        name: &str,
        remote_parent_id: &str,
    ) -> ProviderResult<RemoteFile> {
        let folder = self.api.create_folder(name, remote_parent_id).await?;
        Ok(to_remote_file(folder))
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
        content_hash: item
            .file
            .and_then(|f| f.hashes)
            .and_then(|h| h.quick_xor_hash),
        modified_at: item
            .last_modified
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_graph_links_and_deleted_facet() {
        let page: DeltaPage = serde_json::from_str(
            r#"{
            "value":[{"id":"gone-id","name":"old.txt","deleted":{}}],
            "@odata.deltaLink":"https://graph.microsoft.com/delta?token=next"
        }"#,
        )
        .unwrap();
        assert_eq!(page.value.len(), 1);
        assert!(page.value[0].deleted.is_some());
        assert!(page.next_link.is_none());
        assert!(page.delta_link.unwrap().contains("token=next"));
    }
}
