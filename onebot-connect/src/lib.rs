use std::{fmt::Display, io};

use onebot_connect_interface::Error as OCError;
use onebot_types::ob12::action::RespError;
use serde_value::{DeserializerError, SerializerError};
use tokio::sync::mpsc;

pub mod ob12 {
    pub use onebot_types::ob12::*;
}

#[cfg(feature = "compat")]
pub mod ob11 {
    pub use onebot_types::ob11::*;
}

pub use onebot_types::{base as types_base, select_msg};

#[cfg(feature = "app")]
pub mod app;
#[cfg(feature = "imp")]
pub mod imp;

pub mod common;
pub mod wrap;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    OneBotConnect(#[from] OCError),
    #[cfg(any(feature = "ws", feature = "http"))]
    #[error(transparent)]
    HeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("ws closed")]
    ConnectionClosed,
    #[error("communication channel closed")]
    ChannelClosed,
    #[cfg(feature = "ws")]
    #[error(transparent)]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[cfg(feature = "http")]
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other<T: Display>(e: T) -> Self {
        Self::Other(e.to_string())
    }
}

impl From<Error> for OCError {
    fn from(e: Error) -> Self {
        match e {
            Error::OneBotConnect(e) => e,
            other => Self::other(other),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum RecvError {
    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),
    #[error("invalid data type: {0}")]
    InvalidData(String),
}

impl From<DeserializerError> for Error {
    fn from(e: DeserializerError) -> Self {
        Self::OneBotConnect(OCError::deserialize(e))
    }
}

impl From<RespError> for Error {
    fn from(err: RespError) -> Self {
        Self::OneBotConnect(err.into())
    }
}

#[cfg(feature = "storage")]
impl From<onebot_connect_interface::upload::UploadError> for Error {
    fn from(err: onebot_connect_interface::upload::UploadError) -> Self {
        Self::OneBotConnect(err.into())
    }
}

#[cfg(feature = "compat")]
impl From<onebot_types::compat::CompatError> for Error {
    fn from(err: onebot_types::compat::CompatError) -> Self {
        Self::OneBotConnect(err.into())
    }
}

impl From<SerializerError> for Error {
    fn from(e: SerializerError) -> Self {
        Self::OneBotConnect(OCError::serialize(e))
    }
}

impl<T> From<mpsc::error::SendError<T>> for Error {
    fn from(value: mpsc::error::SendError<T>) -> Self {
        Self::OneBotConnect(value.into())
    }
}

impl RecvError {
    pub fn invalid_data(msg: impl Into<String>) -> RecvError {
        Self::InvalidData(msg.into())
    }
}
