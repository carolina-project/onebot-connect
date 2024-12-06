use std::io;

#[cfg(feature = "app")]
pub mod app;
#[cfg(feature = "imp")]
pub mod imp;

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
}
