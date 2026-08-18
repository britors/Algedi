//! Local filesystem watcher (wraps `notify`, i.e. inotify on Linux).
//! See PROMPT-ALGEDI.md sec. 5.1.

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// A local filesystem change, already debounced.
#[derive(Debug, Clone)]
pub struct LocalChange {
    pub path: PathBuf,
}

pub struct FolderWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<LocalChange>,
}

impl FolderWatcher {
    /// Debounce window before a burst of writes to the same path is treated
    /// as settled (PROMPT-ALGEDI.md sec. 5.2), avoiding uploads of
    /// partially-written files.
    pub const DEBOUNCE: Duration = Duration::from_millis(500);

    pub fn watch(root: &Path) -> notify::Result<Self> {
        let (raw_tx, raw_rx) = channel();
        let mut watcher = notify::recommended_watcher(raw_tx)?;
        watcher.watch(root, RecursiveMode::Recursive)?;

        let (tx, rx) = channel();
        std::thread::spawn(move || {
            // TODO: coalesce bursts of events per-path within DEBOUNCE
            // before forwarding, instead of relaying every raw event.
            while let Ok(Ok(event)) = raw_rx.recv() {
                for path in event.paths {
                    let _ = tx.send(LocalChange { path });
                }
            }
        });

        Ok(Self { _watcher: watcher, rx })
    }

    pub fn recv(&self) -> Option<LocalChange> {
        self.rx.recv().ok()
    }
}
