//! gRPC server implementation for route management.
//!
//! Provides the RoutingService gRPC server that receives route
//! configuration from the Kubernetes controller.

mod routing_service;

pub use routing_service::RoutingServiceImpl;
