use std::{error::Error as ErrTrait, future::Future, pin::Pin};

use crate::ConfigError;

use super::Error;
use onebot_types::{
    base::OBAction,
    ob12::{
        action::{ActionDetail, GetLatestEvents},
        event::RawEvent,
        BotSelf,
    },
    ValueMap,
};

#[cfg(feature = "app_recv")]
mod recv {
    use std::fmt::Debug;

    use onebot_types::ob12::{
        action::{ActionDetail, RespStatus, RetCode},
        event::RawEvent,
    };
    use tokio::sync::oneshot;

    use crate::ClosedReason;

    use super::*;

    pub type ActionResponder = oneshot::Sender<Result<Value, Error>>;

    /// Messages received from connection
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub enum RecvMessage {
        Event(RawEvent),
        /// Response after close command
        /// Ok means closed successfully, Err means close failed
        /// DO NOT send this at command handler, just set active state
        Close(Result<ClosedReason, String>),
    }

    /// Action args passed to connection with command channel
    /// Response will be sent using `resp_tx`
    #[derive(Debug, Clone)]
    pub struct ActionArgs {
        pub action: ActionDetail,
        pub self_: Option<BotSelf>,
    }

    /// Response args passed to connection.
    #[derive(Debug, Clone)]
    pub struct RespArgs {
        pub status: RespStatus,
        pub retcode: RetCode,
        pub data: Value,
        pub message: String,
    }

    impl RespArgs {
        pub fn failed(retcode: RetCode, msg: impl Into<String>) -> Self {
            Self {
                status: RespStatus::Failed,
                retcode,
                data: Value::Option(None),
                message: msg.into(),
            }
        }

        pub fn success<T: Serialize>(data: T) -> Result<Self, serde_value::SerializerError> {
            Ok(Self {
                status: RespStatus::Ok,
                retcode: RetCode::Success,
                data: serde_value::to_value(data)?,
                message: Default::default(),
            })
        }
    }

    /// Command enum to represent different commands that can be sent to connection
    pub enum Command {
        /// Send action and receive response
        Action(ActionArgs, ActionResponder),
        /// Respond to the event by id
        Respond(String, Vec<ActionArgs>),
        /// Get connection config
        GetConfig(String, oneshot::Sender<Option<Value>>),
        /// Set connection config
        SetConfig((String, Value), oneshot::Sender<Result<(), ConfigError>>),
        /// Close connection
        Close,
    }

    /// Receiver for messages from OneBot Connect
    pub trait MessageSource {
        fn poll_message(&mut self) -> impl Future<Output = Option<RecvMessage>> + Send + '_;
    }

    pub trait MessageSourceDyn {
        fn poll_message(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Option<RecvMessage>> + Send + '_>>;
    }

    impl<T: MessageSource> MessageSourceDyn for T {
        fn poll_message(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Option<RecvMessage>> + Send + '_>> {
            Box::pin(self.poll_message())
        }
    }

    /// OneBot app side provider
    pub trait OBAppProvider {
        type Output: OBApp;

        /// Provides a OneBot app instance.
        fn provide(&mut self) -> Result<Self::Output, Error>;
    }

    /// Trait to define the connection behavior
    pub trait Connect {
        /// Connection error type
        type Error: ErrTrait;
        /// Message after connection established successfully.
        type Message: Debug;
        /// Client for applcation to call.
        type Provider: OBAppProvider;
        /// Message source for receiving messages.
        type Source: MessageSource;

        fn connect(
            self,
        ) -> impl Future<Output = Result<(Self::Source, Self::Provider, Self::Message), Self::Error>>;

        fn with_authorization(self, access_token: impl Into<String>) -> Self;
    }
}

#[cfg(feature = "app_recv")]
pub use recv::*;
use serde::{de::IntoDeserializer, Deserialize, Serialize};
use serde_value::Value;

/// Application client, providing functions to interact with connection
pub trait OBApp: Send + Sync {
    /// Checks if the client supports action response.
    /// For example, WebSocket and Http supports response, WebHook doesn't.
    fn response_supported(&self) -> bool {
        true
    }

    /// Sends an action to connection and returns the result.
    /// If action reponse is supported, return Ok(Some), Ok(None) otherwise.
    fn send_action_impl(
        &self,
        action: ActionDetail,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<Value>, Error>> + Send + '_;

    /// Get config from connection.
    #[allow(unused)]
    fn get_config<'a, 'b: 'a>(
        &'a self,
        key: impl Into<String> + Send + 'b,
    ) -> impl Future<Output = Result<Option<Value>, Error>> + Send + '_ {
        async { Ok(None) }
    }

    /// Sets a configuration value in the connection.
    #[allow(unused)]
    fn set_config<'a, 'b: 'a>(
        &'a self,
        key: impl Into<String> + Send + 'b,
        value: Value,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_ {
        async move { Err(ConfigError::UnknownKey(key.into()).into()) }
    }

    /// Clone app client for multi-thread usage.
    fn clone_app(&self) -> Self
    where
        Self: 'static;

    /// App client releasing logic, such as sending actions stored.
    fn release(&mut self) -> impl Future<Output = Result<(), Error>> + Send + '_ {
        async { Ok(()) }
    }
}

/// Extension trait for `App` to provide dynamic dispatching capabilities
pub trait AppDyn: Send + Sync {
    fn response_supported(&self) -> bool {
        true
    }

    fn send_action_dyn(
        &self,
        action: ActionDetail,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, Error>> + Send + '_>>;

    fn get_config<'a, 'b: 'a>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, Error>> + Send + '_>>;

    fn clone_app(&self) -> Box<dyn AppDyn>;

    fn set_config<'a, 'b: 'a>(
        &'a self,
        key: &'b str,
        value: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>>;

    fn release(&mut self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

impl<T: OBApp + 'static> AppDyn for T {
    fn response_supported(&self) -> bool {
        self.response_supported()
    }

    fn get_config<'a, 'b: 'a>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, Error>> + Send + '_>> {
        Box::pin(self.get_config(key))
    }

    fn set_config<'a, 'b: 'a>(
        &'a self,
        key: &'b str,
        value: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(self.set_config(key, value))
    }

    fn send_action_dyn(
        &self,
        action: ActionDetail,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, Error>> + Send + '_>> {
        Box::pin(self.send_action_impl(action, self_))
    }

    fn clone_app(&self) -> Box<dyn AppDyn> {
        Box::new(self.clone_app())
    }

    fn release(&mut self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(self.release())
    }
}

#[inline]
fn process_resp<R>(resp: Option<R>) -> Result<R, Error> {
    match resp {
        Some(res) => Ok(res),
        None => Err(Error::NotSupported("action response not supported".into())),
    }
}

/// Extension trait for `App` to provide additional functionalities
pub trait AppExt {
    /// Send an action, return `Ok(Some(A::Resp))` if `App` is responsive, `Err()` if error
    /// occurred while processing action.
    fn send_action<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>> + Send + '_
    where
        A: OBAction + Send + 'static;

    /// Only send action, ignore its response excluding error.
    fn send_action_only<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_
    where
        A: OBAction + Send + 'static,
        Self: Sync,
    {
        async move { self.send_action(action, self_).await.map(|_| ()) }
    }

    /// Send an action, treating unresponsive as an error.
    fn call_action<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<A::Resp, Error>> + Send + '_
    where
        A: OBAction + Send + 'static,
        Self: Sync,
    {
        async move {
            self.send_action::<A>(action, self_)
                .await?
                .ok_or_else(|| Error::missing("response"))
        }
    }

    fn get_latest_events<'a>(
        &'a self,
        limit: i64,
        timeout: i64,
        self_: Option<BotSelf>,
    ) -> impl std::future::Future<Output = Result<Vec<RawEvent>, Error>> + Send + 'a
    where
        Self: Sync,
    {
        async move {
            let action = GetLatestEvents {
                limit,
                timeout,
                extra: Default::default(),
            };

            let response = self.send_action(action, self_).await?;
            process_resp(response)
        }
    }
}

impl<T: OBApp + Sync> AppExt for T {
    fn send_action<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>> + Send + '_
    where
        A: OBAction + Send + 'static,
    {
        async move {
            let resp = self
                .send_action_impl(
                    ActionDetail {
                        action: action.action_name().into(),
                        params: ValueMap::deserialize(serde_value::to_value(action)?)?,
                    },
                    self_,
                )
                .await?;
            Ok(match resp {
                Some(resp) => Some(<A::Resp as Deserialize>::deserialize(
                    resp.into_deserializer(),
                )?),
                None => None,
            })
        }
    }
}

impl AppExt for dyn AppDyn + Sync {
    fn send_action<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>> + Send + '_
    where
        A: OBAction + Send + 'static,
    {
        async move {
            let resp = self
                .send_action_dyn(
                    ActionDetail {
                        action: action.action_name().into(),
                        params: ValueMap::deserialize(serde_value::to_value(action)?)?,
                    },
                    self_,
                )
                .await?;
            Ok(match resp {
                Some(resp) => Some(<A::Resp as Deserialize>::deserialize(
                    resp.into_deserializer(),
                )?),
                None => None,
            })
        }
    }
}
