use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use uuid::Uuid;

/// Maximum number of concurrent MCP sessions. `initialize` is unauthenticated,
/// so without a cap a remote client could create sessions without bound and
/// exhaust memory before TTL eviction runs (MCP-2). When the cap is reached we
/// first drop expired sessions; if the store is still full, `create` fails
/// closed rather than growing unbounded.
const MAX_SESSIONS: usize = 10_000;

/// MCP session state.
///
/// Deliberately holds no client-supplied data: the `clientInfo` blob from
/// `initialize` is attacker-controlled and was previously retained here unused,
/// amplifying the unauthenticated-session memory-exhaustion vector (MCP-2). Only
/// the activity timestamp needed for TTL eviction is kept.
struct McpSession {
    last_active: Instant,
}

/// Thread-safe MCP session store with TTL eviction.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, McpSession>>,
    ttl: Duration,
}

impl SessionStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Create a new session and return its ID, or `None` if the session cap is
    /// reached even after evicting expired sessions (fail closed).
    pub fn create(&self) -> Option<String> {
        let mut sessions = self.sessions.lock();
        if sessions.len() >= MAX_SESSIONS {
            // Reclaim expired sessions before giving up.
            let now = Instant::now();
            sessions.retain(|_, session| now.duration_since(session.last_active) < self.ttl);
            if sessions.len() >= MAX_SESSIONS {
                return None;
            }
        }
        let id = Uuid::new_v4().to_string();
        sessions.insert(
            id.clone(),
            McpSession {
                last_active: Instant::now(),
            },
        );
        Some(id)
    }

    /// Touch a session (update last_active). Returns false if session doesn't exist.
    pub fn touch(&self, id: &str) -> bool {
        let mut sessions = self.sessions.lock();
        if let Some(session) = sessions.get_mut(id) {
            session.last_active = Instant::now();
            true
        } else {
            false
        }
    }

    /// Remove a session.
    pub fn remove(&self, id: &str) {
        self.sessions.lock().remove(id);
    }

    /// Evict expired sessions. Call periodically from a background task.
    pub fn evict_expired(&self) {
        let now = Instant::now();
        self.sessions
            .lock()
            .retain(|_, session| now.duration_since(session.last_active) < self.ttl);
    }

    /// Number of active sessions.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.sessions.lock().len()
    }

    /// Whether the session store is empty.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.sessions.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_touch() {
        let store = SessionStore::new(Duration::from_secs(60));
        let id = store.create().expect("under cap");
        assert!(store.touch(&id));
        assert!(!store.touch("nonexistent"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn remove_session() {
        let store = SessionStore::new(Duration::from_secs(60));
        let id = store.create().expect("under cap");
        assert_eq!(store.len(), 1);
        store.remove(&id);
        assert_eq!(store.len(), 0);
        assert!(!store.touch(&id));
    }

    #[test]
    fn evict_expired() {
        let store = SessionStore::new(Duration::from_millis(1));
        store.create().expect("under cap");
        store.create().expect("under cap");
        assert_eq!(store.len(), 2);
        std::thread::sleep(Duration::from_millis(10));
        store.evict_expired();
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn create_fails_closed_at_cap_but_reclaims_expired() {
        // A tiny TTL so every existing session is expired and reclaimable.
        let store = SessionStore::new(Duration::from_millis(1));
        {
            let mut sessions = store.sessions.lock();
            for _ in 0..MAX_SESSIONS {
                sessions.insert(
                    Uuid::new_v4().to_string(),
                    McpSession {
                        last_active: Instant::now(),
                    },
                );
            }
        }
        std::thread::sleep(Duration::from_millis(10));
        // At the cap, but the expired entries are reclaimed so create succeeds.
        assert!(store.create().is_some());

        // Now fill to the cap with fresh (non-expired) sessions: create fails closed.
        let store = SessionStore::new(Duration::from_secs(3600));
        {
            let mut sessions = store.sessions.lock();
            for _ in 0..MAX_SESSIONS {
                sessions.insert(
                    Uuid::new_v4().to_string(),
                    McpSession {
                        last_active: Instant::now(),
                    },
                );
            }
        }
        assert!(store.create().is_none());
    }
}
