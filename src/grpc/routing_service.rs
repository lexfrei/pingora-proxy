//! gRPC RoutingService implementation.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::routing_api::routing_service_server::RoutingService;
use crate::routing_api::{
    GetRoutesRequest, GetRoutesResponse, HealthRequest, HealthResponse, UpdateRoutesRequest,
    UpdateRoutesResponse,
};
use crate::store::RouteStore;

/// gRPC service implementation for route management.
pub struct RoutingServiceImpl {
    store: Arc<RouteStore>,
}

impl RoutingServiceImpl {
    /// Creates a new RoutingServiceImpl with the given route store.
    pub fn new(store: Arc<RouteStore>) -> Self {
        Self { store }
    }
}

#[tonic::async_trait]
impl RoutingService for RoutingServiceImpl {
    /// Updates all routes with a full sync.
    async fn update_routes(
        &self,
        request: Request<UpdateRoutesRequest>,
    ) -> Result<Response<UpdateRoutesResponse>, Status> {
        let req = request.into_inner();

        let http_count = req.http_routes.len() as u32;
        let grpc_count = req.grpc_routes.len() as u32;

        let applied_version = self
            .store
            .update_routes(req.http_routes, req.grpc_routes, req.version);

        tracing::info!(
            version = applied_version,
            http_routes = http_count,
            grpc_routes = grpc_count,
            "Routes updated"
        );

        Ok(Response::new(UpdateRoutesResponse {
            success: true,
            error: String::new(),
            applied_version,
            http_route_count: http_count,
            grpc_route_count: grpc_count,
        }))
    }

    /// Returns all currently configured routes.
    async fn get_routes(
        &self,
        _request: Request<GetRoutesRequest>,
    ) -> Result<Response<GetRoutesResponse>, Status> {
        let http_routes = self.store.get_http_routes();
        let grpc_routes = self.store.get_grpc_routes();
        let version = self.store.version();

        Ok(Response::new(GetRoutesResponse {
            http_routes,
            grpc_routes,
            version,
        }))
    }

    /// Returns the health status of the proxy.
    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let version = self.store.version();

        Ok(Response::new(HealthResponse {
            healthy: true,
            status: "ready".to_string(),
            active_connections: 0, // TODO: Track actual connections
            config_version: version,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing_api::HttpRoute;

    fn make_http_route(id: &str, hostnames: Vec<&str>) -> HttpRoute {
        HttpRoute {
            id: id.to_string(),
            hostnames: hostnames.into_iter().map(String::from).collect(),
            rules: vec![],
        }
    }

    #[tokio::test]
    async fn test_health_returns_healthy() {
        let store = Arc::new(RouteStore::new());
        let service = RoutingServiceImpl::new(store);

        let request = Request::new(HealthRequest {});
        let response = service.health(request).await.unwrap();

        let health = response.into_inner();
        assert!(health.healthy);
        assert_eq!(health.status, "ready");
        assert_eq!(health.config_version, 0);
    }

    #[tokio::test]
    async fn test_update_routes_empty() {
        let store = Arc::new(RouteStore::new());
        let service = RoutingServiceImpl::new(store);

        let request = Request::new(UpdateRoutesRequest {
            http_routes: vec![],
            grpc_routes: vec![],
            version: 1,
        });

        let response = service.update_routes(request).await.unwrap();
        let update = response.into_inner();

        assert!(update.success);
        assert_eq!(update.applied_version, 1);
        assert_eq!(update.http_route_count, 0);
        assert_eq!(update.grpc_route_count, 0);
    }

    #[tokio::test]
    async fn test_update_routes_with_data() {
        let store = Arc::new(RouteStore::new());
        let service = RoutingServiceImpl::new(store);

        let request = Request::new(UpdateRoutesRequest {
            http_routes: vec![
                make_http_route("default/route1", vec!["a.example.com"]),
                make_http_route("default/route2", vec!["b.example.com"]),
            ],
            grpc_routes: vec![],
            version: 5,
        });

        let response = service.update_routes(request).await.unwrap();
        let update = response.into_inner();

        assert!(update.success);
        assert_eq!(update.applied_version, 5);
        assert_eq!(update.http_route_count, 2);
        assert_eq!(update.grpc_route_count, 0);
    }

    #[tokio::test]
    async fn test_get_routes_after_update() {
        let store = Arc::new(RouteStore::new());
        let service = RoutingServiceImpl::new(store);

        // First update routes
        let update_request = Request::new(UpdateRoutesRequest {
            http_routes: vec![make_http_route("default/route1", vec!["example.com"])],
            grpc_routes: vec![],
            version: 3,
        });
        service.update_routes(update_request).await.unwrap();

        // Then get routes
        let get_request = Request::new(GetRoutesRequest {});
        let response = service.get_routes(get_request).await.unwrap();
        let routes = response.into_inner();

        assert_eq!(routes.http_routes.len(), 1);
        assert_eq!(routes.http_routes[0].id, "default/route1");
        assert_eq!(routes.version, 3);
    }

    #[tokio::test]
    async fn test_version_increments() {
        let store = Arc::new(RouteStore::new());
        let service = RoutingServiceImpl::new(store.clone());

        // Check initial version
        let health1 = service
            .health(Request::new(HealthRequest {}))
            .await
            .unwrap();
        assert_eq!(health1.into_inner().config_version, 0);

        // Update to version 10
        let update_request = Request::new(UpdateRoutesRequest {
            http_routes: vec![],
            grpc_routes: vec![],
            version: 10,
        });
        service.update_routes(update_request).await.unwrap();

        // Check version updated
        let health2 = service
            .health(Request::new(HealthRequest {}))
            .await
            .unwrap();
        assert_eq!(health2.into_inner().config_version, 10);
    }

    #[tokio::test]
    async fn test_routes_shared_with_store() {
        let store = Arc::new(RouteStore::new());
        let service = RoutingServiceImpl::new(store.clone());

        // Update via service
        let request = Request::new(UpdateRoutesRequest {
            http_routes: vec![make_http_route("default/route1", vec!["example.com"])],
            grpc_routes: vec![],
            version: 1,
        });
        service.update_routes(request).await.unwrap();

        // Verify store is updated
        assert_eq!(store.version(), 1);
        assert_eq!(store.route_count(), (1, 0));
    }
}
