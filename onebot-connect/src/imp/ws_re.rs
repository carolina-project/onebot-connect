use ::http::{header::*, HeaderValue};
use onebot_connect_interface::{
    imp::{Action, Create},
    ClosedReason, ConfigError,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, client::IntoClientRequest},
};

use crate::common::{ws::WSTask, CloseHandler, CmdHandler, RecvHandler};

use super::*;
use crate::Error as AllErr;

#[derive(Debug, Clone)]
pub struct WSHandler;

impl CmdHandler<(Command, mpsc::UnboundedSender<tungstenite::Message>)> for WSHandler {
    async fn handle_cmd(
        &self,
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
                let echo = match echo {
                    ActionEcho::Inner(_) => None,
                    ActionEcho::Outer(echo) => Some(echo),
                };
                let data =
                    serde_json::to_vec(&resp.into_resp_data(echo)).map_err(OCError::serialize)?;

                data_tx.send(data.into()).map_err(OCError::closed)?;
                Ok(())
            }
            Command::Event(event) => Ok(data_tx
                .send(
                    serde_json::to_string(&event)
                        .map_err(OCError::serialize)?
                        .into(),
                )
                .map_err(OCError::closed)?),
        }
    }
}
impl RecvHandler<Action, RecvMessage> for WSHandler {
    async fn handle_recv(
        &self,
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
        &self,
        result: Result<ClosedReason, String>,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
    ) -> Result<(), crate::Error> {
        Ok(msg_tx
            .send(RecvMessage::Close(result))
            .map_err(OCError::closed)?)
    }
}

pub struct WSReCreate<R>
where
    R: IntoClientRequest + Unpin,
{
    req: R,
    access_token: Option<String>,
}
impl<R: IntoClientRequest + Unpin> WSReCreate<R> {
    pub fn new(req: R) -> Self {
        Self {
            req,
            access_token: None,
        }
    }
}

impl<R> Create for WSReCreate<R>
where
    R: IntoClientRequest + Unpin + Send,
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
        let (cmd_tx, msg_rx) = WSTask::create(ws, WSHandler).await;
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
