//! Upstream load balancing and health tracking.
//!
//! Provides weighted round-robin load balancing with health tracking
//! for selecting backends. Unhealthy backends are skipped, and when
//! all backends are unhealthy, graceful degradation returns None.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::routing_api::Backend;

/// Health status of a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
}

/// Tracks health status of backends.
///
/// Thread-safe via DashMap. Unknown backends are considered healthy.
/// After `failure_threshold` consecutive failures, a backend is marked unhealthy.
/// A single success restores the backend to healthy status.
pub struct HealthTracker {
    health: DashMap<String, HealthStatus>,
    failures: DashMap<String, u32>,
    failure_threshold: u32,
}

impl HealthTracker {
    /// Creates a new HealthTracker with the given failure threshold.
    ///
    /// A backend becomes unhealthy after `failure_threshold` consecutive failures.
    pub fn new(failure_threshold: u32) -> Self {
        Self {
            health: DashMap::new(),
            failures: DashMap::new(),
            failure_threshold,
        }
    }

    /// Records a successful request to a backend.
    ///
    /// Resets failure count and restores backend to healthy status.
    pub fn record_success(&self, address: &str) {
        self.failures.insert(address.to_string(), 0);
        self.health.insert(address.to_string(), HealthStatus::Healthy);
    }

    /// Records a failed request to a backend.
    ///
    /// Increments failure count. If threshold is reached, marks backend unhealthy.
    pub fn record_failure(&self, address: &str) {
        let mut count = self.failures.entry(address.to_string()).or_insert(0);
        *count += 1;

        if *count >= self.failure_threshold {
            self.health
                .insert(address.to_string(), HealthStatus::Unhealthy);
        }
    }

    /// Returns whether a backend is healthy.
    ///
    /// Unknown backends are considered healthy.
    pub fn is_healthy(&self, address: &str) -> bool {
        self.health
            .get(address)
            .map(|status| *status == HealthStatus::Healthy)
            .unwrap_or(true)
    }

    /// Resets the health status of a backend.
    ///
    /// Clears failure count and removes health status (defaults to healthy).
    pub fn reset(&self, address: &str) {
        self.failures.remove(address);
        self.health.remove(address);
    }

    /// Returns the count of healthy backends from the given list.
    pub fn healthy_count(&self, backends: &[Backend]) -> usize {
        backends
            .iter()
            .filter(|b| self.is_healthy(&b.address))
            .count()
    }
}

/// Weighted round-robin load balancer with health awareness.
///
/// Thread-safe via atomic counter and shared HealthTracker.
/// Skips unhealthy backends. Returns None when all backends are unhealthy.
pub struct LoadBalancer {
    current: AtomicUsize,
    health_tracker: Arc<HealthTracker>,
}

impl LoadBalancer {
    /// Creates a new LoadBalancer with the given health tracker.
    pub fn new(health_tracker: Arc<HealthTracker>) -> Self {
        Self {
            current: AtomicUsize::new(0),
            health_tracker,
        }
    }

    /// Selects the next backend using weighted round-robin.
    ///
    /// Skips unhealthy backends. Returns None if all backends are unhealthy or empty.
    pub fn next<'a>(&self, backends: &'a [Backend]) -> Option<&'a Backend> {
        // Filter to healthy backends only
        let healthy: Vec<&Backend> = backends
            .iter()
            .filter(|b| self.health_tracker.is_healthy(&b.address))
            .collect();

        if healthy.is_empty() {
            return None;
        }

        // Expand backends by their weights
        let expanded: Vec<&Backend> = healthy
            .iter()
            .flat_map(|b| std::iter::repeat_n(*b, b.weight.max(1) as usize))
            .collect();

        if expanded.is_empty() {
            return None;
        }

        // Round-robin selection with atomic counter
        let idx = self.current.fetch_add(1, Ordering::Relaxed) % expanded.len();
        Some(expanded[idx])
    }

    /// Returns all healthy backends from the given list.
    pub fn healthy_backends<'a>(&self, backends: &'a [Backend]) -> Vec<&'a Backend> {
        backends
            .iter()
            .filter(|b| self.health_tracker.is_healthy(&b.address))
            .collect()
    }

    /// Returns a reference to the health tracker.
    pub fn health_tracker(&self) -> &Arc<HealthTracker> {
        &self.health_tracker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing_api::BackendProtocol;

    fn make_backend(address: &str, weight: u32) -> Backend {
        Backend {
            address: address.to_string(),
            weight,
            protocol: BackendProtocol::Http as i32,
        }
    }

    // ========== Phase 1: HealthTracker - Basic Status Tracking ==========

    #[test]
    fn test_health_tracker_new_backends_are_healthy() {
        let tracker = HealthTracker::new(3);
        assert!(tracker.is_healthy("backend:8080"));
        assert!(tracker.is_healthy("unknown:9090"));
    }

    #[test]
    fn test_health_tracker_record_success_keeps_healthy() {
        let tracker = HealthTracker::new(3);
        tracker.record_success("backend:8080");
        assert!(tracker.is_healthy("backend:8080"));
    }

    #[test]
    fn test_health_tracker_single_failure_stays_healthy() {
        let tracker = HealthTracker::new(3);
        tracker.record_failure("backend:8080");
        assert!(tracker.is_healthy("backend:8080"));
    }

    #[test]
    fn test_health_tracker_threshold_failures_becomes_unhealthy() {
        let tracker = HealthTracker::new(3);
        tracker.record_failure("backend:8080");
        tracker.record_failure("backend:8080");
        tracker.record_failure("backend:8080");
        assert!(!tracker.is_healthy("backend:8080"));
    }

    #[test]
    fn test_health_tracker_success_resets_failure_count() {
        let tracker = HealthTracker::new(3);
        // Two failures
        tracker.record_failure("backend:8080");
        tracker.record_failure("backend:8080");
        // Success resets count
        tracker.record_success("backend:8080");
        // Two more failures should not trigger unhealthy
        tracker.record_failure("backend:8080");
        tracker.record_failure("backend:8080");
        assert!(tracker.is_healthy("backend:8080"));
    }

    #[test]
    fn test_health_tracker_success_restores_unhealthy_backend() {
        let tracker = HealthTracker::new(3);
        // Make unhealthy
        tracker.record_failure("backend:8080");
        tracker.record_failure("backend:8080");
        tracker.record_failure("backend:8080");
        assert!(!tracker.is_healthy("backend:8080"));
        // Success restores
        tracker.record_success("backend:8080");
        assert!(tracker.is_healthy("backend:8080"));
    }

    #[test]
    fn test_health_tracker_reset_clears_status() {
        let tracker = HealthTracker::new(3);
        // Make unhealthy
        tracker.record_failure("backend:8080");
        tracker.record_failure("backend:8080");
        tracker.record_failure("backend:8080");
        assert!(!tracker.is_healthy("backend:8080"));
        // Reset clears status
        tracker.reset("backend:8080");
        assert!(tracker.is_healthy("backend:8080"));
    }

    #[test]
    fn test_health_tracker_healthy_count() {
        let tracker = HealthTracker::new(3);
        let backends = vec![
            make_backend("a:8080", 1),
            make_backend("b:8080", 1),
            make_backend("c:8080", 1),
        ];

        // All healthy initially
        assert_eq!(tracker.healthy_count(&backends), 3);

        // Mark one unhealthy
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");
        assert_eq!(tracker.healthy_count(&backends), 2);
    }

    // ========== Phase 2: LoadBalancer - Basic Round-Robin ==========

    #[test]
    fn test_load_balancer_single_backend_always_selected() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends = vec![make_backend("a:8080", 1)];

        for _ in 0..5 {
            let selected = lb.next(&backends).unwrap();
            assert_eq!(selected.address, "a:8080");
        }
    }

    #[test]
    fn test_load_balancer_two_backends_alternates() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends = vec![make_backend("a:8080", 1), make_backend("b:8080", 1)];

        let selections: Vec<String> = (0..4).map(|_| lb.next(&backends).unwrap().address.clone()).collect();
        assert_eq!(selections, vec!["a:8080", "b:8080", "a:8080", "b:8080"]);
    }

    #[test]
    fn test_load_balancer_three_backends_cycles() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends = vec![
            make_backend("a:8080", 1),
            make_backend("b:8080", 1),
            make_backend("c:8080", 1),
        ];

        let selections: Vec<String> = (0..6).map(|_| lb.next(&backends).unwrap().address.clone()).collect();
        assert_eq!(
            selections,
            vec!["a:8080", "b:8080", "c:8080", "a:8080", "b:8080", "c:8080"]
        );
    }

    #[test]
    fn test_load_balancer_wraps_around() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends = vec![
            make_backend("a:8080", 1),
            make_backend("b:8080", 1),
            make_backend("c:8080", 1),
        ];

        // Call 100 times and verify roughly equal distribution
        let mut counts = std::collections::HashMap::new();
        for _ in 0..99 {
            let addr = lb.next(&backends).unwrap().address.clone();
            *counts.entry(addr).or_insert(0) += 1;
        }

        assert_eq!(counts.get("a:8080"), Some(&33));
        assert_eq!(counts.get("b:8080"), Some(&33));
        assert_eq!(counts.get("c:8080"), Some(&33));
    }

    // ========== Phase 3: LoadBalancer - Weighted Round-Robin ==========

    #[test]
    fn test_weighted_round_robin_basic() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends = vec![make_backend("a:8080", 2), make_backend("b:8080", 1)];

        // Expanded: [a, a, b] -> cycle: a, a, b, a, a, b
        let selections: Vec<String> = (0..6).map(|_| lb.next(&backends).unwrap().address.clone()).collect();
        assert_eq!(
            selections,
            vec!["a:8080", "a:8080", "b:8080", "a:8080", "a:8080", "b:8080"]
        );
    }

    #[test]
    fn test_weighted_round_robin_distribution_80_20() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends = vec![make_backend("a:8080", 80), make_backend("b:8080", 20)];

        let mut a_count = 0;
        let mut b_count = 0;
        for _ in 0..100 {
            let addr = lb.next(&backends).unwrap().address.clone();
            if addr == "a:8080" {
                a_count += 1;
            } else {
                b_count += 1;
            }
        }

        assert_eq!(a_count, 80);
        assert_eq!(b_count, 20);
    }

    #[test]
    fn test_weighted_round_robin_zero_weight_treated_as_one() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends = vec![make_backend("a:8080", 1), make_backend("b:8080", 0)];

        // weight=0 treated as weight=1, so should alternate
        let selections: Vec<String> = (0..4).map(|_| lb.next(&backends).unwrap().address.clone()).collect();
        assert_eq!(selections, vec!["a:8080", "b:8080", "a:8080", "b:8080"]);
    }

    #[test]
    fn test_weighted_round_robin_large_weights() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends = vec![make_backend("a:8080", 100), make_backend("b:8080", 1)];

        let mut a_count = 0;
        let mut b_count = 0;
        for _ in 0..101 {
            let addr = lb.next(&backends).unwrap().address.clone();
            if addr == "a:8080" {
                a_count += 1;
            } else {
                b_count += 1;
            }
        }

        assert_eq!(a_count, 100);
        assert_eq!(b_count, 1);
    }

    // ========== Phase 4: Unhealthy Backend Skipping ==========

    #[test]
    fn test_load_balancer_skips_unhealthy_backend() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(Arc::clone(&tracker));
        let backends = vec![
            make_backend("a:8080", 1),
            make_backend("b:8080", 1),
            make_backend("c:8080", 1),
        ];

        // Mark b as unhealthy
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");

        // Should skip b
        let selections: Vec<String> = (0..4).map(|_| lb.next(&backends).unwrap().address.clone()).collect();
        assert_eq!(selections, vec!["a:8080", "c:8080", "a:8080", "c:8080"]);
    }

    #[test]
    fn test_load_balancer_all_healthy_restored() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(Arc::clone(&tracker));
        let backends = vec![make_backend("a:8080", 1), make_backend("b:8080", 1)];

        // Mark b as unhealthy
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");

        // Should only select a
        let addr1 = lb.next(&backends).unwrap().address.clone();
        let addr2 = lb.next(&backends).unwrap().address.clone();
        assert_eq!(addr1, "a:8080");
        assert_eq!(addr2, "a:8080");

        // Restore b
        tracker.record_success("b:8080");

        // Now should include b again
        // Reset counter for clean test
        let lb2 = LoadBalancer::new(Arc::clone(&tracker));
        let selections: Vec<String> = (0..4).map(|_| lb2.next(&backends).unwrap().address.clone()).collect();
        assert_eq!(selections, vec!["a:8080", "b:8080", "a:8080", "b:8080"]);
    }

    #[test]
    fn test_load_balancer_weighted_with_unhealthy() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(Arc::clone(&tracker));
        let backends = vec![
            make_backend("a:8080", 2),
            make_backend("b:8080", 1),
            make_backend("c:8080", 1),
        ];

        // Mark a (weight=2) as unhealthy
        tracker.record_failure("a:8080");
        tracker.record_failure("a:8080");
        tracker.record_failure("a:8080");

        // Should only use b and c (each weight=1)
        let selections: Vec<String> = (0..4).map(|_| lb.next(&backends).unwrap().address.clone()).collect();
        assert_eq!(selections, vec!["b:8080", "c:8080", "b:8080", "c:8080"]);
    }

    // ========== Phase 5: Graceful Degradation ==========

    #[test]
    fn test_load_balancer_all_backends_unhealthy_returns_none() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(Arc::clone(&tracker));
        let backends = vec![make_backend("a:8080", 1), make_backend("b:8080", 1)];

        // Mark all as unhealthy
        tracker.record_failure("a:8080");
        tracker.record_failure("a:8080");
        tracker.record_failure("a:8080");
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");

        assert!(lb.next(&backends).is_none());
    }

    #[test]
    fn test_load_balancer_empty_backends_returns_none() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(tracker);
        let backends: Vec<Backend> = vec![];

        assert!(lb.next(&backends).is_none());
    }

    #[test]
    fn test_healthy_backends_returns_only_healthy() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(Arc::clone(&tracker));
        let backends = vec![
            make_backend("a:8080", 1),
            make_backend("b:8080", 1),
            make_backend("c:8080", 1),
        ];

        // Mark b as unhealthy
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");

        let healthy = lb.healthy_backends(&backends);
        let addresses: Vec<&str> = healthy.iter().map(|b| b.address.as_str()).collect();
        assert_eq!(addresses, vec!["a:8080", "c:8080"]);
    }

    #[test]
    fn test_healthy_backends_empty_when_all_unhealthy() {
        let tracker = Arc::new(HealthTracker::new(3));
        let lb = LoadBalancer::new(Arc::clone(&tracker));
        let backends = vec![make_backend("a:8080", 1), make_backend("b:8080", 1)];

        // Mark all as unhealthy
        tracker.record_failure("a:8080");
        tracker.record_failure("a:8080");
        tracker.record_failure("a:8080");
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");
        tracker.record_failure("b:8080");

        assert!(lb.healthy_backends(&backends).is_empty());
    }

    // ========== Phase 6: Thread Safety ==========

    #[test]
    fn test_health_tracker_concurrent_updates() {
        use std::thread;

        let tracker = Arc::new(HealthTracker::new(3));
        let mut handles = vec![];

        for i in 0..10 {
            let tracker = Arc::clone(&tracker);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    if i % 2 == 0 {
                        tracker.record_failure("backend:8080");
                    } else {
                        tracker.record_success("backend:8080");
                    }
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // No panics = success
    }

    #[test]
    fn test_load_balancer_concurrent_next_calls() {
        use std::thread;

        let tracker = Arc::new(HealthTracker::new(3));
        let lb = Arc::new(LoadBalancer::new(tracker));
        let backends = vec![
            make_backend("a:8080", 1),
            make_backend("b:8080", 1),
            make_backend("c:8080", 1),
        ];

        let mut handles = vec![];
        for _ in 0..10 {
            let lb = Arc::clone(&lb);
            let backends = backends.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = lb.next(&backends);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // No panics = success
    }

    #[test]
    fn test_load_balancer_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LoadBalancer>();
        assert_send_sync::<HealthTracker>();
    }
}
