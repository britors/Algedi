//! Thin wrapper over the Drive API v3 REST endpoints actually needed by the
//! sync engine (metadata, upload, download, delete).

use serde::Deserialize;

const API_BASE: &str = "https://www.googleapis.com/drive/v3";
#[allow(dead_code)]
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub md5_checksum: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub modified_time: Option<String>,
    #[serde(default)]
    pub parents: Vec<String>,
}

pub struct GDriveApi {
    http: reqwest::Client,
    // RwLock, not a plain field: `CloudProvider::set_access_token` takes
    // `&self` (the provider is shared via `Arc` across concurrent calls),
    // so refreshing the token needs interior mutability.
    access_token: std::sync::RwLock<String>,
}

impl GDriveApi {
    pub fn new(access_token: String) -> Self {
        Self { http: reqwest::Client::new(), access_token: std::sync::RwLock::new(access_token) }
    }

    pub fn set_access_token(&self, token: String) {
        *self.access_token.write().unwrap() = token;
    }

    fn access_token(&self) -> String {
        self.access_token.read().unwrap().clone()
    }

    pub async fn get_file(&self, file_id: &str) -> algedi_provider_trait::ProviderResult<DriveFile> {
        let url = format!(
            "{API_BASE}/files/{file_id}?fields=id,name,mimeType,md5Checksum,size,modifiedTime,parents"
        );
        self.get_json(&url).await
    }

    pub async fn create_folder(
        &self,
        name: &str,
        parent_id: &str,
    ) -> algedi_provider_trait::ProviderResult<DriveFile> {
        // TODO: POST {API_BASE}/files with mimeType=application/vnd.google-apps.folder
        let _ = (name, parent_id);
        Err(algedi_provider_trait::ProviderError::Other("not implemented".into()))
    }

    pub async fn upload_file(
        &self,
        _local_path: &std::path::Path,
        _parent_id: &str,
    ) -> algedi_provider_trait::ProviderResult<DriveFile> {
        // TODO: resumable upload session against UPLOAD_BASE for large
        // files, simple multipart upload otherwise.
        Err(algedi_provider_trait::ProviderError::Other("not implemented".into()))
    }

    pub async fn download_file(
        &self,
        _file_id: &str,
        _dest_path: &std::path::Path,
    ) -> algedi_provider_trait::ProviderResult<()> {
        // TODO: GET /files/{id}?alt=media, streamed to disk.
        Err(algedi_provider_trait::ProviderError::Other("not implemented".into()))
    }

    pub async fn delete_file(&self, file_id: &str) -> algedi_provider_trait::ProviderResult<()> {
        let url = format!("{API_BASE}/files/{file_id}");
        let resp = self
            .http
            .delete(&url)
            .bearer_auth(self.access_token())
            .send()
            .await
            .map_err(|e| algedi_provider_trait::ProviderError::Network(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(algedi_provider_trait::ProviderError::Other(resp.status().to_string()))
        }
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
    ) -> algedi_provider_trait::ProviderResult<T> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(self.access_token())
            .send()
            .await
            .map_err(|e| algedi_provider_trait::ProviderError::Network(e.to_string()))?;

        match resp.status().as_u16() {
            200 => resp
                .json()
                .await
                .map_err(|e| algedi_provider_trait::ProviderError::Other(e.to_string())),
            401 => Err(algedi_provider_trait::ProviderError::AuthRequired),
            404 => Err(algedi_provider_trait::ProviderError::NotFound(url.to_string())),
            429 => Err(algedi_provider_trait::ProviderError::RateLimited { retry_after_secs: None }),
            other => Err(algedi_provider_trait::ProviderError::Other(format!("HTTP {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_access_token_replaces_it_for_subsequent_calls() {
        let api = GDriveApi::new("old-token".into());
        assert_eq!(api.access_token(), "old-token");
        api.set_access_token("new-token".into());
        assert_eq!(api.access_token(), "new-token");
    }
}
