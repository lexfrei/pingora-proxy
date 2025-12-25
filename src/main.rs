//! Pingora-based reverse proxy with gRPC API for dynamic route updates.
//!
//! This proxy receives routing configuration from the Kubernetes controller
//! via gRPC and dynamically updates its routing table without restart.

mod gen;
mod grpc;
mod store;

use std::sync::Arc;

use anyhow::Result;
use tonic::transport::Server;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::grpc::RoutingServiceImpl;
use crate::routing_api::routing_service_server::RoutingServiceServer;
use crate::store::RouteStore;

// Re-export generated types for convenience
pub use gen::routing::v1 as routing_api;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("pingora-proxy starting");

    // Create shared route store
    let store = Arc::new(RouteStore::new());

    // Create gRPC service
    let routing_service = RoutingServiceImpl::new(store.clone());

    // Start gRPC server
    let addr = "[::]:50051".parse()?;
    tracing::info!(%addr, "gRPC server listening");

    Server::builder()
        .add_service(RoutingServiceServer::new(routing_service))
        .serve(addr)
        .await?;

    Ok(())
}
