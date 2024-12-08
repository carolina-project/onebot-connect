use std::io;

#[cfg(feature = "app")]
pub mod app;
#[cfg(feature = "imp")]
pub mod imp;

pub mod common;

type Authorization = Option<(String, String)>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    OneBotConnect(#[from] onebot_connect_interface::Error),
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
    Desc(String),
}

impl Error {
    pub fn desc(msg: impl Into<String>) -> Self {
        Self::Desc(msg.into())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum RecvError {
    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),
    #[error("invalid data type: {0}")]
    InvalidData(String),
}

impl RecvError {
    pub fn invalid_data(msg: impl Into<String>) -> RecvError {
        Self::InvalidData(msg.into())
    }

}
