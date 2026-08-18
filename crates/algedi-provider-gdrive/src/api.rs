//! Thin wrapper over the Drive API v3 REST endpoints actually needed by the
//! sync engine (metadata, upload, download, delete).

use algedi_provider_trait::{ProviderError, ProviderResult};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

pub(crate) const API_BASE: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";
const FIELDS: &str = "id,name,mimeType,md5Checksum,size,modifiedTime,parents";

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
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
        Self {
            http: reqwest::Client::new(),
            access_token: std::sync::RwLock::new(access_token),
        }
    }

    pub fn set_access_token(&self, token: String) {
        *self.access_token.write().unwrap() = token;
    }

    fn access_token(&self) -> String {
        self.access_token.read().unwrap().clone()
    }

    pub async fn get_file(
        &self,
        file_id: &str,
    ) -> algedi_provider_trait::ProviderResult<DriveFile> {
        let url = format!(
            "{API_BASE}/files/{file_id}?fields=id,name,mimeType,md5Checksum,size,modifiedTime,parents"
        );
        self.get_json(&url).await
    }

    pub async fn resolve_folder_path(&self, remote_path: &str) -> ProviderResult<DriveFile> {
        let components = remote_path_components(remote_path)?;
        let mut folder = self.get_file("root").await?;
        for name in components {
            let escaped = name.replace('\\', "\\\\").replace('\'', "\\'");
            let query = format!(
                "name = '{escaped}' and '{}' in parents and mimeType = 'application/vnd.google-apps.folder' and trashed = false",
                folder.id
            );
            let mut matches = Vec::new();
            let mut page_token: Option<String> = None;
            loop {
                let mut url = reqwest::Url::parse(&format!("{API_BASE}/files"))
                    .map_err(|error| ProviderError::Other(error.to_string()))?;
                url.query_pairs_mut()
                    .append_pair("q", &query)
                    .append_pair("spaces", "drive")
                    .append_pair("fields", &format!("nextPageToken,files({FIELDS})"));
                if let Some(token) = &page_token {
                    url.query_pairs_mut().append_pair("pageToken", token);
                }
                let page: DriveFileList = self.get_json(url.as_str()).await?;
                matches.extend(page.files);
                match page.next_page_token {
                    Some(token) => page_token = Some(token),
                    None => break,
                }
            }
            folder = match matches.len() {
                0 => return Err(ProviderError::NotFound(remote_path.into())),
                1 => matches.pop().unwrap(),
                count => {
                    return Err(ProviderError::Other(format!(
                        "remote path is ambiguous: {remote_path:?} matched {count} folders"
                    )))
                }
            };
        }
        Ok(folder)
    }

    pub async fn create_folder(&self, name: &str, parent_id: &str) -> ProviderResult<DriveFile> {
        let response = self
            .http
            .post(format!("{API_BASE}/files"))
            .bearer_auth(self.access_token())
            .query(&[("fields", FIELDS)])
            .json(&serde_json::json!({
                "name": name,
                "mimeType": "application/vnd.google-apps.folder",
                "parents": [parent_id]
            }))
            .send()
            .await
            .map_err(network_error)?;
        decode_json(response, name).await
    }

    pub async fn upload_file(
        &self,
        local_path: &Path,
        parent_id: &str,
    ) -> ProviderResult<DriveFile> {
        let name = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                ProviderError::Other("local path has no valid UTF-8 file name".into())
            })?;
        let size = tokio::fs::metadata(local_path)
            .await
            .map_err(io_error)?
            .len();
        let response = self
            .http
            .post(format!("{UPLOAD_BASE}/files"))
            .bearer_auth(self.access_token())
            .query(&[("uploadType", "resumable"), ("fields", FIELDS)])
            .header("X-Upload-Content-Type", "application/octet-stream")
            .header("X-Upload-Content-Length", size)
            .json(&serde_json::json!({"name": name, "parents": [parent_id]}))
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            return Err(response_error(response, name).await);
        }
        let session = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| {
                ProviderError::Other("Drive resumable upload returned no Location header".into())
            })?;
        let file = tokio::fs::File::open(local_path).await.map_err(io_error)?;
        let response = self
            .http
            .put(session)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
            .send()
            .await
            .map_err(network_error)?;
        decode_json(response, name).await
    }

    pub async fn update_file(&self, local_path: &Path, file_id: &str) -> ProviderResult<DriveFile> {
        let name = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                ProviderError::Other("local path has no valid UTF-8 file name".into())
            })?;
        let size = tokio::fs::metadata(local_path)
            .await
            .map_err(io_error)?
            .len();
        let response = self
            .http
            .patch(format!("{UPLOAD_BASE}/files/{file_id}"))
            .bearer_auth(self.access_token())
            .query(&[("uploadType", "resumable"), ("fields", FIELDS)])
            .header("X-Upload-Content-Type", "application/octet-stream")
            .header("X-Upload-Content-Length", size)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            return Err(response_error(response, file_id).await);
        }
        let session = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| {
                ProviderError::Other("Drive resumable update returned no Location header".into())
            })?;
        let file = tokio::fs::File::open(local_path).await.map_err(io_error)?;
        let response = self
            .http
            .put(session)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
            .send()
            .await
            .map_err(network_error)?;
        decode_json(response, name).await
    }

    pub async fn download_file(&self, file_id: &str, dest_path: &Path) -> ProviderResult<()> {
        let mut response = self
            .http
            .get(format!("{API_BASE}/files/{file_id}"))
            .bearer_auth(self.access_token())
            .query(&[("alt", "media")])
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            return Err(response_error(response, file_id).await);
        }
        let temporary = temporary_path(dest_path);
        let mut output = tokio::fs::File::create(&temporary)
            .await
            .map_err(io_error)?;
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            output.write_all(&chunk).await.map_err(io_error)?;
        }
        output.flush().await.map_err(io_error)?;
        drop(output);
        tokio::fs::rename(&temporary, dest_path)
            .await
            .map_err(io_error)
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
            Err(algedi_provider_trait::ProviderError::Other(
                resp.status().to_string(),
            ))
        }
    }

    pub(crate) async fn get_json<T: for<'de> Deserialize<'de>>(
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
            404 => Err(algedi_provider_trait::ProviderError::NotFound(
                url.to_string(),
            )),
            429 => Err(algedi_provider_trait::ProviderError::RateLimited {
                retry_after_secs: None,
            }),
            other => Err(algedi_provider_trait::ProviderError::Other(format!(
                "HTTP {other}"
            ))),
        }
    }
}

fn temporary_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".algedi-part");
    PathBuf::from(name)
}

fn remote_path_components(remote_path: &str) -> ProviderResult<Vec<&str>> {
    let components: Vec<_> = remote_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if components.iter().any(|part| matches!(*part, "." | "..")) {
        return Err(ProviderError::Other(format!(
            "unsafe remote folder path: {remote_path:?}"
        )));
    }
    Ok(components)
}

fn network_error(error: reqwest::Error) -> ProviderError {
    ProviderError::Network(error.to_string())
}
fn io_error(error: std::io::Error) -> ProviderError {
    ProviderError::Other(error.to_string())
}

async fn decode_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    resource: &str,
) -> ProviderResult<T> {
    if response.status().is_success() {
        response
            .json()
            .await
            .map_err(|e| ProviderError::Other(e.to_string()))
    } else {
        Err(response_error(response, resource).await)
    }
}

async fn response_error(response: reqwest::Response, resource: &str) -> ProviderError {
    let status = response.status();
    let retry_after_secs = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    match status.as_u16() {
        401 => ProviderError::AuthRequired,
        404 => ProviderError::NotFound(resource.to_owned()),
        429 => ProviderError::RateLimited { retry_after_secs },
        code => ProviderError::Other(format!("HTTP {code}")),
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

    #[test]
    fn partial_download_uses_a_sibling_temporary_file() {
        assert_eq!(
            temporary_path(Path::new("/tmp/report.pdf")),
            PathBuf::from("/tmp/report.pdf.algedi-part")
        );
    }

    #[test]
    fn validates_remote_folder_path_components() {
        assert_eq!(
            remote_path_components("/Work/2026/").unwrap(),
            vec!["Work", "2026"]
        );
        assert!(remote_path_components("/Work/../Secrets").is_err());
    }
}
