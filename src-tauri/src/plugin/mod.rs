mod manager;
mod package;
mod runtime;
mod state;
mod types;

use std::path::PathBuf;

use thiserror::Error;

pub use manager::PluginManager;
pub use types::*;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin input is invalid: {0}")]
    Invalid(String),
    #[error("plugin state is invalid: {0}")]
    InvalidState(String),
    #[error("plugin registry lock is unavailable")]
    LockUnavailable,
    #[error("plugin or registry was not found")]
    NotFound,
    #[error("plugin or registry changed; refresh and try again")]
    Conflict,
    #[error("plugin download failed")]
    Network(#[source] reqwest::Error),
    #[error("plugin package verification failed: {0}")]
    Verification(String),
    #[error("plugin component could not be loaded or invoked")]
    Runtime,
    #[error("plugin I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("plugin state serialization failed")]
    Serialize(#[source] serde_json::Error),
}

impl PluginError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_plugin_input",
            Self::InvalidState(_) => "invalid_plugin_state",
            Self::LockUnavailable => "lock_unavailable",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Network(_) => "plugin_network_error",
            Self::Verification(_) => "plugin_verification_failed",
            Self::Runtime => "plugin_runtime_error",
            Self::Io { .. } | Self::Serialize(_) => "plugin_storage_error",
        }
    }
}

fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> PluginError {
    PluginError::Io {
        path: path.into(),
        source,
    }
}
