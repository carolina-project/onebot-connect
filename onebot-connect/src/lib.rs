use std::{fmt::Display, io};

use common::UploadError;
use onebot_connect_interface::Error as OCError;
use serde_value::{DeserializerError, SerializerError};

#[cfg(feature = "app")]
pub mod app;
#[cfg(feature = "imp")]
pub mod imp;

pub mod common;

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
    #[error(transparent)]
    Upload(#[from] UploadError),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other<T: Display>(e: T) -> Self {
        Self::Other(e.to_string())
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

impl From<SerializerError> for Error {
    fn from(e: SerializerError) -> Self {
        Self::OneBotConnect(OCError::serialize(e))
    }
}

impl RecvError {
    pub fn invalid_data(msg: impl Into<String>) -> RecvError {
        Self::InvalidData(msg.into())
    }
}
