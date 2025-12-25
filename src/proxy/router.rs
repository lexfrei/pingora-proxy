//! Route matching for incoming HTTP requests.
//!
//! Matches requests to backends based on hostname and path,
//! following Gateway API specification for match priority.

use std::sync::Arc;

use rand::Rng;

use crate::routing_api::{Backend, HttpRouteMatch, PathMatchType};
use crate::store::RouteStore;

/// Routes incoming HTTP requests to appropriate backends.
///
/// Thread-safe via shared reference to RouteStore.
/// Implements Gateway API matching priority:
/// - Hostname: Exact > Wildcard > Empty (match all)
/// - Path: Exact > Prefix (longer wins) > Regex
pub struct Router {
    store: Arc<RouteStore>,
}

impl Router {
    /// Creates a new Router with the given route store.
    pub fn new(store: Arc<RouteStore>) -> Self {
        Self { store }
    }

    /// Finds a backend for the given host and path.
    ///
    /// Returns `None` if no matching route is found.
    pub fn find_backend(&self, host: &str, path: &str) -> Option<Backend> {
        let routes = self.store.get_http_routes();

        // Track best match: (hostname_score, path_score, backend)
        let mut best_match: Option<(usize, usize, Backend)> = None;

        for route in &routes {
            // Check hostname match
            let hostname_score = match Self::best_hostname_score(&route.hostnames, host) {
                Some(score) => score,
                None => continue,
            };

            // Check rules
            for rule in &route.rules {
                if rule.backends.is_empty() {
                    continue;
                }

                // Check matches (OR logic - any match in rule is sufficient)
                if let Some(path_score) = Self::best_path_score(&rule.matches, path) {
                    let total_score = (hostname_score, path_score);

                    let is_better = match &best_match {
                        None => true,
                        Some((hs, ps, _)) => total_score > (*hs, *ps),
                    };

                    if is_better {
                        let backend = Self::select_backend(&rule.backends);
                        best_match = Some((hostname_score, path_score, backend));
                    }
                }
            }
        }

        best_match.map(|(_, _, b)| b)
    }

    /// Returns the best hostname score for a list of patterns.
    /// Returns None if no pattern matches.
    fn best_hostname_score(hostnames: &[String], host: &str) -> Option<usize> {
        if hostnames.is_empty() {
            // Empty hostnames = match all hosts with lowest priority
            return Some(0);
        }
        hostnames
            .iter()
            .filter_map(|h| Self::hostname_score(h, host))
            .max()
    }

    /// Scores a hostname match (higher = better match).
    /// Exact matches score higher than wildcards.
    fn hostname_score(pattern: &str, host: &str) -> Option<usize> {
        if !Self::hostname_matches(pattern, host) {
            return None;
        }

        if pattern.starts_with("*.") {
            // Wildcard: score = pattern length (less specific)
            Some(pattern.len())
        } else {
            // Exact: score = pattern length + 1000 (more specific)
            Some(pattern.len() + 1000)
        }
    }

    /// Checks if a hostname matches a pattern.
    /// Supports exact match and wildcard (*.example.com).
    fn hostname_matches(pattern: &str, host: &str) -> bool {
        let host = Self::normalize_hostname(host);
        let pattern = pattern.to_lowercase();

        if pattern.starts_with("*.") {
            // Wildcard: *.example.com matches foo.example.com
            // But NOT example.com (no subdomain)
            // But NOT foo.bar.example.com (multiple levels)
            let suffix = &pattern[1..]; // ".example.com"
            if !host.ends_with(suffix) {
                return false;
            }
            let prefix = &host[..host.len() - suffix.len()];
            !prefix.is_empty() && !prefix.contains('.')
        } else {
            // Exact match
            host == pattern
        }
    }

    /// Normalizes a hostname: lowercase and strip port.
    fn normalize_hostname(host: &str) -> String {
        host.split(':').next().unwrap_or(host).to_lowercase()
    }

    /// Returns the best path score for a list of matches.
    fn best_path_score(matches: &[HttpRouteMatch], path: &str) -> Option<usize> {
        if matches.is_empty() {
            // No matches = match all paths with lowest priority
            return Some(0);
        }

        matches
            .iter()
            .filter_map(|m| Self::path_match_score(m, path))
            .max()
    }

    /// Scores a path match within an HttpRouteMatch.
    fn path_match_score(route_match: &HttpRouteMatch, path: &str) -> Option<usize> {
        match &route_match.path {
            Some(pm) => {
                let match_type = PathMatchType::try_from(pm.r#type).unwrap_or(PathMatchType::Unspecified);
                Self::path_score(match_type, &pm.value, path)
            }
            None => Some(0), // No path match = match all paths
        }
    }

    /// Scores a path match (higher = better).
    /// Exact > longer prefix > shorter prefix > regex
    fn path_score(match_type: PathMatchType, pattern: &str, path: &str) -> Option<usize> {
        if !Self::path_matches(match_type, pattern, path) {
            return None;
        }

        match match_type {
            PathMatchType::Exact => Some(10000 + pattern.len()),
            PathMatchType::Prefix => Some(1000 + pattern.len()),
            PathMatchType::Regex => Some(500), // Lower priority than prefix
            PathMatchType::Unspecified => Some(0),
        }
    }

    /// Checks if a path matches a pattern with given match type.
    fn path_matches(match_type: PathMatchType, pattern: &str, path: &str) -> bool {
        match match_type {
            PathMatchType::Exact => path == pattern,
            PathMatchType::Prefix => Self::prefix_matches(pattern, path),
            PathMatchType::Regex => {
                // MVP: treat regex as prefix for now
                Self::prefix_matches(pattern, path)
            }
            PathMatchType::Unspecified => true, // No path = match all
        }
    }

    /// Prefix matching respecting segment boundaries.
    /// /api matches /api, /api/, /api/users
    /// /api does NOT match /apikeys (no segment boundary)
    fn prefix_matches(prefix: &str, path: &str) -> bool {
        if path == prefix {
            return true;
        }
        if path.starts_with(prefix) {
            // Must be at segment boundary
            if prefix.ends_with('/') {
                return true;
            }
            // Next char must be '/'
            return path.chars().nth(prefix.len()) == Some('/');
        }
        false
    }

    /// Selects a backend using weighted random selection.
    fn select_backend(backends: &[Backend]) -> Backend {
        if backends.len() == 1 {
            return backends[0].clone();
        }

        // Weighted random selection
        let total_weight: u32 = backends.iter().map(|b| b.weight.max(1)).sum();
        let mut rng = rand::thread_rng();
        let mut threshold = rng.gen_range(0..total_weight);

        for backend in backends {
            let w = backend.weight.max(1);
            if threshold < w {
                return backend.clone();
            }
            threshold -= w;
        }

        backends[0].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing_api::{BackendProtocol, HttpRoute, HttpRouteMatch, HttpRouteRule, PathMatch};

    // ========== Test Helpers ==========

    fn make_backend(address: &str, weight: u32) -> Backend {
        Backend {
            address: address.to_string(),
            weight,
            protocol: BackendProtocol::Http as i32,
        }
    }

    fn make_path_match(match_type: PathMatchType, value: &str) -> PathMatch {
        PathMatch {
            r#type: match_type as i32,
            value: value.to_string(),
        }
    }

    fn make_route_match(path: Option<PathMatch>) -> HttpRouteMatch {
        HttpRouteMatch {
            path,
            headers: vec![],
            query_params: vec![],
            method: String::new(),
        }
    }

    fn make_rule(matches: Vec<HttpRouteMatch>, backends: Vec<Backend>) -> HttpRouteRule {
        HttpRouteRule {
            matches,
            backends,
            timeout_ms: 0,
            retry: None,
        }
    }

    fn make_route(id: &str, hostnames: Vec<&str>, rules: Vec<HttpRouteRule>) -> HttpRoute {
        HttpRoute {
            id: id.to_string(),
            hostnames: hostnames.into_iter().map(String::from).collect(),
            rules,
        }
    }

    fn make_route_with_backend(
        id: &str,
        hostnames: Vec<&str>,
        path: &str,
        backend_addr: &str,
    ) -> HttpRoute {
        make_route(
            id,
            hostnames,
            vec![make_rule(
                vec![make_route_match(Some(make_path_match(
                    PathMatchType::Prefix,
                    path,
                )))],
                vec![make_backend(backend_addr, 1)],
            )],
        )
    }

    fn make_route_exact_path(
        id: &str,
        hostnames: Vec<&str>,
        path: &str,
        backend_addr: &str,
    ) -> HttpRoute {
        make_route(
            id,
            hostnames,
            vec![make_rule(
                vec![make_route_match(Some(make_path_match(
                    PathMatchType::Exact,
                    path,
                )))],
                vec![make_backend(backend_addr, 1)],
            )],
        )
    }

    fn make_route_prefix_path(
        id: &str,
        hostnames: Vec<&str>,
        path: &str,
        backend_addr: &str,
    ) -> HttpRoute {
        make_route(
            id,
            hostnames,
            vec![make_rule(
                vec![make_route_match(Some(make_path_match(
                    PathMatchType::Prefix,
                    path,
                )))],
                vec![make_backend(backend_addr, 1)],
            )],
        )
    }

    fn make_route_no_path(id: &str, hostnames: Vec<&str>, backend_addr: &str) -> HttpRoute {
        make_route(
            id,
            hostnames,
            vec![make_rule(
                vec![make_route_match(None)],
                vec![make_backend(backend_addr, 1)],
            )],
        )
    }

    fn make_route_multi_backends(
        id: &str,
        hostnames: Vec<&str>,
        path: &str,
        backends: Vec<(&str, u32)>,
    ) -> HttpRoute {
        make_route(
            id,
            hostnames,
            vec![make_rule(
                vec![make_route_match(Some(make_path_match(
                    PathMatchType::Prefix,
                    path,
                )))],
                backends
                    .into_iter()
                    .map(|(addr, weight)| make_backend(addr, weight))
                    .collect(),
            )],
        )
    }

    // ========== Phase 1: Setup & Basic Matching ==========

    #[test]
    fn test_router_new_with_empty_store() {
        let store = Arc::new(RouteStore::new());
        let router = Router::new(store);
        assert!(router.find_backend("example.com", "/").is_none());
    }

    #[test]
    fn test_router_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Router>();
    }

    #[test]
    fn test_exact_hostname_match() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["example.com"],
                "/",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        let result = router.find_backend("example.com", "/");
        assert!(result.is_some());
        assert_eq!(result.unwrap().address, "backend:8080");
    }

    #[test]
    fn test_hostname_no_match_returns_none() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["example.com"],
                "/",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("other.com", "/").is_none());
    }

    #[test]
    fn test_empty_hostname_matches_all() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend("r1", vec![], "/", "catchall:8080")],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("any.host.com", "/").is_some());
        assert!(router.find_backend("another.example.org", "/").is_some());
    }

    // ========== Phase 2: Wildcard Hostnames ==========

    #[test]
    fn test_wildcard_hostname_match() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["*.example.com"],
                "/",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("foo.example.com", "/").is_some());
        assert!(router.find_backend("bar.example.com", "/").is_some());
        // Wildcard does NOT match the bare domain
        assert!(router.find_backend("example.com", "/").is_none());
        // Wildcard does NOT match multiple levels
        assert!(router.find_backend("foo.bar.example.com", "/").is_none());
    }

    #[test]
    fn test_exact_hostname_beats_wildcard() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![
                make_route_with_backend("r1", vec!["*.example.com"], "/", "wildcard:8080"),
                make_route_with_backend("r2", vec!["api.example.com"], "/", "exact:8080"),
            ],
            vec![],
            1,
        );
        let router = Router::new(store);

        // Exact match should win
        let result = router.find_backend("api.example.com", "/");
        assert_eq!(result.unwrap().address, "exact:8080");

        // Other subdomains should match wildcard
        let result = router.find_backend("other.example.com", "/");
        assert_eq!(result.unwrap().address, "wildcard:8080");
    }

    #[test]
    fn test_multiple_hostnames_in_route() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["a.com", "b.com"],
                "/",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("a.com", "/").is_some());
        assert!(router.find_backend("b.com", "/").is_some());
        assert!(router.find_backend("c.com", "/").is_none());
    }

    // ========== Phase 3: Path Matching ==========

    #[test]
    fn test_exact_path_match() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_exact_path(
                "r1",
                vec!["example.com"],
                "/api/v1",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("example.com", "/api/v1").is_some());
        assert!(router.find_backend("example.com", "/api/v1/users").is_none());
        assert!(router.find_backend("example.com", "/api").is_none());
    }

    #[test]
    fn test_prefix_path_match() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_prefix_path(
                "r1",
                vec!["example.com"],
                "/api",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("example.com", "/api").is_some());
        assert!(router.find_backend("example.com", "/api/").is_some());
        assert!(router.find_backend("example.com", "/api/v1").is_some());
        assert!(router.find_backend("example.com", "/api/v1/users").is_some());
        assert!(router.find_backend("example.com", "/other").is_none());
    }

    #[test]
    fn test_prefix_requires_segment_boundary() {
        // Gateway API spec: prefix /api should NOT match /apikeys
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_prefix_path(
                "r1",
                vec!["example.com"],
                "/api",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("example.com", "/api").is_some());
        assert!(router.find_backend("example.com", "/api/").is_some());
        // /apikeys should NOT match /api prefix (no segment boundary)
        assert!(router.find_backend("example.com", "/apikeys").is_none());
    }

    #[test]
    fn test_longer_prefix_wins() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![
                make_route_prefix_path("r1", vec!["example.com"], "/api", "short:8080"),
                make_route_prefix_path("r2", vec!["example.com"], "/api/v1", "long:8080"),
            ],
            vec![],
            1,
        );
        let router = Router::new(store);

        // Longer prefix should win
        let result = router.find_backend("example.com", "/api/v1/users");
        assert_eq!(result.unwrap().address, "long:8080");

        // Shorter prefix still works for its paths
        let result = router.find_backend("example.com", "/api/v2");
        assert_eq!(result.unwrap().address, "short:8080");
    }

    #[test]
    fn test_exact_path_beats_prefix() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![
                make_route_prefix_path("r1", vec!["example.com"], "/api", "prefix:8080"),
                make_route_exact_path("r2", vec!["example.com"], "/api/health", "exact:8080"),
            ],
            vec![],
            1,
        );
        let router = Router::new(store);

        // Exact match should win even though prefix also matches
        let result = router.find_backend("example.com", "/api/health");
        assert_eq!(result.unwrap().address, "exact:8080");

        // Other paths under /api still go to prefix route
        let result = router.find_backend("example.com", "/api/users");
        assert_eq!(result.unwrap().address, "prefix:8080");
    }

    #[test]
    fn test_no_path_match_means_match_all() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_no_path("r1", vec!["example.com"], "catchall:8080")],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("example.com", "/").is_some());
        assert!(router.find_backend("example.com", "/any/path").is_some());
        assert!(router.find_backend("example.com", "/deep/nested/path").is_some());
    }

    // ========== Phase 4: Multiple Rules & Backends ==========

    #[test]
    fn test_multiple_rules_first_match_wins() {
        let store = Arc::new(RouteStore::new());
        // Route with two rules
        store.update_routes(
            vec![make_route(
                "r1",
                vec!["example.com"],
                vec![
                    make_rule(
                        vec![make_route_match(Some(make_path_match(
                            PathMatchType::Prefix,
                            "/api",
                        )))],
                        vec![make_backend("api:8080", 1)],
                    ),
                    make_rule(
                        vec![make_route_match(Some(make_path_match(
                            PathMatchType::Prefix,
                            "/",
                        )))],
                        vec![make_backend("default:8080", 1)],
                    ),
                ],
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        // /api/users matches first rule better (longer prefix)
        let result = router.find_backend("example.com", "/api/users");
        assert_eq!(result.unwrap().address, "api:8080");

        // /other matches second rule
        let result = router.find_backend("example.com", "/other");
        assert_eq!(result.unwrap().address, "default:8080");
    }

    #[test]
    fn test_single_backend_always_selected() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["example.com"],
                "/",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        let result = router.find_backend("example.com", "/");
        assert_eq!(result.unwrap().address, "backend:8080");
    }

    #[test]
    fn test_multiple_backends_weighted_selection() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_multi_backends(
                "r1",
                vec!["example.com"],
                "/",
                vec![("backend1:8080", 80), ("backend2:8080", 20)],
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        // For MVP: just verify we get one of the backends
        let result = router.find_backend("example.com", "/");
        assert!(result.is_some());
        let addr = &result.unwrap().address;
        assert!(addr == "backend1:8080" || addr == "backend2:8080");
    }

    // ========== Phase 5: Concurrency & Edge Cases ==========

    #[test]
    fn test_concurrent_find_backend() {
        use std::thread;

        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["example.com"],
                "/",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Arc::new(Router::new(store));

        let mut handles = vec![];
        for _ in 0..10 {
            let router = Arc::clone(&router);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = router.find_backend("example.com", "/api/test");
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_route_update_while_routing() {
        use std::thread;
        use std::time::Duration;

        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["example.com"],
                "/",
                "v1:8080",
            )],
            vec![],
            1,
        );
        let router = Arc::new(Router::new(Arc::clone(&store)));

        // Start routing in background
        let router_clone = Arc::clone(&router);
        let handle = thread::spawn(move || {
            for _ in 0..1000 {
                let _ = router_clone.find_backend("example.com", "/");
                thread::sleep(Duration::from_micros(10));
            }
        });

        // Update routes while routing is happening
        for i in 2..10 {
            store.update_routes(
                vec![make_route_with_backend(
                    "r1",
                    vec!["example.com"],
                    "/",
                    &format!("v{}:8080", i),
                )],
                vec![],
                i,
            );
            thread::sleep(Duration::from_millis(1));
        }

        handle.join().unwrap();
        // No panics = success
    }

    #[test]
    fn test_host_with_port() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["example.com"],
                "/",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        // Host header might include port - should still match
        assert!(router.find_backend("example.com:443", "/").is_some());
        assert!(router.find_backend("example.com:8080", "/").is_some());
    }

    #[test]
    fn test_case_insensitive_hostname() {
        let store = Arc::new(RouteStore::new());
        store.update_routes(
            vec![make_route_with_backend(
                "r1",
                vec!["Example.COM"],
                "/",
                "backend:8080",
            )],
            vec![],
            1,
        );
        let router = Router::new(store);

        assert!(router.find_backend("example.com", "/").is_some());
        assert!(router.find_backend("EXAMPLE.COM", "/").is_some());
        assert!(router.find_backend("Example.Com", "/").is_some());
    }
}
