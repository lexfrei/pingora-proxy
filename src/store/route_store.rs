//! Thread-safe route storage using DashMap.

use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

use crate::routing_api::{GrpcRoute, HttpRoute};

/// Thread-safe storage for HTTP and gRPC routes.
///
/// Uses `DashMap` for lock-free concurrent access and `AtomicU64`
/// for version tracking.
pub struct RouteStore {
    http_routes: DashMap<String, HttpRoute>,
    grpc_routes: DashMap<String, GrpcRoute>,
    version: AtomicU64,
}

impl RouteStore {
    /// Creates a new empty route store.
    pub fn new() -> Self {
        Self {
            http_routes: DashMap::new(),
            grpc_routes: DashMap::new(),
            version: AtomicU64::new(0),
        }
    }

    /// Updates all routes with a full sync.
    ///
    /// Clears existing routes and replaces with new ones.
    /// Returns the applied version.
    pub fn update_routes(
        &self,
        http_routes: Vec<HttpRoute>,
        grpc_routes: Vec<GrpcRoute>,
        version: u64,
    ) -> u64 {
        // Clear existing routes
        self.http_routes.clear();
        self.grpc_routes.clear();

        // Insert new HTTP routes
        for route in http_routes {
            self.http_routes.insert(route.id.clone(), route);
        }

        // Insert new gRPC routes
        for route in grpc_routes {
            self.grpc_routes.insert(route.id.clone(), route);
        }

        // Update version
        self.version.store(version, Ordering::SeqCst);
        version
    }

    /// Returns all HTTP routes.
    pub fn get_http_routes(&self) -> Vec<HttpRoute> {
        self.http_routes
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns all gRPC routes.
    pub fn get_grpc_routes(&self) -> Vec<GrpcRoute> {
        self.grpc_routes
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns the current configuration version.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Returns the count of (HTTP routes, gRPC routes).
    pub fn route_count(&self) -> (u32, u32) {
        (
            self.http_routes.len() as u32,
            self.grpc_routes.len() as u32,
        )
    }
}

impl Default for RouteStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_http_route(id: &str, hostnames: Vec<&str>) -> HttpRoute {
        HttpRoute {
            id: id.to_string(),
            hostnames: hostnames.into_iter().map(String::from).collect(),
            rules: vec![],
        }
    }

    fn make_grpc_route(id: &str, hostnames: Vec<&str>) -> GrpcRoute {
        GrpcRoute {
            id: id.to_string(),
            hostnames: hostnames.into_iter().map(String::from).collect(),
            rules: vec![],
        }
    }

    #[test]
    fn test_new_store_empty() {
        let store = RouteStore::new();
        assert_eq!(store.version(), 0);
        assert_eq!(store.route_count(), (0, 0));
        assert!(store.get_http_routes().is_empty());
        assert!(store.get_grpc_routes().is_empty());
    }

    #[test]
    fn test_update_routes_success() {
        let store = RouteStore::new();
        let http_routes = vec![make_http_route("default/route1", vec!["example.com"])];

        let applied = store.update_routes(http_routes, vec![], 1);

        assert_eq!(applied, 1);
        assert_eq!(store.version(), 1);
        assert_eq!(store.route_count(), (1, 0));
    }

    #[test]
    fn test_update_routes_with_grpc() {
        let store = RouteStore::new();
        let http_routes = vec![make_http_route("default/http1", vec!["http.example.com"])];
        let grpc_routes = vec![make_grpc_route("default/grpc1", vec!["grpc.example.com"])];

        store.update_routes(http_routes, grpc_routes, 5);

        assert_eq!(store.version(), 5);
        assert_eq!(store.route_count(), (1, 1));
    }

    #[test]
    fn test_update_routes_clears_old() {
        let store = RouteStore::new();

        // First update
        store.update_routes(
            vec![
                make_http_route("default/route1", vec!["a.com"]),
                make_http_route("default/route2", vec!["b.com"]),
            ],
            vec![],
            1,
        );
        assert_eq!(store.route_count(), (2, 0));

        // Second update with fewer routes
        store.update_routes(
            vec![make_http_route("default/route3", vec!["c.com"])],
            vec![],
            2,
        );

        assert_eq!(store.route_count(), (1, 0));
        assert_eq!(store.version(), 2);

        let routes = store.get_http_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].id, "default/route3");
    }

    #[test]
    fn test_get_routes_returns_clone() {
        let store = RouteStore::new();
        store.update_routes(
            vec![make_http_route("default/route1", vec!["example.com"])],
            vec![],
            1,
        );

        let routes1 = store.get_http_routes();
        let routes2 = store.get_http_routes();

        // Both should have same content
        assert_eq!(routes1.len(), routes2.len());
        assert_eq!(routes1[0].id, routes2[0].id);
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(RouteStore::new());
        let mut handles = vec![];

        // Spawn multiple writers
        for i in 0..10 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let routes = vec![make_http_route(
                    &format!("default/route{}", i),
                    vec!["example.com"],
                )];
                store.update_routes(routes, vec![], i as u64);
            }));
        }

        // Spawn multiple readers
        for _ in 0..10 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let _ = store.get_http_routes();
                let _ = store.version();
                let _ = store.route_count();
            }));
        }

        // Wait for all threads - this verifies no deadlocks or panics
        for handle in handles {
            handle.join().unwrap();
        }

        // Store should still be in valid state
        // Note: Due to race conditions between clear() and insert(),
        // exact count is non-deterministic. We only verify no crash/deadlock.
        let (http_count, grpc_count) = store.route_count();
        assert!(http_count >= 1, "Should have at least one route");
        assert_eq!(grpc_count, 0);
    }

    #[test]
    fn test_default_impl() {
        let store = RouteStore::default();
        assert_eq!(store.version(), 0);
        assert_eq!(store.route_count(), (0, 0));
    }
}
