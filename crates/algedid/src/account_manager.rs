//! CRUD for accounts and folder pairs, and the OAuth onboarding lifecycle.
//! Owns the `StateDb` and one `AccountHandle` per account so that accounts
//! never block each other (PROMPT-ALGEDI.md sec. 6).

use crate::provider_config::ProviderConfig;
use crate::secrets::{self, StoredTokens};
use algedi_core::{AccountId, FolderPair, PairId, StateDb, SyncStatus};
use algedi_provider_trait::CloudProvider;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long before an access token's reported expiry we proactively
/// refresh it, so a sync cycle never starts a request with a token that
/// expires mid-flight.
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
pub struct Account {
    pub id: AccountId,
    pub provider: String,
    pub email: String,
}

#[derive(Debug, Clone)]
pub struct ConflictRecord {
    pub path: String,
    pub account_id: AccountId,
    pub timestamp: String,
}

/// Per-account state: sync handle plus its own scheduler loop (spawned by
/// `scheduler.rs`), independent from every other account's handle.
pub struct AccountHandle {
    pub account: Account,
    pub pairs: Vec<FolderPair>,
    /// `None` until `add_account`'s OAuth flow is implemented — accounts
    /// without a live provider are simply skipped by `due_syncs`.
    pub provider: Option<Arc<dyn CloudProvider>>,
    /// Cached in memory for the refresh cycle; the Secret Service entry
    /// (see `secrets.rs`) remains the source of truth.
    refresh_token: Option<String>,
    /// `None` if the provider never reported an expiry — such an account is
    /// never proactively refreshed (PROMPT-ALGEDI.md checklist: refresh
    /// without user intervention, best-effort when we know the deadline).
    expires_at: Option<Instant>,
}

/// Common shape of a freshly (re)authorized or refreshed token set,
/// independent of which provider issued it.
struct FreshTokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_in_secs: Option<u64>,
}

pub struct AccountManager {
    /// Shared (not held) across sync cycles: `scheduler.rs` runs cycles
    /// after releasing the outer `Mutex<AccountManager>`, so one slow
    /// network round-trip never blocks D-Bus calls for other accounts.
    state: Arc<Mutex<StateDb>>,
    handles: HashMap<AccountId, AccountHandle>,
    provider_config: ProviderConfig,
}

impl AccountManager {
    pub fn new(state_db_path: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(StateDb::open(&state_db_path)?)),
            handles: HashMap::new(),
            provider_config: ProviderConfig::load(),
        })
    }

    /// Handle to the shared state DB, for `scheduler.rs` to build
    /// `SyncEngine`s without holding the `AccountManager` lock.
    pub fn state_handle(&self) -> Arc<Mutex<StateDb>> {
        self.state.clone()
    }

    /// Every unpaused (pair, provider) ready to run a sync cycle. Accounts
    /// whose onboarding hasn't produced a live provider yet (all of them,
    /// until OAuth is implemented) are skipped.
    pub fn due_syncs(&self) -> Vec<(FolderPair, Arc<dyn CloudProvider>)> {
        self.handles
            .values()
            .filter_map(|h| h.provider.clone().map(|p| (h, p)))
            .flat_map(|(h, provider)| {
                h.pairs
                    .iter()
                    .filter(|p| !p.paused)
                    .map(move |p| (p.clone(), provider.clone()))
            })
            .collect()
    }

    /// Runs the OAuth2 loopback flow for `provider` (PROMPT-ALGEDI.md
    /// sec. 3), persists tokens through the Secret Service (sec. 4), fetches
    /// the account email, and registers a ready-to-sync `AccountHandle`.
    ///
    /// Requires credentials configured in `$XDG_CONFIG_HOME/algedi/providers.toml`
    /// or the `ALGEDI_*_CLIENT_ID`/`ALGEDI_*_CLIENT_SECRET` env vars — see
    /// docs/oauth-setup.md.
    pub async fn add_account(&mut self, provider: &str) -> anyhow::Result<AccountId> {
        let (email, tokens, live_provider): (String, StoredTokens, Arc<dyn CloudProvider>) = match provider {
            "gdrive" => self.onboard_gdrive().await?,
            "onedrive" => self.onboard_onedrive().await?,
            other => anyhow::bail!("unknown provider '{other}' (expected 'gdrive' or 'onedrive')"),
        };

        let account_id = uuid::Uuid::new_v4();
        secrets::store_tokens(account_id, provider, &email, &tokens).await?;

        self.handles.insert(
            account_id,
            AccountHandle {
                account: Account { id: account_id, provider: provider.to_string(), email },
                pairs: Vec::new(),
                provider: Some(live_provider),
            },
        );

        Ok(account_id)
    }

    async fn onboard_gdrive(&self) -> anyhow::Result<(String, StoredTokens, Arc<dyn CloudProvider>)> {
        let creds = &self.provider_config.gdrive;
        let client_id = creds.client_id.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "Google Drive client_id not configured — set [gdrive].client_id in \
                 $XDG_CONFIG_HOME/algedi/providers.toml or ALGEDI_GDRIVE_CLIENT_ID \
                 (see docs/oauth-setup.md)"
            )
        })?;

        let port = algedi_provider_gdrive::GDriveAuth::find_free_port()?;
        let auth = algedi_provider_gdrive::GDriveAuth::new(client_id, creds.client_secret.clone(), port);
        let tokens = auth.authorize(&[algedi_provider_gdrive::SCOPE_DRIVE_FILE]).await?;
        let email = algedi_provider_gdrive::fetch_account_email(&tokens.access_token).await?;

        let api = algedi_provider_gdrive::GDriveApi::new(tokens.access_token.clone());
        let live_provider: Arc<dyn CloudProvider> = Arc::new(algedi_provider_gdrive::GDriveProvider::new(api));

        Ok((
            email,
            StoredTokens { access_token: tokens.access_token, refresh_token: tokens.refresh_token },
            live_provider,
        ))
    }

    async fn onboard_onedrive(&self) -> anyhow::Result<(String, StoredTokens, Arc<dyn CloudProvider>)> {
        let creds = &self.provider_config.onedrive;
        let client_id = creds.client_id.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "OneDrive client_id not configured — set [onedrive].client_id in \
                 $XDG_CONFIG_HOME/algedi/providers.toml or ALGEDI_ONEDRIVE_CLIENT_ID \
                 (see docs/oauth-setup.md)"
            )
        })?;

        let port = algedi_provider_onedrive::OneDriveAuth::find_free_port()?;
        let auth = algedi_provider_onedrive::OneDriveAuth::new(client_id, port);
        let tokens = auth
            .authorize(&[
                algedi_provider_onedrive::SCOPE_FILES_READWRITE,
                algedi_provider_onedrive::SCOPE_OFFLINE_ACCESS,
            ])
            .await?;
        let email = algedi_provider_onedrive::fetch_account_email(&tokens.access_token).await?;

        let api = algedi_provider_onedrive::GraphApi::new(tokens.access_token.clone());
        let live_provider: Arc<dyn CloudProvider> = Arc::new(algedi_provider_onedrive::OneDriveProvider::new(api));

        Ok((
            email,
            StoredTokens { access_token: tokens.access_token, refresh_token: tokens.refresh_token },
            live_provider,
        ))
    }

    pub async fn remove_account(&mut self, id: AccountId) -> anyhow::Result<()> {
        if let Some(handle) = self.handles.get(&id) {
            if let Ok(tokens) = secrets::load_tokens(id).await {
                let revoke_result = match handle.account.provider.as_str() {
                    "gdrive" => algedi_provider_gdrive::revoke(&tokens.access_token).await,
                    "onedrive" => algedi_provider_onedrive::revoke(&tokens.access_token).await,
                    _ => Ok(()),
                };
                if let Err(err) = revoke_result {
                    tracing::warn!(%id, %err, "failed to revoke token with provider; removing local state anyway");
                }
            }
        }

        if let Err(err) = secrets::delete_tokens(id).await {
            tracing::warn!(%id, %err, "failed to delete Secret Service entry");
        }

        if let Some(handle) = self.handles.remove(&id) {
            let state = self.state.lock().unwrap();
            for pair in &handle.pairs {
                let _ = state.remove_folder_pair(pair.id);
            }
        }
        Ok(())
    }

    pub fn list_accounts(&self) -> Vec<Account> {
        self.handles.values().map(|h| h.account.clone()).collect()
    }

    pub async fn add_folder_pair(
        &mut self,
        account_id: AccountId,
        local_path: PathBuf,
        remote_path: String,
    ) -> anyhow::Result<PairId> {
        let handle = self
            .handles
            .get_mut(&account_id)
            .ok_or_else(|| anyhow::anyhow!("unknown account {account_id}"))?;

        let pair = FolderPair {
            id: uuid::Uuid::new_v4(),
            account_id,
            local_path,
            remote_path,
            remote_folder_id: String::new(), // TODO: resolve/create via provider API
            paused: false,
        };
        self.state.lock().unwrap().insert_folder_pair(&pair)?;
        let id = pair.id;
        handle.pairs.push(pair);
        Ok(id)
    }

    pub async fn remove_folder_pair(&mut self, pair_id: PairId) -> anyhow::Result<()> {
        self.state.lock().unwrap().remove_folder_pair(pair_id)?;
        for handle in self.handles.values_mut() {
            handle.pairs.retain(|p| p.id != pair_id);
        }
        Ok(())
    }

    pub fn file_status(&self, local_path: PathBuf) -> SyncStatus {
        // TODO: once the sync engine is wired into the scheduler, delegate
        // to the matching SyncEngine/StateDb for a real synced/syncing/
        // conflict status. Until then, the only thing we can honestly
        // report is whether the owning pair is paused.
        match self.resolve_path(&local_path) {
            Some((_, _, _, true)) => SyncStatus::Paused,
            _ => SyncStatus::Unknown,
        }
    }

    /// Finds the folder pair that owns `local_path`, i.e. the pair whose
    /// `local_path` is the longest matching ancestor. Used by the Nautilus
    /// extension (via `GetPairForPath`) to resolve context-menu actions and
    /// by `file_status` above.
    pub fn resolve_path(&self, local_path: &Path) -> Option<(PairId, AccountId, String, bool)> {
        let mut best: Option<&FolderPair> = None;
        for handle in self.handles.values() {
            for pair in &handle.pairs {
                if !local_path.starts_with(&pair.local_path) {
                    continue;
                }
                let is_more_specific = best
                    .map(|b| pair.local_path.components().count() > b.local_path.components().count())
                    .unwrap_or(true);
                if is_more_specific {
                    best = Some(pair);
                }
            }
        }
        best.and_then(|pair| {
            self.handles.get(&pair.account_id).map(|h| {
                (pair.id, pair.account_id, h.account.provider.clone(), pair.paused)
            })
        })
    }

    pub async fn set_paused(&mut self, pair_id: PairId, paused: bool) -> anyhow::Result<()> {
        self.state.lock().unwrap().set_paused(pair_id, paused)?;
        for handle in self.handles.values_mut() {
            if let Some(pair) = handle.pairs.iter_mut().find(|p| p.id == pair_id) {
                pair.paused = paused;
            }
        }
        Ok(())
    }

    pub async fn trigger_sync(&mut self, _pair_id: PairId) -> anyhow::Result<()> {
        // TODO: signal the corresponding scheduler task to run an immediate
        // cycle instead of waiting for the next poll tick.
        Ok(())
    }

    pub fn list_conflicts(&self) -> Vec<ConflictRecord> {
        let Ok(rows) = self.state.lock().unwrap().list_conflicts() else {
            return Vec::new();
        };

        rows.into_iter()
            .filter_map(|(pair_id, relative_path, _conflicting_copy_path, detected_at)| {
                self.handles.values().find_map(|h| {
                    h.pairs.iter().find(|p| p.id == pair_id).map(|p| ConflictRecord {
                        path: p.local_path.join(&relative_path).to_string_lossy().into_owned(),
                        account_id: p.account_id,
                        timestamp: detected_at.clone(),
                    })
                })
            })
            .collect()
    }
}

#[cfg(test)]
impl AccountManager {
    fn new_for_test() -> Self {
        Self {
            state: Arc::new(Mutex::new(StateDb::open_in_memory().unwrap())),
            handles: HashMap::new(),
            provider_config: ProviderConfig::default(),
        }
    }

    fn insert_test_pair(&mut self, provider: &str, local_path: PathBuf, paused: bool) -> PairId {
        let account_id = uuid::Uuid::new_v4();
        let pair = FolderPair {
            id: uuid::Uuid::new_v4(),
            account_id,
            local_path,
            remote_path: "/".into(),
            remote_folder_id: "root".into(),
            paused,
        };
        let pair_id = pair.id;
        self.handles.insert(
            account_id,
            AccountHandle {
                account: Account {
                    id: account_id,
                    provider: provider.into(),
                    email: "test@example.com".into(),
                },
                pairs: vec![pair],
                provider: None,
            },
        );
        pair_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_to_the_most_specific_pair() {
        let mut mgr = AccountManager::new_for_test();
        mgr.insert_test_pair("gdrive", PathBuf::from("/home/user/Drive"), false);
        let nested = mgr.insert_test_pair("onedrive", PathBuf::from("/home/user/Drive/Work"), true);

        let (pair_id, _account_id, provider, paused) = mgr
            .resolve_path(Path::new("/home/user/Drive/Work/report.docx"))
            .expect("should resolve to the nested pair");
        assert_eq!(pair_id, nested);
        assert_eq!(provider, "onedrive");
        assert!(paused);

        assert!(mgr
            .resolve_path(Path::new("/home/user/Pictures/photo.png"))
            .is_none());
    }

    #[test]
    fn paused_pair_reports_paused_status() {
        let mut mgr = AccountManager::new_for_test();
        mgr.insert_test_pair("gdrive", PathBuf::from("/home/user/Drive"), true);

        assert_eq!(
            mgr.file_status(PathBuf::from("/home/user/Drive/notes.txt")),
            SyncStatus::Paused
        );
        assert_eq!(
            mgr.file_status(PathBuf::from("/home/user/other/file.txt")),
            SyncStatus::Unknown
        );
    }
}
