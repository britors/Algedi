//! Loads OAuth client credentials for each provider from
//! `$XDG_CONFIG_HOME/algedi/providers.toml`, with environment variable
//! overrides. See PROMPT-ALGEDI.md sec. 3.
//!
//! No credentials ship with this repo — until this file (or the env vars)
//! is populated, `AddAccount` fails with a clear error instead of silently
//! doing nothing. See docs/oauth-setup.md for how to register the app with
//! Google Cloud Console and Microsoft Entra ID.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderCredentials {
    pub client_id: Option<String>,
    /// Google issues one even for "Desktop app" clients; Azure AD's public
    /// client registrations don't use one at all.
    pub client_secret: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ProvidersFile {
    #[serde(default)]
    gdrive: ProviderCredentials,
    #[serde(default)]
    onedrive: ProviderCredentials,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderConfig {
    pub gdrive: ProviderCredentials,
    pub onedrive: ProviderCredentials,
}

impl ProviderConfig {
    /// Real entry point: reads the config file (if any) then applies env
    /// var overrides (`ALGEDI_GDRIVE_CLIENT_ID`, `ALGEDI_GDRIVE_CLIENT_SECRET`,
    /// `ALGEDI_ONEDRIVE_CLIENT_ID`).
    pub fn load() -> Self {
        let mut config = config_path()
            .as_deref()
            .map(Self::load_from_path)
            .unwrap_or_default();

        if let Ok(v) = std::env::var("ALGEDI_GDRIVE_CLIENT_ID") {
            config.gdrive.client_id = Some(v);
        }
        if let Ok(v) = std::env::var("ALGEDI_GDRIVE_CLIENT_SECRET") {
            config.gdrive.client_secret = Some(v);
        }
        if let Ok(v) = std::env::var("ALGEDI_ONEDRIVE_CLIENT_ID") {
            config.onedrive.client_id = Some(v);
        }

        config
    }

    fn load_from_path(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .map(|contents| Self::parse(&contents))
            .unwrap_or_default()
    }

    fn parse(toml_contents: &str) -> Self {
        let file: ProvidersFile = toml::from_str(toml_contents).unwrap_or_default();
        Self { gdrive: file.gdrive, onedrive: file.onedrive }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("algedi").join("providers.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_providers_from_toml() {
        let config = ProviderConfig::parse(
            r#"
            [gdrive]
            client_id = "gdrive-id.apps.googleusercontent.com"
            client_secret = "gdrive-secret"

            [onedrive]
            client_id = "11111111-2222-3333-4444-555555555555"
            "#,
        );

        assert_eq!(config.gdrive.client_id.as_deref(), Some("gdrive-id.apps.googleusercontent.com"));
        assert_eq!(config.gdrive.client_secret.as_deref(), Some("gdrive-secret"));
        assert_eq!(
            config.onedrive.client_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(config.onedrive.client_secret, None);
    }

    #[test]
    fn missing_file_yields_all_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = ProviderConfig::load_from_path(&dir.path().join("does-not-exist.toml"));
        assert_eq!(config.gdrive.client_id, None);
        assert_eq!(config.onedrive.client_id, None);
    }

    #[test]
    fn malformed_toml_yields_all_none_instead_of_panicking() {
        let config = ProviderConfig::parse("this is not valid toml {{{");
        assert_eq!(config.gdrive.client_id, None);
    }
}
