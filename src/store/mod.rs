//! Route storage for Pingora proxy.
//!
//! Provides thread-safe storage for HTTP and gRPC routes received
//! from the Kubernetes controller via gRPC.

mod route_store;

pub use route_store::RouteStore;
