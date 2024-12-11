use std::{fmt::Debug, future::Future, pin::Pin};

use onebot_types::ob12::{
    action::{ActionType, RetCode},
    event::Event,
    BotSelf,
};
use serde::{Deserialize, Serialize};
use serde_value::Value;
use tokio::sync::oneshot;

use crate::{ClosedReason, ConfigError, Error};

/// Error occurred during closing the connection.
pub struct CloseError<T> {
    pub handle: T,
    pub err: Error,
}
impl<T: OBImpl> CloseError<T> {
    pub fn new(handle: T, err: Error) -> Self {
        Self { handle, err }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
pub enum ActionEcho {
    Inner(u64),
    Outer(String),
}
/// **Inner** Action representation
#[derive(Debug, Serialize, Deserialize)]
pub struct Action {
    pub action: ActionType,
    pub echo: ActionEcho,
    pub self_: Option<BotSelf>,
}
pub enum ActionResponse {
    Ok(Value),
    Error { retcode: RetCode, message: String },
}
/// Messages received from connection
#[derive(Debug, Serialize, Deserialize)]
pub enum RecvMessage {
    Action(Action),
    /// Response after close command
    /// Ok means closed successfully, Err means close failed
    /// DO NOT send this at command handler, just set active state
    Close(Result<ClosedReason, String>),
}

/// Command enum to represent different commands that can be sent to connection
pub enum Command {
    Event(Event),
    /// Respond to the action by echo
    Respond(ActionEcho, ActionResponse),
    /// Get connection config
    GetConfig(String, oneshot::Sender<Option<Value>>),
    /// Set connection config
    SetConfig((String, Value), oneshot::Sender<Result<(), ConfigError>>),
    /// Close connection
    Close,
}

/// Trait for polling messages from a OneBot implementation.
pub trait MessageSource {
    fn poll_message(&mut self) -> impl Future<Output = Option<RecvMessage>> + Send + '_;
}

pub trait OBImpl {
    fn send_event_impl(&self, event: Event) -> impl Future<Output = Result<(), Error>> + Send + '_;

    fn respond(
        &self,
        echo: ActionEcho,
        data: ActionResponse,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_;
}

/// Trait for providing event transmitters.
pub trait OBImplProvider {
    type Output: OBImpl;

    fn provide(&mut self) -> Result<Self::Output, Error>;
}

pub trait MessageSourceDyn {
    fn poll_message(&mut self) -> Pin<Box<dyn Future<Output = Option<RecvMessage>> + Send + '_>>;
}

impl<T: MessageSource> MessageSourceDyn for T {
    fn poll_message(&mut self) -> Pin<Box<dyn Future<Output = Option<RecvMessage>> + Send + '_>> {
        Box::pin(self.poll_message())
    }
}

pub trait OBImplDyn {
    fn send_event_impl(
        &self,
        event: Event,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>>;

    fn respond_action(
        &self,
        echo: ActionEcho,
        data: ActionResponse,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>>;
}

impl<T: OBImpl + Send + 'static> OBImplDyn for T {
    fn send_event_impl(
        &self,
        event: Event,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(self.send_event_impl(event))
    }

    fn respond_action(
        &self,
        echo: ActionEcho,
        data: ActionResponse,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
        Box::pin(self.respond(echo, data))
    }
}

pub trait Create {
    type Error: std::error::Error;
    type Source: MessageSource;
    type Provider: OBImplProvider;
    type Message: Debug;

    fn create(
        self,
    ) -> impl Future<Output = Result<(Self::Source, Self::Provider, Self::Message), Self::Error>>;

    fn with_authorization(self, access_token: impl Into<String>) -> Self;
}
