//! Upstream endpoint pools: weighted selection and per-endpoint health
//! (ADR-0035).
//!
//! A pool is the list of endpoints and weights an operation dispatches to.
//! Requests are spread over it by smooth weighted round-robin, the algorithm
//! nginx uses, skipping endpoints whose circuit breaker is open. When every
//! endpoint left is open, they are all used, so that a burst of false
//! positives does not become an outage.
//!
//! The rotation of each pool and the breaker of each endpoint outlive requests
//! and artifact reloads; [`Balancer::retain`] drops what an artifact no longer
//! lists.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde::Deserialize;

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

/// One endpoint of a pool.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Endpoint {
    /// Base URL: scheme, host, port, and optionally a path prefix.
    pub url: String,
    /// Relative share of requests, at least 1.
    #[serde(default = "default_weight")]
    pub weight: u32,
}

fn default_weight() -> u32 {
    1
}

/// A pool, identified by its endpoints and their weights.
pub type PoolKey = Vec<(String, u32)>;

/// The key of `pool`.
pub fn pool_key(pool: &[Endpoint]) -> PoolKey {
    pool.iter().map(|e| (e.url.clone(), e.weight)).collect()
}

/// An endpoint as metrics, logs and spans show it: `scheme://host:port`. A full
/// URL is never shown, since it can carry user information or a query resolved
/// from a secret reference.
pub fn endpoint_id(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port_or_known_default()?;
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Some(format!("{}://{host}:{port}", parsed.scheme()))
}

/// Rotation and health state for every pool a gateway dispatches to.
pub struct Balancer {
    rotations: RwLock<HashMap<PoolKey, Arc<Mutex<Vec<i64>>>>>,
    breakers: RwLock<HashMap<String, Arc<CircuitBreaker>>>,
    breaker_config: CircuitBreakerConfig,
}

impl Default for Balancer {
    fn default() -> Self {
        Self::with_breaker_config(CircuitBreakerConfig::default())
    }
}

impl Balancer {
    /// A balancer whose breakers use `config`.
    pub fn with_breaker_config(breaker_config: CircuitBreakerConfig) -> Self {
        Self {
            rotations: RwLock::new(HashMap::new()),
            breakers: RwLock::new(HashMap::new()),
            breaker_config,
        }
    }

    /// The next endpoint of `pool` to try, skipping the indices in `tried`, or
    /// `None` when every endpoint has been tried.
    pub fn select(&self, pool: &[Endpoint], tried: &[usize]) -> Option<usize> {
        let candidates: Vec<usize> = (0..pool.len()).filter(|i| !tried.contains(i)).collect();
        if candidates.is_empty() {
            return None;
        }
        let healthy: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|&i| self.state(&pool[i]) != CircuitState::Open)
            .collect();
        let eligible = if healthy.is_empty() {
            candidates
        } else {
            healthy
        };

        let rotation = self.rotation(pool);
        let mut current = rotation.lock();
        let mut total = 0i64;
        for &i in &eligible {
            let weight = i64::from(pool[i].weight.max(1));
            current[i] += weight;
            total += weight;
        }
        let mut best = eligible[0];
        for &i in &eligible[1..] {
            if current[i] > current[best] {
                best = i;
            }
        }
        current[best] -= total;
        Some(best)
    }

    /// The breaker of the endpoint `id`, registered on first use.
    pub fn breaker(&self, id: &str) -> Arc<CircuitBreaker> {
        if let Some(breaker) = self.breakers.read().get(id) {
            return Arc::clone(breaker);
        }
        let mut breakers = self.breakers.write();
        Arc::clone(
            breakers.entry(id.to_string()).or_insert_with(|| {
                Arc::new(CircuitBreaker::named(id, self.breaker_config.clone()))
            }),
        )
    }

    /// Keep the state of `pools` and their endpoints, and drop the rest.
    pub fn retain(&self, pools: &[PoolKey]) {
        let keep: HashSet<&PoolKey> = pools.iter().collect();
        self.rotations.write().retain(|key, _| keep.contains(key));
        let endpoints: HashSet<String> = pools
            .iter()
            .flatten()
            .filter_map(|(url, _)| endpoint_id(url))
            .collect();
        self.breakers.write().retain(|id, _| endpoints.contains(id));
    }

    /// The state of `endpoint`'s breaker; one never used is closed.
    fn state(&self, endpoint: &Endpoint) -> CircuitState {
        endpoint_id(&endpoint.url)
            .and_then(|id| self.breakers.read().get(&id).map(|b| b.state()))
            .unwrap_or(CircuitState::Closed)
    }

    fn rotation(&self, pool: &[Endpoint]) -> Arc<Mutex<Vec<i64>>> {
        let key = pool_key(pool);
        if let Some(rotation) = self.rotations.read().get(&key) {
            return Arc::clone(rotation);
        }
        let mut rotations = self.rotations.write();
        Arc::clone(
            rotations
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(vec![0; pool.len()]))),
        )
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize) {
        (self.rotations.read().len(), self.breakers.read().len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn pool(weights: &[(&str, u32)]) -> Vec<Endpoint> {
        weights
            .iter()
            .map(|(url, weight)| Endpoint {
                url: url.to_string(),
                weight: *weight,
            })
            .collect()
    }

    fn sequence(balancer: &Balancer, pool: &[Endpoint], n: usize) -> Vec<usize> {
        (0..n)
            .map(|_| balancer.select(pool, &[]).unwrap())
            .collect()
    }

    fn opens_on_first_failure() -> Balancer {
        Balancer::with_breaker_config(CircuitBreakerConfig {
            failure_threshold: 1,
            failure_window: Duration::from_secs(60),
            reset_timeout: Duration::from_secs(60),
            success_threshold: 1,
        })
    }

    #[test]
    fn weights_interleave_as_nginx_does() {
        let balancer = Balancer::default();
        let p = pool(&[("https://a", 5), ("https://b", 1), ("https://c", 1)]);
        assert_eq!(sequence(&balancer, &p, 7), [0, 0, 1, 0, 2, 0, 0]);
        assert_eq!(sequence(&balancer, &p, 7), [0, 0, 1, 0, 2, 0, 0]);
    }

    #[test]
    fn equal_weights_are_plain_round_robin() {
        let balancer = Balancer::default();
        let p = pool(&[("https://a", 1), ("https://b", 1), ("https://c", 1)]);
        assert_eq!(sequence(&balancer, &p, 6), [0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn pools_with_the_same_endpoints_share_one_rotation() {
        let balancer = Balancer::default();
        let p = pool(&[("https://a", 1), ("https://b", 1)]);
        assert_eq!(balancer.select(&p, &[]), Some(0));
        assert_eq!(balancer.select(&p.clone(), &[]), Some(1));
    }

    #[test]
    fn tried_endpoints_are_skipped_until_none_is_left() {
        let balancer = Balancer::default();
        let p = pool(&[("https://a", 1), ("https://b", 1)]);
        assert_eq!(balancer.select(&p, &[0]), Some(1));
        assert_eq!(balancer.select(&p, &[1]), Some(0));
        assert_eq!(balancer.select(&p, &[0, 1]), None);
    }

    #[test]
    fn an_open_breaker_takes_its_endpoint_out_of_rotation() {
        let balancer = opens_on_first_failure();
        let p = pool(&[("https://a", 1), ("https://b", 1)]);
        balancer.breaker("https://a:443").record_failure();
        assert_eq!(sequence(&balancer, &p, 4), [1, 1, 1, 1]);
    }

    #[test]
    fn when_every_breaker_is_open_all_endpoints_are_used() {
        let balancer = opens_on_first_failure();
        let p = pool(&[("https://a", 1), ("https://b", 1)]);
        balancer.breaker("https://a:443").record_failure();
        balancer.breaker("https://b:443").record_failure();
        assert_eq!(sequence(&balancer, &p, 4), [0, 1, 0, 1]);
    }

    #[test]
    fn a_breaker_is_shared_by_every_pool_listing_its_endpoint() {
        let balancer = Balancer::default();
        assert!(Arc::ptr_eq(
            &balancer.breaker("https://a:443"),
            &balancer.breaker("https://a:443")
        ));
    }

    #[test]
    fn retain_drops_what_an_artifact_no_longer_lists() {
        let balancer = Balancer::default();
        let kept = pool(&[("https://a", 1), ("https://b", 1)]);
        let dropped = pool(&[("https://c", 1)]);
        balancer.select(&kept, &[]);
        balancer.select(&dropped, &[]);
        balancer.breaker("https://a:443");
        balancer.breaker("https://c:443");
        assert_eq!(balancer.counts(), (2, 2));

        balancer.retain(&[pool_key(&kept)]);
        assert_eq!(balancer.counts(), (1, 1));
        assert_eq!(balancer.select(&kept, &[]), Some(1), "the rotation is kept");
    }

    #[test]
    fn an_endpoint_is_shown_as_scheme_host_and_port() {
        assert_eq!(
            endpoint_id("https://user:pw@orders.internal/api?token=x").as_deref(),
            Some("https://orders.internal:443")
        );
        assert_eq!(
            endpoint_id("http://10.0.0.1:8080").as_deref(),
            Some("http://10.0.0.1:8080")
        );
        assert_eq!(
            endpoint_id("https://[::1]:8443").as_deref(),
            Some("https://[::1]:8443")
        );
        assert_eq!(endpoint_id("not a url"), None);
    }
}
