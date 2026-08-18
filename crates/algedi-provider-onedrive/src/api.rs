//! Thin wrapper over the Microsoft Graph `/me/drive` endpoints actually
//! needed by the sync engine (metadata, upload, download, delete).

use algedi_provider_trait::{ProviderError, ProviderResult};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const SIMPLE_UPLOAD_LIMIT: u64 = 4 * 1024 * 1024;
const UPLOAD_CHUNK_SIZE: usize = 10 * 320 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadSession {
    upload_url: String,
}

#[derive(Debug, Deserialize)]
pub struct DriveItem {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub folder: Option<serde_json::Value>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default, rename = "lastModifiedDateTime")]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub file: Option<FileFacet>,
    #[serde(default, rename = "parentReference")]
    pub parent_reference: Option<ParentReference>,
}

#[derive(Debug, Deserialize)]
pub struct FileFacet {
    #[serde(default)]
    pub hashes: Option<Hashes>,
}

#[derive(Debug, Deserialize)]
pub struct Hashes {
    #[serde(default, rename = "quickXorHash")]
    pub quick_xor_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ParentReference {
    pub id: String,
}

pub struct GraphApi {
    http: reqwest::Client,
    // RwLock, not a plain field: `CloudProvider::set_access_token` takes
    // `&self` (the provider is shared via `Arc` across concurrent calls),
    // so refreshing the token needs interior mutability.
    access_token: std::sync::RwLock<String>,
}

impl GraphApi {
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

    pub async fn get_item(
        &self,
        item_id: &str,
    ) -> algedi_provider_trait::ProviderResult<DriveItem> {
        let url = format!("{GRAPH_BASE}/me/drive/items/{item_id}");
        self.get_json(&url).await
    }

    pub async fn upload_item(
        &self,
        local_path: &Path,
        parent_id: &str,
    ) -> ProviderResult<DriveItem> {
        let name = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                ProviderError::Other("local path has no valid UTF-8 file name".into())
            })?;
        let encoded_name = utf8_percent_encode(name, NON_ALPHANUMERIC).to_string();
        let size = tokio::fs::metadata(local_path)
            .await
            .map_err(io_error)?
            .len();
        if size <= SIMPLE_UPLOAD_LIMIT {
            let body = tokio::fs::read(local_path).await.map_err(io_error)?;
            let response = self
                .http
                .put(format!(
                    "{GRAPH_BASE}/me/drive/items/{parent_id}:/{encoded_name}:/content"
                ))
                .bearer_auth(self.access_token())
                .body(body)
                .send()
                .await
                .map_err(network_error)?;
            return decode_json(response, name).await;
        }

        let response = self.http
            .post(format!("{GRAPH_BASE}/me/drive/items/{parent_id}:/{encoded_name}:/createUploadSession"))
            .bearer_auth(self.access_token())
            .json(&serde_json::json!({"item": {"@microsoft.graph.conflictBehavior": "rename", "name": name}}))
            .send().await.map_err(network_error)?;
        let session: UploadSession = decode_json(response, name).await?;
        let mut file = tokio::fs::File::open(local_path).await.map_err(io_error)?;
        let mut offset = 0u64;
        loop {
            let remaining = (size - offset) as usize;
            let count = remaining.min(UPLOAD_CHUNK_SIZE);
            let mut bytes = vec![0u8; count];
            tokio::io::AsyncReadExt::read_exact(&mut file, &mut bytes)
                .await
                .map_err(io_error)?;
            let end = offset + count as u64 - 1;
            let response = self
                .http
                .put(&session.upload_url)
                .header(reqwest::header::CONTENT_LENGTH, count)
                .header(
                    reqwest::header::CONTENT_RANGE,
                    format!("bytes {offset}-{end}/{size}"),
                )
                .body(bytes)
                .send()
                .await
                .map_err(network_error)?;
            if response.status() == reqwest::StatusCode::ACCEPTED {
                offset = end + 1;
                continue;
            }
            return decode_json(response, name).await;
        }
    }

    pub async fn download_item(&self, item_id: &str, dest_path: &Path) -> ProviderResult<()> {
        let mut response = self
            .http
            .get(format!("{GRAPH_BASE}/me/drive/items/{item_id}/content"))
            .bearer_auth(self.access_token())
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            return Err(response_error(response, item_id).await);
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

    pub async fn delete_item(&self, item_id: &str) -> algedi_provider_trait::ProviderResult<()> {
        let url = format!("{GRAPH_BASE}/me/drive/items/{item_id}");
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
        let api = GraphApi::new("old-token".into());
        assert_eq!(api.access_token(), "old-token");
        api.set_access_token("new-token".into());
        assert_eq!(api.access_token(), "new-token");
    }

    #[test]
    fn upload_chunks_follow_graph_alignment_requirement() {
        assert_eq!(UPLOAD_CHUNK_SIZE % (320 * 1024), 0);
    }

    #[test]
    fn partial_download_uses_a_sibling_temporary_file() {
        assert_eq!(
            temporary_path(Path::new("/tmp/report.pdf")),
            PathBuf::from("/tmp/report.pdf.algedi-part")
        );
    }
}
