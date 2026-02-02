//! WebSocket module for data plane connections.

pub mod handler;
pub mod manager;
pub mod protocol;

pub use handler::ws_handler;
pub use manager::ConnectionManager;
