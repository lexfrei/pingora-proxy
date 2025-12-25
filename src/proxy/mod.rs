//! HTTP request routing.
//!
//! Provides routing logic for matching incoming HTTP requests
//! to appropriate backends based on hostname and path.

mod router;

pub use router::Router;
