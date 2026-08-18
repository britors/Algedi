//! Persists OAuth tokens via the Secret Service (`oo7` / GNOME Keyring),
//! keyed by `algedi_account_id`. `StateDb` never sees these — tokens are
//! never written to SQLite (PROMPT-ALGEDI.md sec. 4).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

fn attributes(account_id: uuid::Uuid) -> HashMap<&'static str, String> {
    HashMap::from([("algedi_account_id", account_id.to_string())])
}

pub async fn store_tokens(
    account_id: uuid::Uuid,
    provider: &str,
    email: &str,
    tokens: &StoredTokens,
) -> anyhow::Result<()> {
    let keyring = oo7::Keyring::new().await?;
    let mut attrs = attributes(account_id);
    attrs.insert("provider", provider.to_string());
    attrs.insert("account_email", email.to_string());

    let secret = serde_json::to_vec(tokens)?;
    keyring
        .create_item(&format!("Algedi — {provider} — {email}"), &attrs, secret, true)
        .await?;
    Ok(())
}

pub async fn load_tokens(account_id: uuid::Uuid) -> anyhow::Result<StoredTokens> {
    let keyring = oo7::Keyring::new().await?;
    let items = keyring.search_items(&attributes(account_id)).await?;
    let item = items
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no stored tokens for account {account_id}"))?;
    let secret = item.secret().await?;
    Ok(serde_json::from_slice(&secret[..])?)
}

pub async fn delete_tokens(account_id: uuid::Uuid) -> anyhow::Result<()> {
    let keyring = oo7::Keyring::new().await?;
    keyring.delete(&attributes(account_id)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips against the real Secret Service running in this session
    /// — not mocked. Skips (rather than failing the suite) when no
    /// Secret Service is reachable, e.g. a headless CI runner.
    #[tokio::test]
    async fn round_trips_tokens_through_the_real_secret_service() {
        let Ok(keyring) = oo7::Keyring::new().await else {
            eprintln!("no Secret Service reachable, skipping");
            return;
        };
        drop(keyring);

        let account_id = uuid::Uuid::new_v4();
        let tokens = StoredTokens {
            access_token: "access-123".into(),
            refresh_token: Some("refresh-456".into()),
        };

        store_tokens(account_id, "gdrive", "person@example.com", &tokens)
            .await
            .unwrap();

        let loaded = load_tokens(account_id).await.unwrap();
        assert_eq!(loaded.access_token, tokens.access_token);
        assert_eq!(loaded.refresh_token, tokens.refresh_token);

        delete_tokens(account_id).await.unwrap();
        assert!(load_tokens(account_id).await.is_err());
    }
}
