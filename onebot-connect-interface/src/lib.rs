use onebot_types::ob12::action::RespError;
use serde_value::DeserializerError;
use std::error::Error as ErrTrait;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "client")]
pub mod client;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Resp(#[from] RespError),
    #[error(transparent)]
    Deserializer(#[from] DeserializerError),
    #[error(transparent)]
    Other(Box<dyn ErrTrait + Send>),
}

pub type ActionResult<T> = Result<T, Error>;

impl Error {
    pub fn other<T: ErrTrait + Send + 'static>(err: T) -> Self {
        Self::Other(Box::new(err))
    }
}
