//! REST API handlers for the control plane.

mod artifacts;
mod compilations;
mod health;
mod init;
mod plugins;
mod router;
mod specs;

pub use router::create_router;
