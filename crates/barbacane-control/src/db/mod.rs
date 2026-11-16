//! Database layer for the control plane.

mod api_keys;
mod artifacts;
mod compilations;
mod data_planes;
mod models;
mod plugins;
mod pool;
mod project_plugins;
mod projects;
mod specs;

/// A window of a list: at most `limit` rows, after skipping `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub limit: i64,
    pub offset: i64,
}

impl Page {
    /// Every row.
    pub const ALL: Page = Page {
        limit: i64::MAX,
        offset: 0,
    };
}

pub use api_keys::ApiKeysRepository;
pub use artifacts::ArtifactsRepository;
pub use compilations::CompilationsRepository;
pub use data_planes::DataPlanesRepository;
pub use models::*;
pub use plugins::PluginsRepository;
pub use pool::{create_pool, run_migrations};
pub use project_plugins::ProjectPluginConfigsRepository;
pub use projects::ProjectsRepository;
pub use specs::SpecsRepository;
