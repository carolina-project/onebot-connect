use std::io;

#[cfg(feature = "client")]
pub mod client;

use std::error::Error as ErrTrait;

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
    ChannelClosed(Box<dyn ErrTrait + Send>),
    #[cfg(feature = "ws")]
    #[error(transparent)]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[cfg(feature = "http")]
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
}

impl Error {
    pub fn channel_closed<E: ErrTrait + Send + 'static>(e: E) -> Self {
        Self::ChannelClosed(Box::new(e))
    }
}
