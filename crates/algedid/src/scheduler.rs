//! Orchestrates per-account sync cycles. PROMPT-ALGEDI.md sec. 6 requires
//! accounts not to block each other; each account should eventually get its
//! own spawned task driven by its `FolderWatcher` (local) and a poll timer
//! (remote). This is the placeholder single-task version.

use crate::account_manager::AccountManager;
use crate::dbus_service::Algedi1;
use algedi_core::{FolderWatcher, PairId, SyncAction, SyncEngine};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use zbus::object_server::InterfaceRef;

/// Default remote polling interval; configurable, minimum 15s to avoid
/// provider throttling (PROMPT-ALGEDI.md sec. 5.1).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);
pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(15);

fn poll_interval(configured_secs: Option<u64>) -> Duration {
    configured_secs
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_POLL_INTERVAL)
        .max(MIN_POLL_INTERVAL)
}

pub async fn run_forever(
    accounts: Arc<Mutex<AccountManager>>,
    conn: zbus::Connection,
) -> anyhow::Result<()> {
    let iface: InterfaceRef<Algedi1> = conn
        .object_server()
        .interface("/org/lyraos/Algedi1")
        .await?;

    let configured_poll_interval = accounts.lock().await.poll_interval_secs();
    let remote_poll_interval = poll_interval(configured_poll_interval);
    if configured_poll_interval.is_some_and(|seconds| seconds < MIN_POLL_INTERVAL.as_secs()) {
        tracing::warn!(
            configured_secs = configured_poll_interval,
            minimum_secs = MIN_POLL_INTERVAL.as_secs(),
            "poll interval raised to provider-safe minimum"
        );
    }
    tracing::info!(
        poll_interval_secs = remote_poll_interval.as_secs(),
        "remote polling configured"
    );
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    let mut next_remote_poll = Instant::now();
    let mut watchers: HashMap<PairId, FolderWatcher> = HashMap::new();
    loop {
        ticker.tick().await;

        let (available, state) = {
            let accounts = accounts.lock().await;
            (accounts.due_syncs(), accounts.state_handle())
        };
        let active: HashSet<_> = available.iter().map(|(pair, _)| pair.id).collect();
        watchers.retain(|pair_id, _| active.contains(pair_id));
        for (pair, _) in &available {
            if let std::collections::hash_map::Entry::Vacant(entry) = watchers.entry(pair.id) {
                match FolderWatcher::watch(&pair.local_path) {
                    Ok(watcher) => {
                        entry.insert(watcher);
                    }
                    Err(err) => {
                        tracing::warn!(pair_id = %pair.id, %err, "failed to watch local folder")
                    }
                }
            }
        }

        for (pair, provider) in &available {
            let Some(watcher) = watchers.get(&pair.id) else {
                continue;
            };
            while let Some(change) = watcher.try_recv() {
                let mut engine = SyncEngine::new(pair.clone(), provider.clone(), state.clone());
                let action = match engine.local_action(&change.path) {
                    Ok(Some(action)) => action,
                    Ok(None) => continue,
                    Err(err) => {
                        tracing::warn!(pair_id = %pair.id, path = %change.path.display(), %err, "failed to classify local change");
                        continue;
                    }
                };
                if let Err(err) = engine.apply(&action).await {
                    tracing::warn!(pair_id = %pair.id, ?action, %err, "failed to apply local change");
                    continue;
                }
                if let Some(relative_path) = action.relative_path() {
                    let local_path = pair
                        .local_path
                        .join(relative_path)
                        .to_string_lossy()
                        .into_owned();
                    emit_status_changed(
                        &iface,
                        &local_path,
                        engine.status_for(relative_path).as_str(),
                    )
                    .await;
                }
                emit_sync_progress(&iface, &pair.id.to_string(), 100).await;
            }
        }

        if Instant::now() < next_remote_poll {
            continue;
        }
        next_remote_poll = Instant::now() + remote_poll_interval;

        // Refresh expiring credentials, then snapshot due work and release
        // the AccountManager lock before running any sync cycle.
        let (due, state) = {
            let mut accounts = accounts.lock().await;
            accounts.refresh_expiring_tokens().await;
            (accounts.due_syncs(), accounts.state_handle())
        };

        if due.is_empty() {
            tracing::debug!("sync tick: nothing due (no account has a live provider yet)");
            continue;
        }

        for (pair, provider) in due {
            let pair_id = pair.id;
            let account_id = pair.account_id;
            let local_root = pair.local_path.clone();
            let mut engine = SyncEngine::new(pair, provider, state.clone());

            let actions = match engine.run_cycle().await {
                Ok(actions) => actions,
                Err(err) => {
                    tracing::warn!(%pair_id, %err, "sync cycle failed");
                    continue;
                }
            };

            let total = actions.len();
            for (index, action) in actions.iter().enumerate() {
                match engine.apply(action).await {
                    Ok(()) => {
                        if let Some(relative_path) = action.relative_path() {
                            let local_path = local_root
                                .join(relative_path)
                                .to_string_lossy()
                                .into_owned();
                            let status = engine.status_for(relative_path);
                            emit_status_changed(&iface, &local_path, status.as_str()).await;

                            if matches!(action, SyncAction::Conflict { .. }) {
                                emit_conflict_detected(
                                    &iface,
                                    &local_path,
                                    &account_id.to_string(),
                                )
                                .await;
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(%pair_id, ?action, %err, "failed to apply sync action");
                    }
                }

                let percent = (((index + 1) * 100) / total) as i32;
                emit_sync_progress(&iface, &pair_id.to_string(), percent).await;
            }
        }
    }
}

async fn emit_status_changed(iface: &InterfaceRef<Algedi1>, local_path: &str, status: &str) {
    if let Err(err) = Algedi1::status_changed(iface.signal_context(), local_path, status).await {
        tracing::warn!(%err, "failed to emit StatusChanged signal");
    }
}

async fn emit_conflict_detected(iface: &InterfaceRef<Algedi1>, path: &str, account_id: &str) {
    if let Err(err) = Algedi1::conflict_detected(iface.signal_context(), path, account_id).await {
        tracing::warn!(%err, "failed to emit ConflictDetected signal");
    }
}

async fn emit_sync_progress(iface: &InterfaceRef<Algedi1>, pair_id: &str, percent: i32) {
    if let Err(err) = Algedi1::sync_progress(iface.signal_context(), pair_id, percent).await {
        tracing::warn!(%err, "failed to emit SyncProgress signal");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_manager::AccountManager;
    use futures_util::StreamExt;
    use std::time::Duration as StdDuration;
    use zbus::{MatchRule, MessageStream};

    #[test]
    fn polling_uses_default_and_enforces_minimum() {
        assert_eq!(poll_interval(None), DEFAULT_POLL_INTERVAL);
        assert_eq!(poll_interval(Some(120)), Duration::from_secs(120));
        assert_eq!(poll_interval(Some(1)), MIN_POLL_INTERVAL);
        assert_eq!(poll_interval(Some(0)), MIN_POLL_INTERVAL);
    }

    /// Spins up a real Algedi1 service on the session bus (under a private
    /// test path, no well-known name claimed) and asserts that
    /// `emit_status_changed`/`emit_conflict_detected`/`emit_sync_progress`
    /// produce real D-Bus signals a subscriber receives — the same
    /// mechanism the Nautilus extension relies on for cache invalidation.
    #[tokio::test]
    async fn emits_real_dbus_signals() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = Arc::new(Mutex::new(
            AccountManager::new(dir.path().join("state.sqlite")).unwrap(),
        ));
        let service = Algedi1::new(accounts);

        let path = "/org/lyraos/AlgediSchedulerTest";
        let conn = zbus::connection::Builder::session()
            .unwrap()
            .serve_at(path, service)
            .unwrap()
            .build()
            .await
            .unwrap();
        let iface: InterfaceRef<Algedi1> = conn.object_server().interface(path).await.unwrap();

        let listener = zbus::Connection::session().await.unwrap();
        let rule = MatchRule::builder()
            .interface("org.lyraos.Algedi1")
            .unwrap()
            .path(path)
            .unwrap()
            .build();
        let mut stream = MessageStream::for_match_rule(rule, &listener, None)
            .await
            .unwrap();

        emit_status_changed(&iface, "/tmp/notes.txt", "synced").await;
        emit_conflict_detected(&iface, "/tmp/notes.txt", "account-1").await;
        emit_sync_progress(&iface, "pair-1", 42).await;

        let mut seen = std::collections::HashSet::new();
        tokio::time::timeout(StdDuration::from_secs(5), async {
            while seen.len() < 3 {
                let msg = stream.next().await.unwrap().unwrap();
                let Some(member) = msg.header().member().map(|m| m.as_str().to_owned()) else {
                    continue;
                };
                match member.as_str() {
                    "StatusChanged" => {
                        let (path, status): (String, String) = msg.body().deserialize().unwrap();
                        assert_eq!(path, "/tmp/notes.txt");
                        assert_eq!(status, "synced");
                        seen.insert("StatusChanged");
                    }
                    "ConflictDetected" => {
                        let (path, account_id): (String, String) =
                            msg.body().deserialize().unwrap();
                        assert_eq!(path, "/tmp/notes.txt");
                        assert_eq!(account_id, "account-1");
                        seen.insert("ConflictDetected");
                    }
                    "SyncProgress" => {
                        let (pair_id, percent): (String, i32) = msg.body().deserialize().unwrap();
                        assert_eq!(pair_id, "pair-1");
                        assert_eq!(percent, 42);
                        seen.insert("SyncProgress");
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("timed out waiting for all three signals");
    }
}
