//! Portable application and protocol core for Leafport.

/// Leafport's version, the workspace version in Cargo.toml. Reported to the service in the
/// activate and fulfill requests and in the User-Agent.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use rsa;

pub mod activation;
pub mod cloud;
pub mod drm;
pub mod fulfillment;
pub mod infoset;
pub mod library;
pub mod pdf_drm;
pub mod service;
pub mod signing;
pub mod store;
pub mod vendors;
