//! Pingora ProxyHttp implementation for the gateway.
//!
//! Integrates Router and HealthTracker with Pingora's ProxyHttp trait
//! to route incoming HTTP requests to appropriate backends.

use std::net::SocketAddr;

use async_trait::async_trait;
use pingora_core::prelude::*;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_proxy::{ProxyHttp, Session};

use crate::routing_api::{Backend, BackendProtocol};

use super::Router;

/// Per-request context for the gateway proxy.
///
/// Stores state that needs to persist across the request lifecycle,
/// such as the selected backend address for health tracking.
#[derive(Default)]
pub struct GatewayCtx {
    /// The backend address selected for this request.
    /// Used for health tracking in connected_to_upstream/fail_to_connect.
    pub backend_address: Option<String>,
}

/// Gateway proxy that routes HTTP requests to backends.
///
/// Implements Pingora's ProxyHttp trait to integrate with the Pingora
/// HTTP proxy framework. Uses Router to find backends and tracks
/// backend health via HealthTracker.
pub struct GatewayProxy {
    router: Router,
}

impl GatewayProxy {
    /// Creates a new GatewayProxy with the given router.
    pub fn new(router: Router) -> Self {
        Self { router }
    }

    /// Creates an HttpPeer from a backend.
    ///
    /// Parses the address, determines TLS requirements, and extracts SNI.
    fn backend_to_peer(backend: &Backend) -> Result<HttpPeer> {
        let addr = parse_backend_address(&backend.address)
            .map_err(|e| Error::explain(ErrorType::InternalError, e))?;

        let protocol = BackendProtocol::try_from(backend.protocol)
            .unwrap_or(BackendProtocol::Http);
        let use_tls = requires_tls(protocol);
        let sni = extract_sni(&backend.address);

        Ok(HttpPeer::new(addr, use_tls, sni))
    }
}

#[async_trait]
impl ProxyHttp for GatewayProxy {
    type CTX = GatewayCtx;

    fn new_ctx(&self) -> Self::CTX {
        GatewayCtx::default()
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        // Extract host from headers
        let host_header = session
            .req_header()
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok());
        let authority = session
            .req_header()
            .uri
            .authority()
            .map(|a| a.as_str());

        let host = extract_host_for_routing(host_header, authority);
        let path = session.req_header().uri.path();

        // Find backend via router
        let backend = self.router.find_backend(&host, path).ok_or_else(|| {
            Error::explain(
                ErrorType::HTTPStatus(404),
                format!("no route for host={} path={}", host, path),
            )
        })?;

        // Store for health tracking
        ctx.backend_address = Some(backend.address.clone());

        // Create HttpPeer
        let peer = Self::backend_to_peer(&backend)?;
        Ok(Box::new(peer))
    }

    async fn connected_to_upstream(
        &self,
        _session: &mut Session,
        _reused: bool,
        _peer: &HttpPeer,
        _fd: std::os::unix::io::RawFd,
        _digest: Option<&pingora_core::protocols::Digest>,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Record successful connection for health tracking
        if let Some(ref addr) = ctx.backend_address {
            self.router.health_tracker().record_success(addr);
        }
        Ok(())
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        e: Box<Error>,
    ) -> Box<Error> {
        // Record failed connection for health tracking
        if let Some(ref addr) = ctx.backend_address {
            self.router.health_tracker().record_failure(addr);
        }
        e
    }

    async fn logging(
        &self,
        session: &mut Session,
        _e: Option<&Error>,
        ctx: &mut Self::CTX,
    ) {
        let status = session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);

        let method = session.req_header().method.as_str();
        let path = session.req_header().uri.path();
        let backend = ctx.backend_address.as_deref().unwrap_or("-");

        tracing::info!(
            method = method,
            path = path,
            status = status,
            backend = backend,
            "request completed"
        );
    }
}

/// Parses a backend address string into a SocketAddr.
///
/// Expects format "IP:PORT" (e.g., "192.168.1.1:8080" or "[::1]:8080").
/// Returns an error if the address is invalid or missing a port.
pub fn parse_backend_address(address: &str) -> Result<SocketAddr, String> {
    address
        .parse::<SocketAddr>()
        .map_err(|e| format!("invalid backend address '{}': {}", address, e))
}

/// Determines if a backend protocol requires TLS.
///
/// HTTP and H2C (cleartext HTTP/2) do not require TLS.
/// HTTPS and H2 (HTTP/2 over TLS) require TLS.
pub fn requires_tls(protocol: BackendProtocol) -> bool {
    matches!(protocol, BackendProtocol::Https | BackendProtocol::H2)
}

/// Extracts the SNI (Server Name Indication) from a backend address.
///
/// For "host:port" returns "host".
/// For "[ipv6]:port" returns "ipv6" (without brackets).
/// For IP addresses, returns the IP as string.
pub fn extract_sni(address: &str) -> String {
    // Handle IPv6 addresses: [::1]:8080 -> ::1
    if address.starts_with('[') {
        if let Some(end) = address.find(']') {
            return address[1..end].to_string();
        }
    }

    // Handle regular host:port -> host
    address
        .rsplit_once(':')
        .map(|(host, _)| host.to_string())
        .unwrap_or_else(|| address.to_string())
}

/// Extracts the host for routing from request headers.
///
/// Priority:
/// 1. Host header (preferred)
/// 2. :authority pseudo-header (HTTP/2 fallback)
///
/// Port is stripped if present.
/// Returns empty string if neither is available.
pub fn extract_host_for_routing(host_header: Option<&str>, authority: Option<&str>) -> String {
    let raw_host = host_header.or(authority).unwrap_or("");

    // Strip port if present
    // Handle IPv6: [::1]:8080 -> [::1]
    if raw_host.starts_with('[') {
        if let Some(bracket_end) = raw_host.find(']') {
            return raw_host[..=bracket_end].to_string();
        }
    }

    // Regular host:port -> host
    raw_host
        .rsplit_once(':')
        .map(|(host, _)| host.to_string())
        .unwrap_or_else(|| raw_host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Phase 1: Address Parsing (3 tests) ==========

    #[test]
    fn test_parse_address_valid_ipv4_with_port() {
        let result = parse_backend_address("192.168.1.1:8080");
        assert!(result.is_ok());
        let addr = result.unwrap();
        assert_eq!(addr.ip().to_string(), "192.168.1.1");
        assert_eq!(addr.port(), 8080);
    }

    #[test]
    fn test_parse_address_missing_port() {
        let result = parse_backend_address("192.168.1.1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid backend address"));
    }

    #[test]
    fn test_parse_address_ipv6_with_port() {
        let result = parse_backend_address("[::1]:8080");
        assert!(result.is_ok());
        let addr = result.unwrap();
        assert!(addr.ip().is_ipv6());
        assert_eq!(addr.port(), 8080);
    }

    // ========== Phase 1: TLS Requirements (3 tests) ==========

    #[test]
    fn test_requires_tls_http_false() {
        assert!(!requires_tls(BackendProtocol::Http));
    }

    #[test]
    fn test_requires_tls_https_true() {
        assert!(requires_tls(BackendProtocol::Https));
    }

    #[test]
    fn test_requires_tls_h2c_false() {
        assert!(!requires_tls(BackendProtocol::H2c));
    }

    #[test]
    fn test_requires_tls_h2_true() {
        assert!(requires_tls(BackendProtocol::H2));
    }

    // ========== Phase 2: SNI Extraction (3 tests) ==========

    #[test]
    fn test_extract_sni_from_host_port() {
        assert_eq!(extract_sni("backend.example.com:8443"), "backend.example.com");
    }

    #[test]
    fn test_extract_sni_from_ip_port() {
        assert_eq!(extract_sni("1.2.3.4:8443"), "1.2.3.4");
    }

    #[test]
    fn test_extract_sni_ipv6() {
        assert_eq!(extract_sni("[::1]:8443"), "::1");
    }

    // ========== Phase 3: Host Extraction (5 tests) ==========

    #[test]
    fn test_extract_host_from_host_header() {
        let result = extract_host_for_routing(Some("example.com"), None);
        assert_eq!(result, "example.com");
    }

    #[test]
    fn test_extract_host_from_authority() {
        let result = extract_host_for_routing(None, Some("example.com"));
        assert_eq!(result, "example.com");
    }

    #[test]
    fn test_extract_host_prefers_host_header() {
        let result = extract_host_for_routing(Some("host.com"), Some("authority.com"));
        assert_eq!(result, "host.com");
    }

    #[test]
    fn test_extract_host_strips_port() {
        let result = extract_host_for_routing(Some("example.com:8080"), None);
        assert_eq!(result, "example.com");
    }

    #[test]
    fn test_extract_host_missing_returns_empty() {
        let result = extract_host_for_routing(None, None);
        assert_eq!(result, "");
    }

    // ========== Phase 4: GatewayProxy Struct (3 tests) ==========

    use super::super::upstream::HealthTracker;
    use super::super::Router;
    use crate::store::RouteStore;
    use std::sync::Arc;

    fn make_test_router() -> Router {
        let store = Arc::new(RouteStore::new());
        let health_tracker = Arc::new(HealthTracker::new(3));
        Router::new(store, health_tracker)
    }

    #[test]
    fn test_gateway_proxy_new() {
        let router = make_test_router();
        let _proxy = GatewayProxy::new(router);
        // If we get here, construction succeeded
    }

    #[test]
    fn test_gateway_proxy_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<GatewayProxy>();
    }

    #[test]
    fn test_gateway_ctx_default() {
        let ctx = GatewayCtx::default();
        assert!(ctx.backend_address.is_none());
    }

    // ========== Phase 5: Backend to HttpPeer Conversion (3 tests) ==========

    fn make_backend(address: &str, protocol: BackendProtocol) -> Backend {
        Backend {
            address: address.to_string(),
            weight: 1,
            protocol: protocol as i32,
        }
    }

    #[test]
    fn test_backend_to_peer_http() {
        let backend = make_backend("192.168.1.1:8080", BackendProtocol::Http);
        let peer = GatewayProxy::backend_to_peer(&backend).unwrap();
        assert!(!peer.is_tls());
    }

    #[test]
    fn test_backend_to_peer_https() {
        let backend = make_backend("192.168.1.1:8443", BackendProtocol::Https);
        let peer = GatewayProxy::backend_to_peer(&backend).unwrap();
        assert!(peer.is_tls());
    }

    #[test]
    fn test_backend_to_peer_invalid_address() {
        let backend = make_backend("not-an-address", BackendProtocol::Http);
        let result = GatewayProxy::backend_to_peer(&backend);
        assert!(result.is_err());
    }
}
