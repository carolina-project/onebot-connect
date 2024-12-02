use std::{error::Error as ErrTrait, future::Future, pin::Pin};

use super::Error;
use onebot_types::{
    base::OBAction,
    ob12::{action::ActionType, event::Event, BotSelf},
};

#[cfg(feature = "client_recv")]
mod recv {
    use futures::channel::mpsc;
    use serde::{Deserialize, Serialize};

    use super::*;

    /// Messages received from OneBot Connect
    #[derive(Debug, Serialize, Deserialize)]
    pub enum RecvMessage {
        Event(Event),
        /// Response after closed
        Closed(Result<(), String>),
    }
    pub type MessageRecv = mpsc::Receiver<RecvMessage>;

    pub trait Connect {
        type CErr: ErrTrait;
        type Client: Client;

        fn connect(self) -> Result<(MessageRecv, Self::Client), Self::CErr>;
    }
}

#[cfg(feature = "client_recv")]
pub use recv::*;
use serde_value::Value;

pub trait Client {
    fn send_action_impl(
        &mut self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, Error>> + Send>>;

    fn close(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
}

pub trait ClientExt: Client {
    fn send_action<E, T>(
        &mut self,
        action: T,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<T::Resp, Error>>
    where
        E: std::error::Error + 'static,
        T: OBAction + TryInto<ActionType, Error = E>,
    {
        async move {
            let resp = self
                .send_action_impl(action.try_into().map_err(Error::other)?, self_)
                .await?;
            Ok(<T::Resp as serde::Deserialize>::deserialize(resp)?)
        }
    }
}

impl<T: Client> ClientExt for T {}
impl ClientExt for dyn Client {}
