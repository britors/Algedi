//! Diff/reconciliation algorithm. Operates only through the `CloudProvider`
//! trait, never against a concrete adapter. See PROMPT-ALGEDI.md sec. 5.2.

use crate::state_db::StateDb;
use crate::{FolderPair, SyncStatus};
use algedi_provider_trait::{ChangeKind, CloudProvider, RemoteChange, RemoteFile};
use std::sync::{Arc, Mutex};

/// Action to take once local and remote state for a single file have been
/// compared against the last known synced state. Carries the remote-side
/// snapshot (`RemoteFile`) needed to actually apply the action.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncAction {
    Upload {
        relative_path: String,
        remote: RemoteFile,
    },
    Download {
        relative_path: String,
        remote: RemoteFile,
    },
    Conflict {
        relative_path: String,
        remote: RemoteFile,
    },
    DeleteLocal {
        relative_path: String,
    },
    NoOp,
}

impl SyncAction {
    /// The path this action concerns, relative to the pair's local root —
    /// used by callers (the scheduler) to build a `StatusChanged` signal
    /// without re-matching on every variant. `None` for `NoOp`.
    pub fn relative_path(&self) -> Option<&str> {
        match self {
            SyncAction::Upload { relative_path, .. }
            | SyncAction::Download { relative_path, .. }
            | SyncAction::Conflict { relative_path, .. }
            | SyncAction::DeleteLocal { relative_path } => Some(relative_path),
            SyncAction::NoOp => None,
        }
    }
}

pub struct SyncEngine {
    pub pair: FolderPair,
    provider: Arc<dyn CloudProvider>,
    /// Shared with every other `SyncEngine` the daemon runs concurrently —
    /// `AccountManager` owns a single `StateDb` per process, not one per
    /// pair (PROMPT-ALGEDI.md sec. 2).
    state: Arc<Mutex<StateDb>>,
}

impl SyncEngine {
    pub fn new(
        pair: FolderPair,
        provider: Arc<dyn CloudProvider>,
        state: Arc<Mutex<StateDb>>,
    ) -> Self {
        Self {
            pair,
            provider,
            state,
        }
    }

    /// One full reconciliation cycle: pull remote changes, then decide an
    /// action per changed path (upload / download / conflict) by comparing
    /// against the last known synced content hash.
    ///
    /// This only *decides* actions — call `apply` on each one to actually
    /// execute it.
    pub async fn run_cycle(&mut self) -> anyhow::Result<Vec<SyncAction>> {
        if self.pair.paused {
            return Ok(Vec::new());
        }

        let cursor = self.state.lock().unwrap().get_change_cursor(self.pair.id)?;
        let (remote_changes, new_cursor) = self.provider.list_changes(cursor.as_ref()).await?;
        let mut actions = Vec::with_capacity(remote_changes.len());
        for change in &remote_changes {
            let mut change = change.clone();
            if matches!(change.kind, ChangeKind::Deleted) {
                let known_path = self
                    .state
                    .lock()
                    .unwrap()
                    .relative_path_for_remote_id(self.pair.id, &change.file.remote_id)?;
                if let Some(known_path) = known_path {
                    change.file.name = known_path;
                } else if change.file.parent_id.is_some() {
                    let Some(relative_path) = self
                        .provider
                        .resolve_relative_path(&change.file, &self.pair.remote_folder_id)
                        .await?
                    else {
                        continue;
                    };
                    change.file.name = relative_path;
                } else {
                    continue;
                }
            } else {
                let Some(relative_path) = self
                    .provider
                    .resolve_relative_path(&change.file, &self.pair.remote_folder_id)
                    .await?
                else {
                    continue;
                };
                change.file.name = relative_path;
            }
            actions.push(self.classify(&change)?);
        }
        self.state
            .lock()
            .unwrap()
            .set_change_cursor(self.pair.id, &new_cursor)?;
        Ok(actions)
    }

    /// Compares a single remote change against the last-known synced hashes
    /// for that path, per the three-scenario table in PROMPT-ALGEDI.md
    /// sec. 5.2.
    fn classify(&self, change: &RemoteChange) -> anyhow::Result<SyncAction> {
        let relative_path = change.file.name.clone();
        let local_path = self.pair.local_path.join(&relative_path);

        let (known_local_hash, known_remote_hash) = self
            .state
            .lock()
            .unwrap()
            .get_hashes(self.pair.id, &relative_path)?;

        let current_local_hash = crate::hash_file(&local_path).ok();
        let local_changed = current_local_hash != known_local_hash;

        if matches!(change.kind, ChangeKind::Deleted) {
            return Ok(if local_changed {
                SyncAction::Conflict {
                    relative_path,
                    remote: change.file.clone(),
                }
            } else {
                SyncAction::DeleteLocal { relative_path }
            });
        }

        let current_remote_hash = change.file.content_hash.clone();
        let remote_changed = current_remote_hash != known_remote_hash;

        Ok(match (remote_changed, local_changed) {
            (true, true) => SyncAction::Conflict {
                relative_path,
                remote: change.file.clone(),
            },
            (true, false) => SyncAction::Download {
                relative_path,
                remote: change.file.clone(),
            },
            (false, true) => SyncAction::Upload {
                relative_path,
                remote: change.file.clone(),
            },
            (false, false) => SyncAction::NoOp,
        })
    }

    /// Executes a previously decided `SyncAction`: downloads/uploads
    /// content, writes a conflict copy, or deletes the local file — then
    /// updates `StateDb` so the next cycle sees the new synced state.
    pub async fn apply(&mut self, action: &SyncAction) -> anyhow::Result<()> {
        match action {
            SyncAction::Download {
                relative_path,
                remote,
            } => self.apply_download(relative_path, remote).await,
            SyncAction::Upload {
                relative_path,
                remote,
            } => self.apply_upload(relative_path, remote).await,
            SyncAction::Conflict {
                relative_path,
                remote,
            } => self.apply_conflict(relative_path, remote).await,
            SyncAction::DeleteLocal { relative_path } => self.apply_delete_local(relative_path),
            SyncAction::NoOp => Ok(()),
        }
    }

    async fn apply_download(
        &mut self,
        relative_path: &str,
        remote: &RemoteFile,
    ) -> anyhow::Result<()> {
        let local_path = self.pair.local_path.join(relative_path);

        if remote.is_folder {
            std::fs::create_dir_all(&local_path)?;
            self.state.lock().unwrap().record_synced(
                self.pair.id,
                relative_path,
                Some(&remote.remote_id),
                None,
                None,
            )?;
            return Ok(());
        }

        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.provider
            .download(&remote.remote_id, &local_path)
            .await?;
        let local_hash = crate::hash_file(&local_path)?;
        self.state.lock().unwrap().record_synced(
            self.pair.id,
            relative_path,
            Some(&remote.remote_id),
            Some(&local_hash),
            remote.content_hash.as_deref(),
        )?;
        Ok(())
    }

    async fn apply_upload(
        &mut self,
        relative_path: &str,
        remote: &RemoteFile,
    ) -> anyhow::Result<()> {
        if remote.is_folder {
            anyhow::bail!("uploading folder changes is not supported yet: {relative_path}");
        }

        let local_path = self.pair.local_path.join(relative_path);
        // NOTE: CloudProvider::upload is create-shaped. A real adapter
        // should use `remote.remote_id` to update the existing file's
        // content in place once the trait grows that capability, instead
        // of creating a duplicate.
        let uploaded = self
            .provider
            .upload(&local_path, &self.pair.remote_folder_id)
            .await?;
        let local_hash = crate::hash_file(&local_path)?;
        self.state.lock().unwrap().record_synced(
            self.pair.id,
            relative_path,
            Some(&uploaded.remote_id),
            Some(&local_hash),
            uploaded.content_hash.as_deref(),
        )?;
        Ok(())
    }

    async fn apply_conflict(
        &mut self,
        relative_path: &str,
        remote: &RemoteFile,
    ) -> anyhow::Result<()> {
        if remote.is_folder {
            anyhow::bail!("folder conflicts are not handled yet: {relative_path}");
        }

        let local_path = self.pair.local_path.join(relative_path);

        // Never overwrite silently (PROMPT-ALGEDI.md sec. 5.3): keep the
        // local edit under a distinct name, then let the original path
        // hold the synced (remote) version.
        if local_path.exists() {
            let hostname = hostname::get()
                .map(|h| h.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "desconhecido".into());
            let conflict_path =
                crate::conflicting_file_name(&local_path, &hostname, chrono::Local::now());
            std::fs::rename(&local_path, &conflict_path)?;
            self.state.lock().unwrap().record_conflict(
                self.pair.id,
                relative_path,
                &conflict_path,
            )?;
        }

        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.provider
            .download(&remote.remote_id, &local_path)
            .await?;
        let local_hash = crate::hash_file(&local_path)?;
        // Marked "conflict", not "synced": the path now holds the remote
        // version, but the auto-generated copy still needs the user's
        // attention (Nautilus badge, Atividade view).
        self.state.lock().unwrap().record_file_state(
            self.pair.id,
            relative_path,
            Some(&remote.remote_id),
            Some(&local_hash),
            remote.content_hash.as_deref(),
            "conflict",
        )?;
        Ok(())
    }

    fn apply_delete_local(&mut self, relative_path: &str) -> anyhow::Result<()> {
        let local_path = self.pair.local_path.join(relative_path);
        if local_path.exists() {
            std::fs::remove_file(&local_path)?;
        }
        self.state
            .lock()
            .unwrap()
            .remove_file_record(self.pair.id, relative_path)?;
        Ok(())
    }

    pub fn status_for(&self, relative_path: &str) -> SyncStatus {
        if self.pair.paused {
            return SyncStatus::Paused;
        }
        self.state
            .lock()
            .unwrap()
            .file_status(self.pair.id, relative_path)
            .unwrap_or(SyncStatus::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use algedi_provider_trait::{ChangeCursor, ProviderError, ProviderResult};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::path::Path;

    struct FakeProvider {
        changes: Vec<RemoteChange>,
        /// remote_id -> bytes served by `download`.
        contents: Mutex<HashMap<String, Vec<u8>>>,
        metadata: HashMap<String, RemoteFile>,
    }

    impl FakeProvider {
        fn new(changes: Vec<RemoteChange>) -> Self {
            Self {
                changes,
                contents: Mutex::new(HashMap::new()),
                metadata: HashMap::new(),
            }
        }

        fn with_content(self, remote_id: &str, bytes: &[u8]) -> Self {
            self.contents
                .lock()
                .unwrap()
                .insert(remote_id.to_string(), bytes.to_vec());
            self
        }

        fn with_metadata(mut self, file: RemoteFile) -> Self {
            self.metadata.insert(file.remote_id.clone(), file);
            self
        }
    }

    #[async_trait]
    impl CloudProvider for FakeProvider {
        fn provider_id(&self) -> &'static str {
            "fake"
        }

        async fn list_changes(
            &self,
            _cursor: Option<&ChangeCursor>,
        ) -> ProviderResult<(Vec<RemoteChange>, ChangeCursor)> {
            Ok((self.changes.clone(), "cursor-1".into()))
        }

        async fn upload(
            &self,
            local_path: &Path,
            remote_parent_id: &str,
        ) -> ProviderResult<RemoteFile> {
            let hash =
                crate::hash_file(local_path).map_err(|e| ProviderError::Other(e.to_string()))?;
            let size = std::fs::metadata(local_path).map(|m| m.len()).unwrap_or(0);
            let name = local_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            Ok(RemoteFile {
                remote_id: format!("uploaded-{name}"),
                name,
                parent_id: Some(remote_parent_id.to_string()),
                is_folder: false,
                size,
                content_hash: Some(hash),
                modified_at: chrono::Utc::now(),
            })
        }

        async fn download(&self, remote_id: &str, dest_path: &Path) -> ProviderResult<()> {
            let bytes = self
                .contents
                .lock()
                .unwrap()
                .get(remote_id)
                .cloned()
                .unwrap_or_default();
            std::fs::write(dest_path, bytes).map_err(|e| ProviderError::Other(e.to_string()))
        }

        async fn delete(&self, _remote_id: &str) -> ProviderResult<()> {
            Ok(())
        }

        async fn get_metadata(&self, remote_id: &str) -> ProviderResult<RemoteFile> {
            self.metadata
                .get(remote_id)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote_id.into()))
        }

        fn web_url(&self, _remote_id: &str) -> String {
            String::new()
        }

        fn set_access_token(&self, _access_token: String) {}
    }

    fn remote_file(name: &str, hash: &str) -> RemoteFile {
        RemoteFile {
            remote_id: format!("id-{name}"),
            name: name.into(),
            parent_id: Some("root".into()),
            is_folder: false,
            size: 0,
            content_hash: Some(hash.into()),
            modified_at: chrono::Utc::now(),
        }
    }

    fn remote_folder(id: &str, name: &str, parent_id: &str) -> RemoteFile {
        RemoteFile {
            remote_id: id.into(),
            name: name.into(),
            parent_id: Some(parent_id.into()),
            is_folder: true,
            size: 0,
            content_hash: None,
            modified_at: chrono::Utc::now(),
        }
    }

    fn test_pair(local_path: std::path::PathBuf) -> FolderPair {
        FolderPair {
            id: uuid::Uuid::new_v4(),
            account_id: uuid::Uuid::new_v4(),
            local_path,
            remote_path: "/".into(),
            remote_folder_id: "root".into(),
            paused: false,
        }
    }

    #[tokio::test]
    async fn downloads_when_only_remote_changed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"local content").unwrap();

        let state = StateDb::open_in_memory().unwrap();
        let pair = test_pair(dir.path().to_path_buf());
        state.insert_folder_pair(&pair).unwrap();
        let local_hash = crate::hash_file(&dir.path().join("notes.txt")).unwrap();
        state
            .record_synced(
                pair.id,
                "notes.txt",
                None,
                Some(&local_hash),
                Some("old-remote-hash"),
            )
            .unwrap();

        let file = remote_file("notes.txt", "new-remote-hash");
        let provider = Arc::new(FakeProvider::new(vec![RemoteChange {
            file: file.clone(),
            kind: ChangeKind::Modified,
        }]));

        let mut engine = SyncEngine::new(pair, provider, Arc::new(Mutex::new(state)));
        let actions = engine.run_cycle().await.unwrap();

        assert_eq!(
            actions,
            vec![SyncAction::Download {
                relative_path: "notes.txt".into(),
                remote: file
            }]
        );
    }

    #[tokio::test]
    async fn conflicts_when_both_sides_changed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"edited locally").unwrap();

        let state = StateDb::open_in_memory().unwrap();
        let pair = test_pair(dir.path().to_path_buf());
        state.insert_folder_pair(&pair).unwrap();
        state
            .record_synced(
                pair.id,
                "notes.txt",
                None,
                Some("old-local-hash"),
                Some("old-remote-hash"),
            )
            .unwrap();

        let file = remote_file("notes.txt", "new-remote-hash");
        let provider = Arc::new(FakeProvider::new(vec![RemoteChange {
            file: file.clone(),
            kind: ChangeKind::Modified,
        }]));

        let mut engine = SyncEngine::new(pair, provider, Arc::new(Mutex::new(state)));
        let actions = engine.run_cycle().await.unwrap();

        assert_eq!(
            actions,
            vec![SyncAction::Conflict {
                relative_path: "notes.txt".into(),
                remote: file
            }]
        );
    }

    #[tokio::test]
    async fn no_op_when_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"stable content").unwrap();

        let state = StateDb::open_in_memory().unwrap();
        let pair = test_pair(dir.path().to_path_buf());
        state.insert_folder_pair(&pair).unwrap();
        let local_hash = crate::hash_file(&dir.path().join("notes.txt")).unwrap();
        state
            .record_synced(
                pair.id,
                "notes.txt",
                None,
                Some(&local_hash),
                Some("stable-remote-hash"),
            )
            .unwrap();

        let provider = Arc::new(FakeProvider::new(vec![RemoteChange {
            file: remote_file("notes.txt", "stable-remote-hash"),
            kind: ChangeKind::Modified,
        }]));

        let mut engine = SyncEngine::new(pair, provider, Arc::new(Mutex::new(state)));
        let actions = engine.run_cycle().await.unwrap();

        assert_eq!(actions, vec![SyncAction::NoOp]);
    }

    #[tokio::test]
    async fn deleting_remote_file_untouched_locally_deletes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"still here").unwrap();

        let state = StateDb::open_in_memory().unwrap();
        let pair = test_pair(dir.path().to_path_buf());
        state.insert_folder_pair(&pair).unwrap();
        let local_hash = crate::hash_file(&dir.path().join("notes.txt")).unwrap();
        state
            .record_synced(
                pair.id,
                "notes.txt",
                None,
                Some(&local_hash),
                Some("old-remote-hash"),
            )
            .unwrap();

        let provider = Arc::new(FakeProvider::new(vec![RemoteChange {
            file: remote_file("notes.txt", "old-remote-hash"),
            kind: ChangeKind::Deleted,
        }]));

        let mut engine = SyncEngine::new(pair, provider, Arc::new(Mutex::new(state)));
        let actions = engine.run_cycle().await.unwrap();

        assert_eq!(
            actions,
            vec![SyncAction::DeleteLocal {
                relative_path: "notes.txt".into()
            }]
        );
    }

    #[tokio::test]
    async fn paused_pair_runs_no_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDb::open_in_memory().unwrap();
        let mut pair = test_pair(dir.path().to_path_buf());
        pair.paused = true;
        state.insert_folder_pair(&pair).unwrap();

        let provider = Arc::new(FakeProvider::new(vec![RemoteChange {
            file: remote_file("notes.txt", "hash"),
            kind: ChangeKind::Modified,
        }]));

        let mut engine = SyncEngine::new(pair, provider, Arc::new(Mutex::new(state)));
        assert_eq!(engine.run_cycle().await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn applying_download_writes_the_file_and_converges_to_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(StateDb::open_in_memory().unwrap()));
        let pair = test_pair(dir.path().to_path_buf());
        state.lock().unwrap().insert_folder_pair(&pair).unwrap();

        let file = remote_file("notes.txt", "remote-hash-1");
        let provider = Arc::new(
            FakeProvider::new(vec![RemoteChange {
                file: file.clone(),
                kind: ChangeKind::Modified,
            }])
            .with_content(&file.remote_id, b"downloaded content"),
        );

        let mut engine = SyncEngine::new(pair, provider, state.clone());

        let actions = engine.run_cycle().await.unwrap();
        assert_eq!(actions.len(), 1);
        for action in &actions {
            engine.apply(action).await.unwrap();
        }

        assert_eq!(
            std::fs::read(dir.path().join("notes.txt")).unwrap(),
            b"downloaded content"
        );

        // A second cycle against the same remote state should now be a
        // no-op: the loop has converged.
        let actions = engine.run_cycle().await.unwrap();
        assert_eq!(actions, vec![SyncAction::NoOp]);
    }

    #[tokio::test]
    async fn applying_conflict_preserves_both_versions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"my local edit").unwrap();

        let state = Arc::new(Mutex::new(StateDb::open_in_memory().unwrap()));
        let pair = test_pair(dir.path().to_path_buf());
        state.lock().unwrap().insert_folder_pair(&pair).unwrap();
        state
            .lock()
            .unwrap()
            .record_synced(
                pair.id,
                "notes.txt",
                None,
                Some("old-local-hash"),
                Some("old-remote-hash"),
            )
            .unwrap();

        let file = remote_file("notes.txt", "new-remote-hash");
        let provider = Arc::new(
            FakeProvider::new(vec![RemoteChange {
                file: file.clone(),
                kind: ChangeKind::Modified,
            }])
            .with_content(&file.remote_id, b"the remote version"),
        );

        let mut engine = SyncEngine::new(pair.clone(), provider, state.clone());
        let actions = engine.run_cycle().await.unwrap();
        for action in &actions {
            engine.apply(action).await.unwrap();
        }

        // Original path now holds the remote version — nothing lost.
        assert_eq!(
            std::fs::read(dir.path().join("notes.txt")).unwrap(),
            b"the remote version"
        );

        // The local edit survives under a conflict-named copy.
        let conflict_entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("notes (conflito de"))
            .collect();
        assert_eq!(conflict_entries.len(), 1);
        assert_eq!(
            std::fs::read(dir.path().join(&conflict_entries[0])).unwrap(),
            b"my local edit"
        );

        // And it's recorded for the Atividade view / ListConflicts D-Bus method.
        let conflicts = state.lock().unwrap().list_conflicts().unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].0, pair.id);
        assert_eq!(conflicts[0].1, "notes.txt");
    }

    #[tokio::test]
    async fn applying_delete_local_removes_file_and_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"bye").unwrap();

        let state = Arc::new(Mutex::new(StateDb::open_in_memory().unwrap()));
        let pair = test_pair(dir.path().to_path_buf());
        state.lock().unwrap().insert_folder_pair(&pair).unwrap();
        let local_hash = crate::hash_file(&dir.path().join("notes.txt")).unwrap();
        state
            .lock()
            .unwrap()
            .record_synced(
                pair.id,
                "notes.txt",
                None,
                Some(&local_hash),
                Some("old-remote-hash"),
            )
            .unwrap();

        let provider = Arc::new(FakeProvider::new(vec![RemoteChange {
            file: remote_file("notes.txt", "old-remote-hash"),
            kind: ChangeKind::Deleted,
        }]));

        let mut engine = SyncEngine::new(pair.clone(), provider, state.clone());
        let actions = engine.run_cycle().await.unwrap();
        for action in &actions {
            engine.apply(action).await.unwrap();
        }

        assert!(!dir.path().join("notes.txt").exists());
        assert_eq!(
            state
                .lock()
                .unwrap()
                .get_hashes(pair.id, "notes.txt")
                .unwrap(),
            (None, None)
        );
    }

    #[tokio::test]
    async fn resolves_nested_remote_path_below_pair_root() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDb::open_in_memory().unwrap();
        let pair = test_pair(dir.path().to_path_buf());
        state.insert_folder_pair(&pair).unwrap();
        let mut file = remote_file("report.txt", "hash");
        file.parent_id = Some("year".into());
        let provider = Arc::new(
            FakeProvider::new(vec![RemoteChange {
                file,
                kind: ChangeKind::Created,
            }])
            .with_metadata(remote_folder("year", "2026", "work"))
            .with_metadata(remote_folder("work", "Work", "root")),
        );
        let mut engine = SyncEngine::new(pair, provider, Arc::new(Mutex::new(state)));
        let actions = engine.run_cycle().await.unwrap();
        assert_eq!(actions[0].relative_path(), Some("Work/2026/report.txt"));
    }

    #[tokio::test]
    async fn ignores_remote_items_outside_pair_root() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDb::open_in_memory().unwrap();
        let pair = test_pair(dir.path().to_path_buf());
        state.insert_folder_pair(&pair).unwrap();
        let mut file = remote_file("private.txt", "hash");
        file.parent_id = Some("elsewhere".into());
        let provider = Arc::new(
            FakeProvider::new(vec![RemoteChange {
                file,
                kind: ChangeKind::Created,
            }])
            .with_metadata(remote_folder("elsewhere", "Elsewhere", "other-root"))
            .with_metadata(RemoteFile {
                remote_id: "other-root".into(),
                name: "Other root".into(),
                parent_id: None,
                is_folder: true,
                size: 0,
                content_hash: None,
                modified_at: chrono::Utc::now(),
            }),
        );
        let mut engine = SyncEngine::new(pair, provider, Arc::new(Mutex::new(state)));
        assert!(engine.run_cycle().await.unwrap().is_empty());
    }
}
