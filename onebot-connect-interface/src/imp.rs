use std::{fmt::Debug, future::Future, pin::Pin};

use onebot_types::ob12::{self, action::RetCode, event::Event, BotSelf};
use serde::{Deserialize, Serialize};
use serde_value::Value;
use tokio::sync::oneshot;

use crate::{ClosedReason, ConfigError, Error};

/// Error occurred during closing the connection.
pub struct CloseError<T> {
    pub handle: T,
    pub err: Error,
}
impl<T: Impl> CloseError<T> {
    pub fn new(handle: T, err: Error) -> Self {
        Self { handle, err }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ActionEcho {
    Inner(u64),
    Outer(String),
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Action {
    action: ob12::action::Action,
    echo: ActionEcho,
    self_: Option<BotSelf>,
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
    Close(oneshot::Sender<Result<ClosedReason, String>>),
}

/// Trait for polling messages from a OneBot implementation.
pub trait MessageSource {
    fn poll_message(&mut self) -> impl Future<Output = Option<RecvMessage>> + Send + '_;
}

pub trait Impl {
    fn send_event_impl(&self, event: Event) -> impl Future<Output = Result<(), Error>> + Send + '_;

    fn respond(
        &self,
        echo: ActionEcho,
        data: ActionResponse,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_;
}

/// Trait for providing event transmitters.
pub trait ImplProvider {
    type Output: Impl;

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

pub trait ImplDyn {
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

impl<T: Impl + Send + 'static> ImplDyn for T {
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
    type Source: MessageSource;
    type Provider: ImplProvider;
    type Message: Debug;

    fn create(
        self,
    ) -> impl Future<Output = Result<(Self::Source, Self::Provider, Self::Message), Error>> + Send;
}
