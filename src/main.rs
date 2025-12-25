//! Pingora-based reverse proxy with gRPC API for dynamic route updates.
//!
//! This proxy receives routing configuration from the Kubernetes controller
//! via gRPC and dynamically updates its routing table without restart.

mod gen;
mod grpc;
mod proxy;
mod store;

use std::sync::Arc;

use anyhow::Result;
use pingora_core::server::Server;
use pingora_core::server::configuration::Opt;
use pingora_proxy::http_proxy_service;
use tonic::transport::Server as TonicServer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::grpc::RoutingServiceImpl;
use crate::proxy::{GatewayProxy, HealthTracker, Router};
use crate::routing_api::routing_service_server::RoutingServiceServer;
use crate::store::RouteStore;

// Re-export generated types for convenience
pub use gen::routing::v1 as routing_api;

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("pingora-proxy starting");

    // Create shared components
    let store = Arc::new(RouteStore::new());
    let health_tracker = Arc::new(HealthTracker::new(3));
    let router = Router::new(store.clone(), health_tracker);

    // Spawn gRPC server in background
    let grpc_store = store.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        rt.block_on(async {
            let routing_service = RoutingServiceImpl::new(grpc_store);
            let addr = "[::]:50051".parse().expect("invalid gRPC address");
            tracing::info!(%addr, "gRPC server listening");

            if let Err(e) = TonicServer::builder()
                .add_service(RoutingServiceServer::new(routing_service))
                .serve(addr)
                .await
            {
                tracing::error!(error = %e, "gRPC server error");
            }
        });
    });

    // Create Pingora server
    let opt = Opt::default();
    let mut server = Server::new(Some(opt))?;
    server.bootstrap();

    // Create and add HTTP proxy service
    let gateway = GatewayProxy::new(router);
    let mut proxy_service = http_proxy_service(&server.configuration, gateway);
    proxy_service.add_tcp("0.0.0.0:8080");

    tracing::info!(addr = "0.0.0.0:8080", "HTTP proxy listening");

    server.add_service(proxy_service);
    server.run_forever();
}
