use std::{future::Future, ops::Deref, pin::Pin};

use crate::ConfigError;

use crate::Error;
use onebot_types::{
    base::OBAction,
    ob12::{
        action::{ActionDetail, RespData, RespError},
        BotSelf,
    },
    ValueMap,
};

#[cfg(feature = "app_recv")]
mod recv {
    use std::{error, fmt::Debug, ops::DerefMut};

    use onebot_types::ob12::{action::ActionDetail, event::RawEvent};
    use tokio::sync::oneshot;

    use crate::ClosedReason;

    use super::*;

    pub type ActionResponder = oneshot::Sender<Result<RespData, Error>>;

    /// Application side messages received from connection
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    pub trait MessageSource: Send {
        fn poll_message(&mut self) -> impl Future<Output = Option<RecvMessage>> + Send + '_;
    }

    pub trait MessageSourceDyn: Send {
        fn poll_message(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Option<RecvMessage>> + Send + '_>>;
    }

    impl MessageSource for Box<dyn MessageSourceDyn> {
        fn poll_message(&mut self) -> impl Future<Output = Option<RecvMessage>> + Send + '_ {
            self.deref_mut().poll_message()
        }
    }

    impl<T: MessageSource> MessageSourceDyn for T {
        fn poll_message(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Option<RecvMessage>> + Send + '_>> {
            Box::pin(self.poll_message())
        }
    }

    /// OneBot app side provider
    pub trait OBAppProvider: Send {
        type Output: OBApp;

        fn use_event_context(&self) -> bool {
            false
        }

        fn set_event_context(&mut self, event: &RawEvent) {
            unimplemented!("set event {event:?}")
        }

        /// Provides a OneBot app instance.
        fn provide(&mut self) -> Result<Self::Output, Error>;
    }

    pub trait AppProviderDyn: Send {
        fn use_event_context(&self) -> bool;

        fn set_event_context(&mut self, event: &RawEvent);

        fn provide(&mut self) -> Result<Box<dyn AppDyn>, Error>;
    }

    impl<T: OBAppProvider + 'static> AppProviderDyn for T {
        fn use_event_context(&self) -> bool {
            self.use_event_context()
        }

        fn set_event_context(&mut self, event: &RawEvent) {
            self.set_event_context(event)
        }

        fn provide(&mut self) -> Result<Box<dyn AppDyn>, Error> {
            Ok(Box::new(self.provide()?))
        }
    }

    impl OBAppProvider for Box<dyn AppProviderDyn> {
        type Output = Box<dyn AppDyn>;

        fn provide(&mut self) -> Result<Self::Output, Error> {
            self.deref_mut().provide()
        }
    }

    /// Trait to define the connection behavior
    pub trait Connect {
        /// Connection error type
        type Error: error::Error + Send + 'static;
        /// Message after connection established successfully.
        type Message: Debug + Send;
        /// Client for applcation to call.
        type Provider: OBAppProvider;
        /// Message source for receiving messages.
        type Source: MessageSource;

        fn connect(
            self,
        ) -> impl Future<Output = Result<(Self::Source, Self::Provider, Self::Message), Self::Error>>
               + Send;

        fn with_authorization(self, access_token: impl Into<String>) -> Self;
    }
}

#[cfg(feature = "app_recv")]
pub use recv::*;
use serde::Deserialize;
use serde_value::Value;

use super::AppExt;

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
    ) -> impl Future<Output = Result<Option<RespData>, Error>> + Send + '_;

    /// Close connection, **DO NOT** call `release()` after this.
    fn close(&self) -> impl Future<Output = Result<(), Error>> + Send + '_;

    /// Get config from connection.
    #[allow(unused)]
    fn get_config(
        &self,
        key: impl AsRef<str>,
    ) -> impl Future<Output = Result<Option<Value>, Error>> + Send + '_ {
        async { Ok(None) }
    }

    /// Sets a configuration value in the connection.
    #[allow(unused)]
    fn set_config(
        &self,
        key: impl AsRef<str>,
        value: Value,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_ {
        let key = key.as_ref().into();
        async move { Err(ConfigError::UnknownKey(key).into()) }
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
    ) -> Pin<Box<dyn Future<Output = Result<Option<RespData>, Error>> + Send + '_>>;

    fn get_config(
        &self,
        key: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, Error>> + Send + '_>>;

    fn clone_app(&self) -> Box<dyn AppDyn>;

    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>>;

    fn set_config(
        &self,
        key: String,
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

    fn get_config(
        &self,
        key: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, Error>> + Send + '_>> {
        Box::pin(self.get_config(key))
    }

    fn set_config(
        &self,
        key: String,
        value: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(self.set_config(key, value))
    }

    fn send_action_dyn(
        &self,
        action: ActionDetail,
        self_: Option<BotSelf>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<RespData>, Error>> + Send + '_>> {
        Box::pin(self.send_action_impl(action, self_))
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(self.close())
    }

    fn clone_app(&self) -> Box<dyn AppDyn> {
        Box::new(self.clone_app())
    }

    fn release(&mut self) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(self.release())
    }
}

macro_rules! deref_impl {
    ($ty:ty) => {
        impl OBApp for $ty {
            fn response_supported(&self) -> bool {
                self.deref().response_supported()
            }

            fn send_action_impl(
                &self,
                action: ActionDetail,
                self_: Option<BotSelf>,
            ) -> impl Future<Output = Result<Option<RespData>, Error>> + Send + '_ {
                self.deref().send_action_dyn(action, self_)
            }

            fn close(&self) -> impl Future<Output = Result<(), Error>> + Send + '_ {
                self.deref().close()
            }

            fn clone_app(&self) -> Self
            where
                Self: 'static,
            {
                self.deref().clone_app().into()
            }

            fn get_config(
                &self,
                key: impl AsRef<str>,
            ) -> impl Future<Output = Result<Option<Value>, Error>> + Send + '_ {
                self.deref().get_config(key.as_ref().to_owned())
            }

            fn set_config(
                &self,
                key: impl AsRef<str>,
                value: Value,
            ) -> impl Future<Output = Result<(), Error>> + Send + '_ {
                self.deref().set_config(key.as_ref().to_owned(), value)
            }
        }
    };
}

deref_impl!(Box<dyn AppDyn>);

impl<T: OBApp> AppExt for T {
    fn send_action<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>> + Send + '_
    where
        A: OBAction + Send + 'static,
    {
        let action_name = action.action_name().to_owned();
        let value_res = serde_value::to_value(action)
            .and_then(|r| ValueMap::deserialize(r).map_err(serde::ser::Error::custom))
            .map(move |r| {
                self.send_action_impl(
                    ActionDetail {
                        action: action_name,
                        params: r,
                    },
                    self_,
                )
            });
        async move {
            if let Some(resp) = value_res?.await? {
                if resp.is_success() {
                    Ok(Some(<A::Resp as Deserialize>::deserialize(resp.data)?))
                } else {
                    Err(RespError::from(resp).into())
                }
            } else {
                Ok(None)
            }
        }
    }
}

impl AppExt for dyn AppDyn {
    fn send_action<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>> + Send + '_
    where
        A: OBAction + Send + 'static,
    {
        let action_name = action.action_name().to_owned();
        let value_res = serde_value::to_value(action)
            .and_then(|r| ValueMap::deserialize(r).map_err(serde::ser::Error::custom))
            .map(move |r| {
                self.send_action_dyn(
                    ActionDetail {
                        action: action_name,
                        params: r,
                    },
                    self_,
                )
            });
        async move {
            if let Some(resp) = value_res?.await? {
                if resp.is_success() {
                    Ok(Some(<A::Resp as Deserialize>::deserialize(resp.data)?))
                } else {
                    Err(RespError::from(resp).into())
                }
            } else {
                Ok(None)
            }
        }
    }
}
