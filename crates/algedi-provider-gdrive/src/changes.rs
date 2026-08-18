//! `CloudProvider` impl for Google Drive. Change detection uses the
//! `changes.list` endpoint with a persisted `pageToken` (PROMPT-ALGEDI.md
//! sec. 5.1).

use crate::GDriveProvider;
use algedi_provider_trait::{
    ChangeCursor, ChangeKind, CloudProvider, ProviderError, ProviderResult, RemoteChange,
    RemoteFile,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartPageToken {
    start_page_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangePage {
    #[serde(default)]
    changes: Vec<DriveChange>,
    next_page_token: Option<String>,
    new_start_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveChange {
    #[serde(default)]
    removed: bool,
    file_id: String,
    file: Option<crate::api::DriveFile>,
    #[serde(default)]
    change_type: String,
}

#[async_trait]
impl CloudProvider for GDriveProvider {
    fn provider_id(&self) -> &'static str {
        "gdrive"
    }

    async fn list_changes(
        &self,
        cursor: Option<&ChangeCursor>,
    ) -> ProviderResult<(Vec<RemoteChange>, ChangeCursor)> {
        let mut page_token = match cursor {
            Some(token) => token.clone(),
            None => {
                self.api
                    .get_json::<StartPageToken>(&format!(
                        "{}/changes/startPageToken",
                        crate::api::API_BASE
                    ))
                    .await?
                    .start_page_token
            }
        };
        let mut changes = Vec::new();
        loop {
            let mut url = reqwest::Url::parse(&format!("{}/changes", crate::api::API_BASE))
                .map_err(|e| ProviderError::Other(e.to_string()))?;
            url.query_pairs_mut()
                .append_pair("pageToken", &page_token)
                .append_pair("spaces", "drive")
                .append_pair("includeRemoved", "true")
                .append_pair("pageSize", "1000")
                .append_pair("fields", "changes(fileId,removed,changeType,file(id,name,mimeType,md5Checksum,size,modifiedTime,parents)),nextPageToken,newStartPageToken");
            let page: ChangePage = self.api.get_json(url.as_str()).await?;
            changes.extend(page.changes.into_iter().filter_map(to_remote_change));
            if let Some(next) = page.next_page_token {
                page_token = next;
            } else {
                let cursor = page.new_start_page_token.ok_or_else(|| {
                    ProviderError::Other(
                        "Drive changes response had neither nextPageToken nor newStartPageToken"
                            .into(),
                    )
                })?;
                return Ok((changes, cursor));
            }
        }
    }

    async fn upload(
        &self,
        local_path: &Path,
        remote_parent_id: &str,
    ) -> ProviderResult<RemoteFile> {
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

fn to_remote_change(change: DriveChange) -> Option<RemoteChange> {
    if !change.change_type.is_empty() && change.change_type != "file" {
        return None;
    }
    if change.removed {
        return Some(RemoteChange {
            file: RemoteFile {
                remote_id: change.file_id,
                name: String::new(),
                parent_id: None,
                is_folder: false,
                size: 0,
                content_hash: None,
                modified_at: chrono::Utc::now(),
            },
            kind: ChangeKind::Deleted,
        });
    }
    change.file.map(|file| RemoteChange {
        file: to_remote_file(file),
        kind: ChangeKind::Modified,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_removed_change_without_fabricating_a_path() {
        let change: DriveChange =
            serde_json::from_str(r#"{"removed":true,"fileId":"gone-id","changeType":"file"}"#)
                .unwrap();
        let mapped = to_remote_change(change).unwrap();
        assert_eq!(mapped.kind, ChangeKind::Deleted);
        assert_eq!(mapped.file.remote_id, "gone-id");
        assert!(mapped.file.name.is_empty());
    }

    #[test]
    fn ignores_shared_drive_metadata_changes() {
        let change: DriveChange =
            serde_json::from_str(r#"{"removed":false,"fileId":"drive-id","changeType":"drive"}"#)
                .unwrap();
        assert!(to_remote_change(change).is_none());
    }
}
