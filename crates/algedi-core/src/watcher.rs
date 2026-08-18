//! Local filesystem watcher (wraps `notify`, i.e. inotify on Linux).
//! See PROMPT-ALGEDI.md sec. 5.1.

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

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
            let mut pending = PendingChanges::default();
            loop {
                match raw_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(Ok(event)) => pending.record(event.paths, Instant::now()),
                    Ok(Err(error)) => tracing::warn!(%error, "filesystem watcher error"),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                for path in pending.take_settled(Instant::now(), Self::DEBOUNCE) {
                    if tx.send(LocalChange { path }).is_err() {
                        return;
                    }
                }
            }
        });

        Ok(Self {
            _watcher: watcher,
            rx,
        })
    }

    pub fn recv(&self) -> Option<LocalChange> {
        self.rx.recv().ok()
    }

    pub fn try_recv(&self) -> Option<LocalChange> {
        self.rx.try_recv().ok()
    }
}

#[derive(Default)]
struct PendingChanges {
    paths: HashMap<PathBuf, Instant>,
}

impl PendingChanges {
    fn record(&mut self, paths: Vec<PathBuf>, now: Instant) {
        for path in paths {
            self.paths.insert(path, now);
        }
    }

    fn take_settled(&mut self, now: Instant, debounce: Duration) -> Vec<PathBuf> {
        let mut settled: Vec<_> = self
            .paths
            .iter()
            .filter(|(_, seen)| now.saturating_duration_since(**seen) >= debounce)
            .map(|(path, _)| path.clone())
            .collect();
        settled.sort_by_key(|path| path.components().count());
        for path in &settled {
            self.paths.remove(path);
        }
        settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_bursts_per_path_until_debounce_expires() {
        let start = Instant::now();
        let mut pending = PendingChanges::default();
        pending.record(vec!["a.txt".into(), "b.txt".into()], start);
        pending.record(vec!["a.txt".into()], start + Duration::from_millis(300));
        assert_eq!(
            pending.take_settled(start + Duration::from_millis(550), FolderWatcher::DEBOUNCE),
            vec![PathBuf::from("b.txt")]
        );
        assert_eq!(
            pending.take_settled(start + Duration::from_millis(800), FolderWatcher::DEBOUNCE),
            vec![PathBuf::from("a.txt")]
        );
    }
}
