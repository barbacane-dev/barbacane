//! Dev server file watcher.
//!
//! Watches spec files, manifest, and local plugin WASM files for changes,
//! bridging `notify` events to a tokio channel with debouncing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// File watcher for the dev server.
///
/// Watches a set of paths (files and directories) and produces debounced
/// change notifications via an async channel.
pub struct DevWatcher {
    watcher: RecommendedWatcher,
    rx: mpsc::UnboundedReceiver<PathBuf>,
    watched: HashSet<PathBuf>,
}

impl DevWatcher {
    /// Create a new watcher for the given paths.
    ///
    /// Files are watched non-recursively; directories are watched recursively
    /// (so new spec files in the specs folder are picked up automatically).
    pub fn new(paths: &[PathBuf]) -> Result<Self, String> {
        let (tx, rx) = mpsc::unbounded_channel();

        let watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    for path in event.paths {
                        let _ = tx.send(path);
                    }
                }
            },
            Config::default(),
        )
        .map_err(|e| format!("failed to create file watcher: {e}"))?;

        let mut dev_watcher = Self {
            watcher,
            rx,
            watched: HashSet::new(),
        };

        for path in paths {
            dev_watcher.watch(path)?;
        }

        Ok(dev_watcher)
    }

    /// Watch a single path (file or directory).
    fn watch(&mut self, path: &Path) -> Result<(), String> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        if self.watched.contains(&canonical) {
            return Ok(());
        }

        let mode = if canonical.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        self.watcher
            .watch(&canonical, mode)
            .map_err(|e| format!("failed to watch {}: {e}", canonical.display()))?;

        self.watched.insert(canonical);
        Ok(())
    }

    /// Wait for the next batch of file changes, debounced.
    ///
    /// Blocks until at least one change arrives, then collects any additional
    /// changes within the debounce window. Returns deduplicated changed paths.
    pub async fn next_change(&mut self, debounce: Duration) -> Vec<PathBuf> {
        // Wait for first event.
        let first = match self.rx.recv().await {
            Some(p) => p,
            None => return vec![],
        };

        let mut changed = vec![first];
        let deadline = tokio::time::Instant::now() + debounce;

        // Drain additional events within debounce window.
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                path = self.rx.recv() => {
                    match path {
                        Some(p) => {
                            if !changed.contains(&p) {
                                changed.push(p);
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        changed
    }

    /// Replace the set of watched paths.
    ///
    /// Unwatches paths no longer in the set and watches new ones.
    pub fn update_watches(&mut self, new_paths: &[PathBuf]) -> Result<(), String> {
        let new_set: HashSet<PathBuf> = new_paths
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();

        // Unwatch removed paths.
        let to_remove: Vec<PathBuf> = self.watched.difference(&new_set).cloned().collect();

        for path in &to_remove {
            let _ = self.watcher.unwatch(path);
            self.watched.remove(path);
        }

        // Watch new paths.
        for path in &new_set {
            if !self.watched.contains(path) {
                self.watch(path)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_creation_with_valid_paths() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("test.yaml");
        std::fs::write(&file, "test").unwrap();

        let watcher = DevWatcher::new(&[file.clone()]);
        assert!(watcher.is_ok());
    }

    #[test]
    fn watcher_creation_with_directory() {
        let temp = tempfile::tempdir().unwrap();
        let watcher = DevWatcher::new(&[temp.path().to_path_buf()]);
        assert!(watcher.is_ok());
    }

    #[tokio::test]
    async fn watcher_detects_file_change() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("test.yaml");
        std::fs::write(&file, "v1").unwrap();

        let mut watcher = DevWatcher::new(&[file.clone()]).unwrap();

        // Write a change after a small delay.
        let file_clone = file.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            std::fs::write(&file_clone, "v2").unwrap();
        });

        let changed = tokio::time::timeout(
            Duration::from_secs(5),
            watcher.next_change(Duration::from_millis(200)),
        )
        .await
        .expect("timed out waiting for change");

        assert!(!changed.is_empty());
    }

    #[tokio::test]
    async fn watcher_detects_new_file_in_directory() {
        let temp = tempfile::tempdir().unwrap();

        let mut watcher = DevWatcher::new(&[temp.path().to_path_buf()]).unwrap();

        // Create a new file after a small delay.
        let dir = temp.path().to_path_buf();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            std::fs::write(dir.join("new.yaml"), "content").unwrap();
        });

        let changed = tokio::time::timeout(
            Duration::from_secs(5),
            watcher.next_change(Duration::from_millis(200)),
        )
        .await
        .expect("timed out waiting for change");

        assert!(!changed.is_empty());
    }
}
