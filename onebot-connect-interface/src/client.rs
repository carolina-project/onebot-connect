use std::{error::Error as ErrTrait, future::Future, pin::Pin};

use super::Error;
use onebot_types::{
    base::OBAction,
    ob12::{action::ActionType, BotSelf},
};

#[cfg(feature = "client_recv")]
mod recv {
    use futures::channel::oneshot;
    use onebot_types::ob12::event::Event;
    use serde::{Deserialize, Serialize};

    use super::*;

    /// Messages received from OneBot Connect
    #[derive(Debug, Serialize, Deserialize)]
    pub enum RecvMessage {
        Event(Event),
        /// Response after closed
        Closed(Result<(), String>),
    }

    /// Action args passed to OneBot Connect with command channel
    /// Response will be sent using `resp_tx`
    pub struct ActionArgs {
        pub action: ActionType,
        pub self_: Option<BotSelf>,
        pub resp_tx: oneshot::Sender<Result<Value, Error>>,
    }

    /// Command enum to represent different commands that can be sent to OneBot Connect
    pub enum Command {
        Action(ActionArgs),
        Close,
    }

    /// Receiver for events from OneBot Connect
    pub trait EventSource {
        fn poll_event(&mut self) -> impl Future<Output = Option<Event>> + Send + '_;
    }

    /// Trait to define the connection behavior for OneBot Connect
    pub trait Connect {
        type CErr: ErrTrait;
        type Client: Client;
        type ESource: EventSource;

        fn connect(self) -> Result<(Self::ESource, Self::Client), Self::CErr>;
    }
}

#[cfg(feature = "client_recv")]
pub use recv::*;
use serde_value::Value;

/// OneBot application client, providing functions to interact with OneBot Connect
pub trait Client {
    fn send_action_impl(
        &mut self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Value, Error>> + Send + 'static;

    fn close_impl(&mut self) -> impl Future<Output = Result<(), String>> + Send + 'static;
}

pub trait ClientDyn {
    fn send_action_dyn(
        &mut self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, Error>> + Send>>;

    fn close_dyn(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
}

impl<T: Client> ClientDyn for T {
    fn send_action_dyn(
        &mut self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, Error>> + Send>> {
        Box::pin(self.send_action_impl(action, self_))
    }

    fn close_dyn(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        Box::pin(self.close_impl())
    }
}

pub trait ClientExt {
    fn send_action<E, A>(
        &mut self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<A::Resp, Error>>
    where
        E: std::error::Error + 'static,
        A: OBAction + TryInto<ActionType, Error = E>;
}

impl<T: Client> ClientExt for T {
    fn send_action<E, A>(
        &mut self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<A::Resp, Error>>
    where
        E: std::error::Error + 'static,
        A: OBAction + TryInto<ActionType, Error = E>,
    {
        async move {
            let resp = self
                .send_action_impl(action.try_into().map_err(Error::other)?, self_)
                .await?;
            Ok(<A::Resp as serde::Deserialize>::deserialize(resp)?)
        }
    }
}

impl ClientExt for dyn ClientDyn {
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
                .send_action_dyn(action.try_into().map_err(Error::other)?, self_)
                .await?;
            Ok(<T::Resp as serde::Deserialize>::deserialize(resp)?)
        }
    }
}
