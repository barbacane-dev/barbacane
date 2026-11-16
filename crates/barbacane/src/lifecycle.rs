//! Work that outlives the request that started it: WebSocket relays, and the
//! middleware `on_response` run after a streamed response. The drain at
//! shutdown waits for it, and a relay closes its WebSocket when shutdown begins.

use std::future::Future;
use std::sync::LazyLock;

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Background tasks and the shutdown they observe.
pub struct Background {
    tasks: TaskTracker,
    shutdown: CancellationToken,
}

impl Background {
    pub fn new() -> Self {
        Self {
            tasks: TaskTracker::new(),
            shutdown: CancellationToken::new(),
        }
    }

    /// Run `task`, counted by [`Background::running`] until it ends.
    pub fn spawn<F>(&self, task: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tasks.spawn(task)
    }

    /// Tasks still running.
    pub fn running(&self) -> usize {
        self.tasks.len()
    }

    /// Signal shutdown to every task watching [`Background::shutdown`].
    pub fn begin_shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Cancelled once shutdown begins.
    pub fn shutdown(&self) -> CancellationToken {
        self.shutdown.clone()
    }
}

impl Default for Background {
    fn default() -> Self {
        Self::new()
    }
}

/// The process's background work.
pub fn background() -> &'static Background {
    static BACKGROUND: LazyLock<Background> = LazyLock::new(Background::new);
    &BACKGROUND
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_task_is_counted_until_it_ends() {
        let background = Background::new();
        assert_eq!(background.running(), 0);
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let handle = background.spawn(async move {
            let _ = wait.await;
        });
        assert_eq!(background.running(), 1);
        release.send(()).unwrap();
        handle.await.unwrap();
        // The tracker counts a task until its future is dropped.
        tokio::time::timeout(Duration::from_secs(1), async {
            while background.running() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the finished task is no longer counted");
    }

    #[tokio::test]
    async fn shutdown_reaches_every_watcher() {
        let background = Background::new();
        let watchers: Vec<_> = (0..3)
            .map(|_| {
                let shutdown = background.shutdown();
                background.spawn(async move { shutdown.cancelled().await })
            })
            .collect();
        assert!(!background.shutdown().is_cancelled());
        background.begin_shutdown();
        for watcher in watchers {
            tokio::time::timeout(Duration::from_secs(1), watcher)
                .await
                .expect("the watcher saw shutdown")
                .unwrap();
        }
        // A token taken after shutdown began is already cancelled.
        assert!(background.shutdown().is_cancelled());
    }
}
