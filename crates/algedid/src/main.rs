//! algedid: background daemon, the single source of truth for sync state.
//! Runs as a systemd --user service and exposes org.lyraos.Algedi1 over the
//! session D-Bus. See PROMPT-ALGEDI.md sec. 1 and 8.

mod account_manager;
mod dbus_service;
mod provider_config;
mod scheduler;
mod secrets;

use account_manager::AccountManager;
use dbus_service::Algedi1;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let state_dir = state_dir()?;
    std::fs::create_dir_all(&state_dir)?;

    let accounts = Arc::new(Mutex::new(AccountManager::new(
        state_dir.join("algedi.sqlite"),
    )?));

    let service = Algedi1::new(accounts.clone());
    let conn = zbus::connection::Builder::session()?
        .name("org.lyraos.Algedi1")?
        .serve_at("/org/lyraos/Algedi1", service)?
        .build()
        .await?;

    tracing::info!("algedid listening on org.lyraos.Algedi1");

    scheduler::run_forever(accounts, conn).await
}

fn state_dir() -> anyhow::Result<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state"))
        })
        .ok_or_else(|| anyhow::anyhow!("cannot determine XDG_STATE_HOME or HOME"))?;
    Ok(base.join("algedi"))
}
