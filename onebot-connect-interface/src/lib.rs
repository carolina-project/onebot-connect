use std::fmt::{Debug, Display};

use onebot_types::ob12::action::RespError;
use serde::{Deserialize, Serialize};

#[cfg(feature = "imp")]
pub mod imp;

#[cfg(feature = "app")]
pub mod app;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ClosedReason {
    /// Closed successfully.
    Ok,
    /// Closed, but some error occurred.
    Error(String),
    /// Partially closed.
    Partial(String),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Resp(#[from] RespError),
    #[error("serialize error: {0}")]
    Serialize(String),
    #[error("deserialize error: {0}")]
    Deserialize(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("not supported: {0}")]
    NotSupported(String),
    #[error("closed: {0}")]
    Closed(String),
    #[error("missing {0}")]
    Missing(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("expected type `{0}`, found type `{1}`")]
    TypeMismatch(String, String),
    #[error("unknown config key `{0}`")]
    UnknownKey(String),
    #[error("{0}")]
    Other(String),
}

pub type ActionResult<T> = Result<T, Error>;

impl Error {
    pub fn other<T: Display>(e: T) -> Self {
        Self::Other(e.to_string())
    }

    pub fn serialize<E: Display>(e: E) -> Self {
        Self::Serialize(e.to_string())
    }

    pub fn deserialize<E: Display>(e: E) -> Self {
        Self::Deserialize(e.to_string())
    }

    pub fn not_supported<E: Display>(e: E) -> Self {
        Self::NotSupported(e.to_string())
    }

    pub fn closed<E: Display>(e: E) -> Self {
        Self::Closed(e.to_string())
    }

    pub fn missing<E: Display>(e: E) -> Self {
        Self::Missing(e.to_string())
    }
}
