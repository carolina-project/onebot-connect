use std::{error::Error as ErrTrait, future::Future, pin::Pin};

use crate::ConfigError;

use super::Error;
use onebot_types::{
    base::OBAction,
    ob12::{action::ActionType, BotSelf},
};

#[cfg(feature = "client_recv")]
mod recv {
    use std::fmt::Debug;

    use onebot_types::ob12::event::Event;
    use serde::{Deserialize, Serialize};
    use tokio::sync::oneshot;

    use super::*;

    #[derive(Debug, Serialize, Deserialize)]
    pub enum ClosedReason {
        /// Closed successfully.
        Ok,
        /// Closed, but some error occurred.
        Error(String),
        /// Partially closed.
        Partial(String),
    }

    /// Messages received from OneBot Connect
    #[derive(Debug, Serialize, Deserialize)]
    pub enum RecvMessage {
        Event(Event),
        /// Response after close command
        /// Ok means closed successfully, Err means close failed
        Close(Result<ClosedReason, String>),
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
        GetConfig(String, oneshot::Sender<Option<Value>>),
        SetConfig((String, Value), oneshot::Sender<Result<(), ConfigError>>),
        Close(oneshot::Sender<Result<ClosedReason, String>>),
    }

    /// Receiver for messages from OneBot Connect
    pub trait MessageSource {
        fn poll_event(&mut self) -> impl Future<Output = Option<RecvMessage>> + Send + '_;
    }

    /// Trait to define the connection behavior for OneBot Connect
    pub trait Connect {
        /// Connection error type
        type Err: ErrTrait;
        /// Message after connection established successfully.
        type Message: Debug;
        /// Client for applcation to call.
        type Client: Client;
        type Source: MessageSource;

        fn connect(
            self,
        ) -> impl Future<Output = Result<(Self::Source, Self::Client, Self::Message), Self::Err>>;

        fn with_authorization(self, access_token: impl AsRef<str>) -> Self;
    }
}

#[cfg(feature = "client_recv")]
pub use recv::*;
use serde_value::Value;

/// OneBot application client, providing functions to interact with OneBot Connect
pub trait Client {
    fn send_action_impl(
        &self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Value, Error>> + Send + '_;

    fn close_impl(&self) -> impl Future<Output = Result<(), String>> + Send + '_;

    fn get_config<'a, 'b: 'a>(
        &'a self,
        key: impl AsRef<str> + Send + 'b,
    ) -> impl Future<Output = Option<Value>> + Send + '_;

    fn set_config<'a, 'b: 'a>(
        &'a self,
        key: impl AsRef<str> + Send + 'b,
        value: Value,
    ) -> impl Future<Output = Result<(), ConfigError>> + Send + '_;
}

pub trait ClientDyn {
    fn send_action_dyn(
        &self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, Error>> + Send + '_>>;

    fn close_dyn(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    fn get_config<'a, 'b: 'a>(&'a self, key: &'b str) -> Pin<Box<dyn Future<Output = Option<Value>> + Send + '_>>;

    fn set_config<'a, 'b: 'a>(
        &'a self,
        key: &'b str,
        value: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), ConfigError>> + Send + '_>>;
}

impl<T: Client> ClientDyn for T {
    fn send_action_dyn(
        &self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, Error>> + Send + '_>> {
        Box::pin(self.send_action_impl(action, self_))
    }

    fn close_dyn(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(self.close_impl())
    }

    fn get_config<'a, 'b: 'a>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Option<Value>> + Send + '_>> {
        Box::pin(self.get_config(key))
    }

    fn set_config<'a, 'b: 'a>(
            &'a self,
            key: &'b str,
            value: Value,
        ) -> Pin<Box<dyn Future<Output = Result<(), ConfigError>> + Send + '_>> {
        Box::pin(self.set_config(key, value))
    }
}

pub trait ClientExt {
    fn send_action<E, A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<A::Resp, Error>>
    where
        E: std::error::Error + Send + 'static,
        A: OBAction + TryInto<ActionType, Error = E>;
}

impl<T: Client> ClientExt for T {
    fn send_action<E, A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<A::Resp, Error>>
    where
        E: std::error::Error + Send + 'static,
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
        &self,
        action: T,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<T::Resp, Error>>
    where
        E: std::error::Error + Send + 'static,
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
