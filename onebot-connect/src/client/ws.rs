use std::net::TcpStream;

use futures::channel::mpsc;
use http::{header::InvalidHeaderValue, HeaderValue};
use onebot_connect_interface::{
    client::{Client, Command, Connect, RecvMessage},
    Error,
};
use onebot_types::ob12::{self};
use serde_value::Value;
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    connect_async, tungstenite::{client::IntoClientRequest, WebSocket}, MaybeTlsStream, WebSocketStream
};

pub type WebSocketConn = WebSocketStream<MaybeTlsStream<TcpStream>>;

use crate::RxMessageSource;

#[derive(Debug, Error)]
pub enum WsConnectError {
    #[error(transparent)]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error(transparent)]
    HeaderValue(#[from] InvalidHeaderValue),
}

/// OneBot WebSocket Connect Generator
pub struct WSConnect<R>
where
    R: IntoClientRequest + Unpin,
{
    req: R,
    access_token: Option<String>,
}

impl<R: IntoClientRequest + Unpin> WSConnect<R> {
    pub fn new(req: R) -> Self {
        Self {
            req,
            access_token: None,
        }
    }

    pub fn with_authorization(self, access_token: impl AsRef<str>) -> Self {
        Self {
            req: self.req,
            access_token: Some(access_token.as_ref().to_owned()),
        }
    }
}

impl<R: IntoClientRequest + Unpin> Connect for WSConnect<R> {
    type CErr = WsConnectError;

    type Client = WSClient;

    type Source = RxMessageSource;

    async fn connect(self) -> Result<(Self::Source, Self::Client), Self::CErr> {
        let mut req = self.req.into_client_request()?;
        if let Some(token) = self.access_token {
            req.headers_mut()
                .insert("Authorization", HeaderValue::from_str(&token)?);
        }
        let (ws, _) = connect_async(self.req).await?;

    }
}

/// WebSocket connection task handle
pub struct WSConnection {
    task_handle: JoinHandle<()>,
}

pub struct WSConnectionHandle {
    msg_rx: mpsc::Receiver<RecvMessage>,
    cmd_tx: mpsc::Sender<Command>,
}

impl WSConnection {
    pub fn new(mut ws: WebSocketConn) -> (Self, WSConnectionHandle) {
        let (msg_tx, msg_rx) = mpsc::channel(24);
        let (cmd_tx, cmd_rx) = mpsc::channel(24);

        ws.split()

    }

    async fn event_task() {}
}

/// Client for application to call
pub struct WSClient {}

impl Client for WSClient {
    fn send_action_impl(
        &mut self,
        action: onebot_types::ob12::action::ActionType,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> impl std::future::Future<Output = Result<Value, Error>> + Send + 'static {
        todo!()
    }

    fn close_impl(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send + 'static {
        todo!()
    }
}
