use onebot_types::ob12::action::RespError;
use std::error::Error as ErrTrait;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "client")]
pub mod client;

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

    pub fn not_supported(msg: impl AsRef<str>) -> Self {
        Self::NotSupported(msg.as_ref().into())
    }

    pub fn closed<E: ErrTrait + Send + 'static>(e: E) -> Self {
        Self::Closed(Box::new(e))
    }
}
