use ::http::HeaderValue;
use hyper::header::AUTHORIZATION;
use onebot_connect_interface::{
    app::{ActionArgs, ActionResponder, Command, Connect, RecvMessage},
    ClosedReason, ConfigError, Error as OCError,
};
use onebot_types::ob12::{self, action::RespData, event::Event};
use serde::Deserialize;
use serde_json::Value as Json;
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, client::IntoClientRequest},
    MaybeTlsStream, WebSocketStream,
};

pub type WebSocketConn = WebSocketStream<MaybeTlsStream<TcpStream>>;

use crate::{
    app::generate_echo,
    common::{ws::WSTask, CloseHandler, CmdHandler, RecvHandler},
};

use super::*;
use crate::Error as AllErr;

#[derive(Debug, Default)]
pub struct WSHandler {
    map: ActionMap,
}

impl WSHandler {
    pub async fn handle_action(
        &mut self,
        action_tx: mpsc::UnboundedSender<tungstenite::Message>,
        args: ActionArgs,
        responder: ActionResponder,
    ) -> Result<(), AllErr> {
        let ActionArgs { action, self_ } = args;
        let echo = generate_echo(8, &self.map);

        let res = serde_json::to_vec(&ob12::action::Action {
            action,
            echo: Some(echo.clone()),
            self_,
        });
        match res {
            Ok(data) => {
                self.map.insert(echo, responder);
                action_tx.send(data.into()).map_err(OCError::closed)?;
            }
            Err(e) => {
                responder
                    .send(Err(OCError::other(e)))
                    .map_err(|_| AllErr::ChannelClosed)?;
            }
        }

        Ok(())
    }
}

impl CmdHandler<(Command, mpsc::UnboundedSender<tungstenite::Message>), RecvMessage> for WSHandler {
    async fn handle_cmd(
        &mut self,
        cmd: (Command, mpsc::UnboundedSender<tungstenite::Message>),
        state: crate::common::ConnState,
    ) -> Result<(), AllErr> {
        let (cmd, action_tx) = cmd;
        match cmd {
            Command::Action(args, responder) => {
                self.handle_action(action_tx, args, responder).await
            }
            Command::Close => {
                state.set_active(false);
                Ok(())
            }
            Command::GetConfig(_, tx) => tx.send(None).map_err(|_| AllErr::ChannelClosed),
            Command::SetConfig((key, _), tx) => tx
                .send(Err(ConfigError::UnknownKey(key)))
                .map_err(|_| AllErr::ChannelClosed),
            Command::Respond(_, _) => {
                log::error!("command not supported: respond");
                Ok(())
            }
        }
    }
}

pub(super) enum RecvData {
    Event(Event),
    /// Response data and `echo`
    Response(RespData),
}

impl<'de> Deserialize<'de> for RecvData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let data: serde_json::Map<String, Json> = Deserialize::deserialize(deserializer)?;
        if data.get("echo").is_some() {
            Ok(RecvData::Response(
                serde_json::from_value(Json::Object(data)).map_err(Error::custom)?,
            ))
        } else if data.get("type").is_some() {
            Ok(RecvData::Event(
                serde_json::from_value(Json::Object(data)).map_err(Error::custom)?,
            ))
        } else {
            Err(Error::custom("unknown data type"))
        }
    }
}

impl RecvHandler<RecvData, RecvMessage> for WSHandler {
    async fn handle_recv(
        &mut self,
        recv: RecvData,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        _state: crate::common::ConnState,
    ) -> Result<(), crate::Error> {
        match recv {
            RecvData::Event(event) => {
                msg_tx
                    .send(RecvMessage::Event(event))
                    .map_err(OCError::closed)?;
            }
            RecvData::Response(resp) => match resp.echo {
                Some(ref e) => {
                    if let Some(responder) = self.map.remove(e) {
                        responder
                            .send(Ok(resp.data))
                            .map_err(|_| crate::Error::ChannelClosed)?;
                    }
                }
                None => {
                    log::error!("action missing field `echo`")
                }
            },
        }
        Ok(())
    }
}

impl CloseHandler<RecvMessage> for WSHandler {
    async fn handle_close(
        &mut self,
        result: Result<ClosedReason, String>,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
    ) -> Result<(), crate::Error> {
        Ok(msg_tx
            .send(RecvMessage::Close(result))
            .map_err(OCError::closed)?)
    }
}

/// OneBot Connect Websocket Generator
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
}

impl<R: IntoClientRequest + Unpin> Connect for WSConnect<R> {
    type Error = crate::Error;
    type Message = ();
    type Provider = TxAppProvider;
    type Source = RxMessageSource;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let mut req = self.req.into_client_request()?;
        if let Some(token) = self.access_token {
            req.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }
        let (ws, _) = connect_async(req).await?;
        let (cmd_tx, msg_rx) = WSTask::create(ws, WSHandler::default()).await;
        Ok((RxMessageSource::new(msg_rx), TxAppProvider::new(cmd_tx), ()))
    }

    fn with_authorization(self, access_token: impl Into<String>) -> Self {
        Self {
            access_token: Some(access_token.into()),
            ..self
        }
    }
}
