//! HTTP request routing and load balancing.
//!
//! Provides routing logic for matching incoming HTTP requests
//! to appropriate backends based on hostname and path,
//! with weighted round-robin load balancing and health tracking.

mod gateway;
mod router;
mod upstream;

pub use gateway::GatewayProxy;
pub use router::Router;
pub use upstream::HealthTracker;
