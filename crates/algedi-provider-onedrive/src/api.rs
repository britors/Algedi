//! Thin wrapper over the Microsoft Graph `/me/drive` endpoints actually
//! needed by the sync engine (metadata, upload, download, delete).

use serde::Deserialize;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

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
        Self { http: reqwest::Client::new(), access_token: std::sync::RwLock::new(access_token) }
    }

    pub fn set_access_token(&self, token: String) {
        *self.access_token.write().unwrap() = token;
    }

    fn access_token(&self) -> String {
        self.access_token.read().unwrap().clone()
    }

    pub async fn get_item(&self, item_id: &str) -> algedi_provider_trait::ProviderResult<DriveItem> {
        let url = format!("{GRAPH_BASE}/me/drive/items/{item_id}");
        self.get_json(&url).await
    }

    pub async fn upload_item(
        &self,
        _local_path: &std::path::Path,
        _parent_id: &str,
    ) -> algedi_provider_trait::ProviderResult<DriveItem> {
        // TODO: PUT /me/drive/items/{parent-id}:/{name}:/content for small
        // files; createUploadSession for files above ~4MB.
        Err(algedi_provider_trait::ProviderError::Other("not implemented".into()))
    }

    pub async fn download_item(
        &self,
        _item_id: &str,
        _dest_path: &std::path::Path,
    ) -> algedi_provider_trait::ProviderResult<()> {
        // TODO: GET /me/drive/items/{id}/content, streamed to disk.
        Err(algedi_provider_trait::ProviderError::Other("not implemented".into()))
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
        let api = GraphApi::new("old-token".into());
        assert_eq!(api.access_token(), "old-token");
        api.set_access_token("new-token".into());
        assert_eq!(api.access_token(), "new-token");
    }
}
