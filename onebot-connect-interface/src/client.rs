use std::{error::Error as ErrTrait, future::Future, pin::Pin};

use crate::ConfigError;

use super::Error;
use onebot_types::{
    base::OBAction,
    ob12::{action::ActionType, BotSelf},
};

#[derive(Debug, Serialize, Deserialize)]
pub enum ClosedReason {
    /// Closed successfully.
    Ok,
    /// Closed, but some error occurred.
    Error(String),
    /// Partially closed.
    Partial(String),
}

#[cfg(feature = "client_recv")]
mod recv {
    use std::fmt::Debug;

    use onebot_types::ob12::event::Event;
    use tokio::sync::oneshot;

    use super::*;

    /// Messages received from connection
    #[derive(Debug, Serialize, Deserialize)]
    pub enum RecvMessage {
        Event(Event),
        /// Response after close command
        /// Ok means closed successfully, Err means close failed
        Close(Result<ClosedReason, String>),
    }

    /// Action args passed to connection with command channel
    /// Response will be sent using `resp_tx`
    pub struct ActionArgs {
        pub action: ActionType,
        pub self_: Option<BotSelf>,
        pub resp_tx: oneshot::Sender<Result<Value, Error>>,
    }

    /// Command enum to represent different commands that can be sent to connection
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

    pub trait ClientProvider {
        type Output: Client;

        /// Provides a client instance.
        fn provide(&self) -> Result<Self::Output, Error>;
    }

    /// Trait to define the connection behavior
    pub trait Connect {
        /// Connection error type
        type Error: ErrTrait;
        /// Message after connection established successfully.
        type Message: Debug;
        /// Client for applcation to call.
        type Provider: ClientProvider;
        /// Message source for receiving messages.
        type Source: MessageSource;

        fn connect(
            self,
        ) -> impl Future<Output = Result<(Self::Source, Self::Provider, Self::Message), Self::Error>>;

        fn with_authorization(self, access_token: impl AsRef<str>) -> Self;
    }
}

#[cfg(feature = "client_recv")]
pub use recv::*;
use serde::{Deserialize, Serialize};
use serde_value::Value;

/// Application client, providing functions to interact with connection
pub trait Client {
    /// Checks if the client supports action response.
    /// For example, WebSocket and Http supports response, WebHook doesn't.
    fn response_supported(&self) -> bool {
        true
    }

    /// Sends an action to connection and returns the result.
    /// If action reponse is supported, return Ok(Some), Ok(None) otherwise.
    fn send_action_impl(
        &self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<Value>, Error>> + Send + '_;

    /// Get config from connection's client.
    fn get_config<'a, 'b: 'a>(
        &'a self,
        key: impl AsRef<str> + Send + 'b,
    ) -> impl Future<Output = Option<Value>> + Send + '_;

    /// Sets a configuration value in the connection.
    fn set_config<'a, 'b: 'a>(
        &'a self,
        key: impl AsRef<str> + Send + 'b,
        value: Value,
    ) -> impl Future<Output = Result<(), ConfigError>> + Send + '_;

    /// Client releasing logic, such as sending actions stored.
    fn release(self) -> impl Future<Output = Result<(), Error>> + Send + 'static
    where
        Self: Sized,
    {
        async { Ok(()) }
    }
}

/// Extension trait for `Client` to provide dynamic dispatching capabilities
pub trait ClientDyn {
    fn response_supported(&self) -> bool {
        true
    }

    fn send_action_dyn(
        &self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, Error>> + Send + '_>>;

    fn get_config<'a, 'b: 'a>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Option<Value>> + Send + '_>>;

    fn set_config<'a, 'b: 'a>(
        &'a self,
        key: &'b str,
        value: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), ConfigError>> + Send + '_>>;

    fn release(self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'static>>
    where
        Self: Sized,
    {
        Box::pin(async { Ok(()) })
    }
}

impl<T: Client> ClientDyn for T {
    fn response_supported(&self) -> bool {
        self.response_supported()
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

    fn send_action_dyn(
        &self,
        action: ActionType,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, Error>> + Send + '_>> {
        Box::pin(self.send_action_impl(action, self_))
    }

    fn release(self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'static>> {
        Box::pin(self.release())
    }
}

/// Extension trait for `Client` to provide additional functionalities
pub trait ClientExt {
    fn send_action<E, A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>>
    where
        E: std::error::Error + Send + 'static,
        A: OBAction + TryInto<ActionType, Error = E>;
}

impl<T: Client> ClientExt for T {
    fn send_action<E, A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>>
    where
        E: std::error::Error + Send + 'static,
        A: OBAction + TryInto<ActionType, Error = E>,
    {
        async move {
            let resp = self
                .send_action_impl(action.try_into().map_err(Error::other)?, self_)
                .await?;
            Ok(match resp {
                Some(resp) => Some(<A::Resp as Deserialize>::deserialize(resp)?),
                None => None,
            })
        }
    }
}

impl ClientExt for dyn ClientDyn {
    fn send_action<E, A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>>
    where
        E: std::error::Error + Send + 'static,
        A: OBAction + TryInto<ActionType, Error = E>,
    {
        async move {
            let resp = self
                .send_action_dyn(action.try_into().map_err(Error::other)?, self_)
                .await?;
            Ok(match resp {
                Some(resp) => Some(<A::Resp as Deserialize>::deserialize(resp)?),
                None => None,
            })
        }
    }
}
