use super::*;
extern crate http as http_lib;
use crate::{
    common::{http_s::*, *},
    Error as AllErr,
};

use onebot_connect_interface::{app::Connect, ClosedReason, ConfigError};
use onebot_types::ob12::{
    action::{ActionDetail, RawAction, RespData},
    event::RawEvent,
};
use parking_lot::Mutex;
use std::{net::SocketAddr, sync::Arc};

type EventResponder = oneshot::Sender<HttpResponse<Vec<RawAction>>>;

type ActionsMap = DashMap<String, EventResponder>;
#[derive(Default, Clone)]
struct WHandler {
    actions_map: Arc<ActionsMap>,
}

impl CmdHandler<Command> for WHandler {
    async fn handle_cmd(
        &self,
        cmd: Command,
        state: crate::common::ConnState,
    ) -> Result<(), crate::Error> {
        match cmd {
            Command::Action(_, tx) => tx
                .send(Err(OCError::not_supported("send action actively").into()))
                .map_err(|_| AllErr::ChannelClosed),
            Command::Respond(id, actions) => {
                if let Some((_, tx)) = self.actions_map.remove(&id) {
                    let actions = actions
                        .into_iter()
                        .map(|ActionArgs { action, self_ }| RawAction {
                            detail: action,
                            echo: None,
                            self_,
                        })
                        .collect();
                    tx.send(HttpResponse::Ok(actions))
                        .map_err(|_| AllErr::ChannelClosed)?;
                } else {
                    log::error!("cannot find event: {}", id);
                }

                Ok(())
            }
            Command::GetConfig(_, tx) => tx.send(None).map_err(|_| AllErr::ChannelClosed),
            Command::SetConfig((key, _), tx) => tx
                .send(Err(ConfigError::UnknownKey(key)))
                .map_err(|_| AllErr::ChannelClosed),
            Command::Close => {
                state.set_active(false);
                Ok(())
            }
        }
    }
}

impl RecvHandler<(RawEvent, EventResponder), RecvMessage> for WHandler {
    async fn handle_recv(
        &self,
        recv: (RawEvent, EventResponder),
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        _state: crate::common::ConnState,
    ) -> Result<(), crate::Error> {
        let (event, actions_tx) = recv;
        self.actions_map.insert(event.id.clone(), actions_tx);

        msg_tx
            .send(RecvMessage::Event(event))
            .map_err(|_| AllErr::ChannelClosed)
    }
}

impl CloseHandler<RecvMessage> for WHandler {
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

pub struct WebhookConnect {
    config: HttpConfig,
    addr: SocketAddr,
}
impl WebhookConnect {
    pub fn new(addr: impl Into<SocketAddr>) -> Self {
        Self {
            config: Default::default(),
            addr: addr.into(),
        }
    }
}
impl Connect for WebhookConnect {
    type Error = crate::Error;
    type Message = ();
    type Provider = WebhookAppProvider;
    type Source = RxMessageSource;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let (cmd_tx, msg_rx) = HttpServerTask::create(
            self.addr,
            WHandler::default(),
            OB12HttpProc::new(self.config, parse_req),
        )
        .await?;

        Ok((
            RxMessageSource::new(msg_rx),
            WebhookAppProvider::new(cmd_tx),
            (),
        ))
    }

    fn with_authorization(mut self, access_token: impl Into<String>) -> Self {
        let token = access_token.into();
        self.config.authorization = Some((format!("Bearer {}", token), token));
        self
    }
}

pub struct WebhookAppInner {
    event_id: String,
    actions: Mutex<Vec<ActionArgs>>,
    cmd_tx: mpsc::UnboundedSender<Command>,
}
pub struct WebhookApp {
    is_owner: bool,
    inner: Arc<WebhookAppInner>,
}
impl WebhookApp {
    pub(crate) fn new(event_id: impl Into<String>, cmd_tx: mpsc::UnboundedSender<Command>) -> Self {
        Self {
            is_owner: true,
            inner: WebhookAppInner {
                event_id: event_id.into(),
                actions: Default::default(),
                cmd_tx,
            }
            .into(),
        }
    }
}
impl OBApp for WebhookApp {
    fn response_supported(&self) -> bool {
        false
    }

    async fn send_action_impl(
        &self,
        action: ActionDetail,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<RespData>, OCError> {
        self.inner.actions.lock().push(ActionArgs { action, self_ });
        Ok(None)
    }

    fn clone_app(&self) -> Self {
        Self {
            is_owner: false,
            inner: self.inner.clone(),
        }
    }

    async fn close(&self) -> Result<(), OCError> {
        Ok(self.inner.cmd_tx.send(Command::Close)?)
    }

    async fn release(&mut self) -> Result<(), OCError> {
        if self.is_owner {
            let actions = std::mem::take(&mut *self.inner.actions.lock());
            self.inner
                .cmd_tx
                .send(Command::Respond(self.inner.event_id.clone(), actions))
                .map_err(OCError::closed)
        } else {
            Err(OCError::not_supported(
                "`release` not supported for non-owner",
            ))
        }
    }
}

pub struct WebhookAppProvider {
    event_id: Option<String>,
    cmd_tx: mpsc::UnboundedSender<Command>,
}
impl WebhookAppProvider {
    pub fn new(cmd_tx: mpsc::UnboundedSender<Command>) -> Self {
        Self {
            event_id: None,
            cmd_tx,
        }
    }
}
impl OBAppProvider for WebhookAppProvider {
    type Output = WebhookApp;

    fn use_event_context(&self) -> bool {
        true
    }

    fn set_event_context(&mut self, event: &RawEvent) {
        self.event_id = Some(event.id.to_owned())
    }

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        if let Some(id) = self.event_id.take() {
            Ok(WebhookApp::new(id, self.cmd_tx.clone()))
        } else {
            Err(OCError::not_supported("send action actively not supported"))
        }
    }
}
