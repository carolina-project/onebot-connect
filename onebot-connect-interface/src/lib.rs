use std::error::Error as ErrTrait;

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
    Serialize(Box<dyn ErrTrait + Send>),
    #[error("deserialize error: {0}")]
    Deserialize(Box<dyn ErrTrait + Send>),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("not supported: {0}")]
    NotSupported(String),
    #[error("closed: {0}")]
    Closed(Box<dyn ErrTrait + Send>),
    #[error(transparent)]
    Other(Box<dyn ErrTrait + Send>),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("expected type `{0}`, found type `{1}`")]
    TypeMismatch(String, String),
    #[error("unknown config key `{0}`")]
    UnknownKey(String),
    #[error(transparent)]
    Other(Box<dyn ErrTrait + Send>),
}

pub type ActionResult<T> = Result<T, Error>;

impl Error {
    pub fn other<T: ErrTrait + Send + 'static>(err: T) -> Self {
        Self::Other(Box::new(err))
    }

    pub fn serialize<E: serde::ser::Error + Send + 'static>(e: E) -> Self {
        Self::Serialize(Box::new(e))
    }

    pub fn deserialize<E: serde::de::Error + Send + 'static>(e: E) -> Self {
        Self::Deserialize(Box::new(e))
    }

    pub fn not_supported(msg: impl Into<String>) -> Self {
        Self::NotSupported(msg.into())
    }

    pub fn closed<E: ErrTrait + Send + 'static>(e: E) -> Self {
        Self::Closed(Box::new(e))
    }
}
