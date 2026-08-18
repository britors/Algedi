//! Implements the org.lyraos.Algedi1 D-Bus interface (PROMPT-ALGEDI.md
//! sec. 8), delegating all state and logic to `AccountManager`. Mirrors
//! data/org.lyraos.Algedi1.xml — keep both in sync.
//!
//! Note: signal declarations use zbus 4's `#[interface]`/`#[zbus(signal)]`
//! pattern; double-check the exact signature against the zbus version
//! actually resolved by Cargo once dependencies are fetched.

use crate::account_manager::AccountManager;
use std::sync::Arc;
use tokio::sync::Mutex;
use zbus::interface;

pub struct Algedi1 {
    accounts: Arc<Mutex<AccountManager>>,
}

impl Algedi1 {
    pub fn new(accounts: Arc<Mutex<AccountManager>>) -> Self {
        Self { accounts }
    }
}

#[interface(name = "org.lyraos.Algedi1")]
impl Algedi1 {
    async fn add_account(&self, provider: String) -> zbus::fdo::Result<String> {
        let mut accounts = self.accounts.lock().await;
        accounts
            .add_account(&provider)
            .await
            .map(|id| id.to_string())
            .map_err(to_dbus_err)
    }

    async fn remove_account(&self, account_id: String) -> zbus::fdo::Result<()> {
        let id = parse_uuid(&account_id)?;
        self.accounts
            .lock()
            .await
            .remove_account(id)
            .await
            .map_err(to_dbus_err)
    }

    async fn list_accounts(&self) -> zbus::fdo::Result<Vec<(String, String, String)>> {
        let accounts = self.accounts.lock().await;
        Ok(accounts
            .list_accounts()
            .into_iter()
            .map(|a| (a.id.to_string(), a.provider, a.email))
            .collect())
    }

    async fn add_folder_pair(
        &self,
        account_id: String,
        local_path: String,
        remote_path: String,
    ) -> zbus::fdo::Result<String> {
        let id = parse_uuid(&account_id)?;
        let mut accounts = self.accounts.lock().await;
        accounts
            .add_folder_pair(id, local_path.into(), remote_path)
            .await
            .map(|pair_id| pair_id.to_string())
            .map_err(to_dbus_err)
    }

    async fn remove_folder_pair(&self, pair_id: String) -> zbus::fdo::Result<()> {
        let id = parse_uuid(&pair_id)?;
        self.accounts
            .lock()
            .await
            .remove_folder_pair(id)
            .await
            .map_err(to_dbus_err)
    }

    async fn get_file_status(&self, local_path: String) -> zbus::fdo::Result<String> {
        let accounts = self.accounts.lock().await;
        Ok(accounts.file_status(local_path.into()).as_str().to_string())
    }

    /// Resolves the folder pair that owns `local_path`, so thin clients
    /// (Nautilus extension, Vega) can drive PauseSync/ResumeSync/SyncNow
    /// and show a correct provider label without duplicating any sync
    /// logic. Returns empty strings/`pair_id` when no pair owns the path.
    async fn get_pair_for_path(&self, local_path: String) -> zbus::fdo::Result<(String, String, String, bool)> {
        let accounts = self.accounts.lock().await;
        Ok(
            match accounts.resolve_path(std::path::Path::new(&local_path)) {
                Some((pair_id, account_id, provider, paused)) => {
                    (pair_id.to_string(), account_id.to_string(), provider, paused)
                }
                None => (String::new(), String::new(), String::new(), false),
            },
        )
    }

    async fn pause_sync(&self, pair_id: String) -> zbus::fdo::Result<()> {
        let id = parse_uuid(&pair_id)?;
        self.accounts
            .lock()
            .await
            .set_paused(id, true)
            .await
            .map_err(to_dbus_err)
    }

    async fn resume_sync(&self, pair_id: String) -> zbus::fdo::Result<()> {
        let id = parse_uuid(&pair_id)?;
        self.accounts
            .lock()
            .await
            .set_paused(id, false)
            .await
            .map_err(to_dbus_err)
    }

    async fn sync_now(&self, pair_id: String) -> zbus::fdo::Result<()> {
        let id = parse_uuid(&pair_id)?;
        self.accounts
            .lock()
            .await
            .trigger_sync(id)
            .await
            .map_err(to_dbus_err)
    }

    async fn list_conflicts(&self) -> zbus::fdo::Result<Vec<(String, String, String)>> {
        let accounts = self.accounts.lock().await;
        Ok(accounts
            .list_conflicts()
            .into_iter()
            .map(|c| (c.path, c.account_id.to_string(), c.timestamp))
            .collect())
    }

    #[zbus(signal)]
    pub async fn status_changed(
        signal_ctxt: &zbus::SignalContext<'_>,
        local_path: &str,
        status: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn conflict_detected(
        signal_ctxt: &zbus::SignalContext<'_>,
        path: &str,
        account_id: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn sync_progress(
        signal_ctxt: &zbus::SignalContext<'_>,
        pair_id: &str,
        percent: i32,
    ) -> zbus::Result<()>;
}

fn parse_uuid(s: &str) -> zbus::fdo::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(s).map_err(|_| zbus::fdo::Error::InvalidArgs(format!("invalid id: {s}")))
}

fn to_dbus_err(e: anyhow::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(e.to_string())
}
