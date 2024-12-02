use std::{error::Error as ErrTrait, future::Future};

use super::Error;
use onebot_types::{base::OBAction, ob12::event::Event};

#[cfg(feature = "client_recv")]
mod recv {
    use serde::Deserializer;

    pub use super::*;

    pub struct ConnectHandle<R: Deserializer> {
        event_rx: futures_channel::mpsc::Receiver<Event>,
        resp_rx: futures_channel::mpsc::Receiver<R>,
    }

    pub trait Connect<R: Deserializer> {
        type CErr: ErrTrait;

        fn connect(&mut self) -> Result<ConnectHandle<R>, Self::CErr>;
    }
}

#[cfg(feature = "client_recv")]
pub use recv::*;

pub trait Client {
    fn send_action<T: OBAction>(action: T) -> impl Future<Output = Result<T::Resp, Error>>;
}

pub trait ClientExt {}
