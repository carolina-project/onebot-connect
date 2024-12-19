use fxhash::FxHashMap;
use http::header::AUTHORIZATION;
use http::HeaderValue;
use onebot_connect_interface::app::{
    ActionArgs, ActionResponder, Command, Connect, OBAppProvider, RecvMessage,
};
use onebot_types::ob11::action::{RawAction, RespData};
use onebot_types::ob11::RawEvent;
use serde::Deserialize;
use serde_json::Value as Json;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use tokio_tungstenite::{connect_async, WebSocketStream};

use crate::app::{generate_echo, RxMessageSource, TxAppProvider, TxAppSide};
use crate::common::ws::{WSTask, WsStream};
use crate::common::{CloseHandler, CmdHandler, RecvHandler};
use crate::Error as AllErr;
use onebot_connect_interface::{ActionResult, ClosedReason, ConfigError, Error as OCError};

use super::data::{ActionConverted, AppData};

type ActionMap = FxHashMap<String, (String, oneshot::Sender<ActionResult<serde_value::Value>>)>;

pub struct WSHandler {
    map: ActionMap,
    data: AppData,
    app: TxAppSide,
}

impl WSHandler {
    pub fn new(data: AppData, app: TxAppSide) -> Self {
        Self {
            map: Default::default(),
            data,
            app,
        }
    }

    pub async fn handle_action(
        &mut self,
        action_tx: mpsc::UnboundedSender<tungstenite::Message>,
        args: ActionArgs,
        responder: ActionResponder,
    ) -> Result<(), AllErr> {
        let ActionArgs { action, .. } = args;
        let result = self.data.convert_action(action, &self.app).await?;

        let detail = match result {
            ActionConverted::Send(send) => send,
            ActionConverted::Respond(data) => {
                return responder
                    .send(data.into_result(None).map_err(Into::into))
                    .map_err(|_| AllErr::ChannelClosed)
            }
        };
        let echo = generate_echo(8, &self.map);
        let action = RawAction {
            echo: Some(echo.clone()),
            detail,
        };
        let res = serde_json::to_vec(&action);
        match res {
            Ok(data) => {
                self.map.insert(echo, (action.detail.action, responder));
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

impl CmdHandler<(Command, mpsc::UnboundedSender<tungstenite::Message>)> for WSHandler {
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
    Event(RawEvent),
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
        } else if data.get("post_type").is_some() {
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
    ) -> Result<(), AllErr> {
        match recv {
            RecvData::Event(event) => {
                let converted = self.data.convert_event(event, &msg_tx, &self.app).await?;
                msg_tx
                    .send(RecvMessage::Event(converted))
                    .map_err(OCError::closed)?;
            }
            RecvData::Response(resp) => match resp.echo {
                Some(ref e) => {
                    if let Some((name, responder)) = self.map.remove(e) {
                        let converted = self.data.convert_resp_data(&name, resp.data).await?;
                        responder
                            .send(Ok(converted))
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

pub(crate) async fn connect_ws(
    ws: WebSocketStream<impl WsStream>,
) -> Result<(RxMessageSource, TxAppProvider, ()), AllErr> {
    let app_data = AppData::default();

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    let mut app_provider = TxAppProvider::new(cmd_tx.clone());

    WSTask::create_with_channel(
        ws,
        cmd_rx,
        msg_tx,
        WSHandler::new(app_data, app_provider.provide()?),
    )
    .await;
    Ok((RxMessageSource::new(msg_rx), TxAppProvider::new(cmd_tx), ()))
}

pub struct OB11WSConnect<R: IntoClientRequest + Unpin> {
    req: R,
    access_token: Option<String>,
}

impl<R: IntoClientRequest + Unpin> OB11WSConnect<R> {
    pub fn new(req: R) -> Self {
        Self {
            req,
            access_token: None,
        }
    }
}

impl<R: IntoClientRequest + Unpin> Connect for OB11WSConnect<R> {
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
        connect_ws(ws).await
    }

    fn with_authorization(self, access_token: impl Into<String>) -> Self {
        Self {
            access_token: Some(access_token.into()),
            ..self
        }
    }
}
