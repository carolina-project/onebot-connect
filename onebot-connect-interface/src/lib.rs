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
    #[error(transparent)]
    Other(#[from] Box<dyn ErrTrait>),
}
