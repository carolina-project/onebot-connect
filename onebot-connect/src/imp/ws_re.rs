use ::http::HeaderValue;
use hyper::header::AUTHORIZATION;
use onebot_connect_interface::{ClosedReason, ConfigError};
use onebot_types::ob12::action::{RespData, RespStatus, RetCode};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, client::IntoClientRequest},
};

use crate::common::{ws::WSTask, CloseHandler, CmdHandler, RecvHandler};

use super::*;
use crate::Error as AllErr;

#[derive(Debug, Default)]
pub struct WSHandler;

impl CmdHandler<(Command, mpsc::UnboundedSender<tungstenite::Message>)> for WSHandler {
    async fn handle_cmd(
        &mut self,
        cmd: (Command, mpsc::UnboundedSender<tungstenite::Message>),
        state: crate::common::ConnState,
    ) -> Result<(), AllErr> {
        let (cmd, data_tx) = cmd;

        match cmd {
            Command::Close => {
                state.set_active(false);
                Ok(())
            }
            Command::GetConfig(_, tx) => tx.send(None).map_err(|_| crate::Error::ChannelClosed),
            Command::SetConfig((key, _), tx) => tx
                .send(Err(ConfigError::UnknownKey(key)))
                .map_err(|_| crate::Error::ChannelClosed),
            Command::Respond(echo, resp) => {
                let (status, retcode, message, data) = match resp {
                    ActionResponse::Ok(data) => {
                        (RespStatus::Ok, RetCode::Success, Default::default(), data)
                    }
                    ActionResponse::Error { retcode, message } => (
                        RespStatus::Failed,
                        retcode,
                        message,
                        serde_value::Value::Option(None),
                    ),
                };
                let echo = match echo {
                    ActionEcho::Inner(_) => None,
                    ActionEcho::Outer(echo) => Some(echo),
                };

                let data = serde_json::to_vec(&RespData {
                    status,
                    retcode,
                    data,
                    message,
                    echo,
                })
                .map_err(OCError::serialize)?;

                data_tx.send(data.into()).map_err(OCError::closed)?;
                Ok(())
            }
            Command::Event(event) => Ok(data_tx
                .send(
                    serde_json::to_vec(&event)
                        .map_err(OCError::serialize)?
                        .into(),
                )
                .map_err(OCError::closed)?),
        }
    }
}
impl RecvHandler<Action, RecvMessage> for WSHandler {
    async fn handle_recv(
        &mut self,
        recv: Action,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        _state: crate::common::ConnState,
    ) -> Result<(), crate::Error> {
        Ok(msg_tx
            .send(RecvMessage::Action(recv))
            .map_err(OCError::closed)?)
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

pub struct WSCreate<R>
where
    R: IntoClientRequest + Unpin,
{
    req: R,
    access_token: Option<String>,
}
impl<R: IntoClientRequest + Unpin> WSCreate<R> {
    pub fn new(req: R) -> Self {
        Self {
            req,
            access_token: None,
        }
    }
}

impl<R> Create for WSCreate<R>
where
    R: IntoClientRequest + Unpin,
{
    type Error = crate::Error;
    type Source = RxMessageSource;
    type Provider = TxImplProvider;
    type Message = ();

    async fn create(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let Self { req, access_token } = self;
        let mut req = req.into_client_request()?;
        if let Some(token) = access_token {
            req.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }
        let (ws, _) = connect_async(req).await?;
        let (cmd_tx, msg_rx) = WSTask::create(ws, WSHandler::default()).await;
        Ok((
            RxMessageSource::new(msg_rx),
            TxImplProvider::new(cmd_tx),
            (),
        ))
    }

    fn with_authorization(self, access_token: impl Into<String>) -> Self {
        Self {
            access_token: Some(access_token.into()),
            ..self
        }
    }
}
