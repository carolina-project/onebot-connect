use std::fmt::{Debug, Display};

use onebot_types::ob12::action::{RespData, RespError, RespStatus, RetCode};
use serde::{Deserialize, Serialize};
use serde_value::{DeserializerError, SerializerError, Value};

pub use onebot_types as types;
pub use serde_value as value;

#[cfg(feature = "app")]
pub mod app;
#[cfg(feature = "imp")]
pub mod imp;
#[cfg(feature = "upload")]
pub mod upload;

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
    #[cfg(feature = "compat")]
    #[error(transparent)]
    Compat(#[from] onebot_types::compat::CompatError),
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
    #[cfg(feature = "upload")]
    #[error("upload error: {0}")]
    Upload(#[from] crate::upload::UploadError),
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

pub type AllResult<T> = Result<T, Error>;

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

impl From<SerializerError> for Error {
    fn from(value: SerializerError) -> Self {
        Self::serialize(value)
    }
}

impl From<DeserializerError> for Error {
    fn from(value: DeserializerError) -> Self {
        Self::deserialize(value)
    }
}

#[doc(hidden)]
#[cfg(feature = "tokio")]
mod tokio_impl {
    use super::*;

    impl<T> From<tokio::sync::mpsc::error::SendError<T>> for Error {
        fn from(e: tokio::sync::mpsc::error::SendError<T>) -> Self {
            Self::closed(e)
        }
    }
    impl<T> From<tokio::sync::broadcast::error::SendError<T>> for Error {
        fn from(e: tokio::sync::broadcast::error::SendError<T>) -> Self {
            Self::closed(e)
        }
    }
}

/// Response args passed to connection.
#[derive(Debug, Clone)]
pub struct RespArgs {
    pub status: RespStatus,
    pub retcode: RetCode,
    pub data: Value,
    pub message: String,
}

impl From<RespData> for RespArgs {
    fn from(value: RespData) -> Self {
        let RespData {
            status,
            retcode,
            data,
            message,
            ..
        } = value;
        Self {
            status,
            retcode,
            data,
            message,
        }
    }
}

impl RespArgs {
    pub fn failed(retcode: RetCode, msg: impl Into<String>) -> Self {
        Self {
            status: RespStatus::Failed,
            retcode,
            data: Value::Option(None),
            message: msg.into(),
        }
    }

    pub fn success<T: Serialize>(data: T) -> Result<Self, serde_value::SerializerError> {
        Ok(Self {
            status: RespStatus::Ok,
            retcode: RetCode::Success,
            data: serde_value::to_value(data)?,
            message: Default::default(),
        })
    }

    pub fn into_resp_data(self, echo: Option<String>) -> RespData {
        let Self {
            status,
            retcode,
            data,
            message,
        } = self;
        RespData {
            status,
            retcode,
            data,
            message,
            echo,
        }
    }

    pub fn into_result(self, echo: Option<String>) -> Result<Value, RespError> {
        let RespArgs {
            status,
            retcode,
            data,
            message,
        } = self;
        if status == RespStatus::Failed {
            Err(RespError {
                retcode,
                message,
                echo,
            })
        } else {
            Ok(data)
        }
    }
}
