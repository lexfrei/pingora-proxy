//! Pingora-based reverse proxy with gRPC API for dynamic route updates.
//!
//! This proxy receives routing configuration from the Kubernetes controller
//! via gRPC and dynamically updates its routing table without restart.

mod gen;

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Re-export generated types for convenience
pub use gen::routing::v1 as routing_api;

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("pingora-proxy starting");

    // TODO: Initialize Pingora server
    // TODO: Start gRPC server for route updates
    // TODO: Start HTTP proxy

    Ok(())
}
