use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use uuid::Uuid;

/// MCP session state.
struct McpSession {
    _client_info: Option<serde_json::Value>,
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

    /// Create a new session and return its ID.
    pub fn create(&self, client_info: Option<serde_json::Value>) -> String {
        let id = Uuid::new_v4().to_string();
        let session = McpSession {
            _client_info: client_info,
            last_active: Instant::now(),
        };
        self.sessions.lock().insert(id.clone(), session);
        id
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
        let id = store.create(None);
        assert!(store.touch(&id));
        assert!(!store.touch("nonexistent"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn remove_session() {
        let store = SessionStore::new(Duration::from_secs(60));
        let id = store.create(None);
        assert_eq!(store.len(), 1);
        store.remove(&id);
        assert_eq!(store.len(), 0);
        assert!(!store.touch(&id));
    }

    #[test]
    fn evict_expired() {
        let store = SessionStore::new(Duration::from_millis(1));
        store.create(None);
        store.create(None);
        assert_eq!(store.len(), 2);
        std::thread::sleep(Duration::from_millis(10));
        store.evict_expired();
        assert_eq!(store.len(), 0);
    }
}
