use std::{
    collections::VecDeque, future::Future, net::SocketAddr, ops::Deref, sync::Arc, time::Duration,
};

use dashmap::DashMap;
use onebot_connect_interface::{
    imp::{Action as ImpAction, Create},
    ConfigError,
};
use onebot_types::{
    ob12::{
        self,
        action::{GetLatestEvents, RawAction, RespData, RespStatus, RetCode},
        event::RawEvent,
    },
    OBAction,
};
use parking_lot::Mutex;
use rand::{thread_rng, Rng};
use serde::{de::IntoDeserializer, Deserialize};
use tokio::sync::oneshot;

use crate::common::{http_s::*, *};

extern crate http as http_lib;

use super::*;
use crate::Error as AllErr;

type ActionResponder = oneshot::Sender<HttpResponse<RespData>>;
type RespMap = DashMap<ActionEcho, ActionResponder>;
type ActionRecv = (RawAction, ActionResponder);

struct HttpHandlerInner {
    resp_map: RespMap,
    impl_: HttpImpl,
}
#[derive(Clone)]
struct HttpHandler {
    inner: Arc<HttpHandlerInner>,
}
impl Deref for HttpHandler {
    type Target = HttpHandlerInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl HttpHandler {
    fn new(impl_: HttpImpl) -> Self {
        Self {
            inner: HttpHandlerInner {
                resp_map: Default::default(),
                impl_,
            }
            .into(),
        }
    }
}

impl CmdHandler<Command> for HttpHandler {
    async fn handle_cmd(&self, cmd: Command, state: ConnState) -> Result<(), crate::Error> {
        match cmd {
            Command::GetConfig(_, tx) => tx.send(None).map_err(|_| AllErr::ChannelClosed),
            Command::SetConfig((key, _), tx) => tx
                .send(Err(ConfigError::UnknownKey(key)))
                .map_err(|_| AllErr::ChannelClosed),
            Command::Close => {
                state.set_active(false);
                Ok(())
            }
            Command::Respond(echo, response) => {
                if let Some((echo, responder)) = self.resp_map.remove(&echo) {
                    let echo = match echo {
                        ActionEcho::Inner(_) => None,
                        ActionEcho::Outer(s) => Some(s),
                    };
                    let response = match response {
                        ActionResponse::Ok(data) => ob12::action::RespData {
                            status: RespStatus::Ok,
                            retcode: RetCode::Success,
                            data,
                            message: Default::default(),
                            echo,
                        },
                        ActionResponse::Error { retcode, message } => ob12::action::RespData {
                            status: RespStatus::Failed,
                            retcode,
                            data: serde_value::Value::Option(None),
                            message,
                            echo,
                        },
                    };
                    responder
                        .send(HttpResponse::Ok(response))
                        .map_err(|_| AllErr::ChannelClosed)
                } else {
                    log::error!("cannot find action: {:?}", echo);
                    Ok(())
                }
            }
            Command::Event(_) => {
                log::error!("send event actively not supported, please use `get_latest_events`");
                Ok(())
            }
        }
    }
}
impl RecvHandler<ActionRecv, RecvMessage> for HttpHandler {
    async fn handle_recv(
        &self,
        recv: ActionRecv,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        _state: crate::common::ConnState,
    ) -> Result<(), AllErr> {
        let (action, resp_tx) = recv;
        let RawAction {
            detail,
            echo,
            self_,
        } = action;

        let echo = match echo {
            Some(echo) => ActionEcho::Outer(echo),
            None => {
                let mut rand = thread_rng();
                loop {
                    let echo = ActionEcho::Inner(rand.gen());
                    if !self.resp_map.contains_key(&echo) {
                        break echo;
                    } else {
                        continue;
                    }
                }
            }
        };

        self.resp_map.insert(echo.clone(), resp_tx);

        if &detail.action == GetLatestEvents::ACTION.unwrap() {
            let params = GetLatestEvents::deserialize(detail.params.into_deserializer())?;
            let events = self
                .impl_
                .get_events(params.limit as _, Duration::from_secs(params.timeout as _))
                .await;
            let value = serde_value::to_value(events)?;
            self.impl_.respond(echo, ActionResponse::Ok(value)).await?;
            Ok(())
        } else {
            msg_tx
                .send(RecvMessage::Action(ImpAction {
                    detail,
                    echo,
                    self_,
                }))
                .map_err(|_| AllErr::ChannelClosed)
        }
    }
}
impl CloseHandler<RecvMessage> for HttpHandler {
    async fn handle_close(
        &self,
        result: Result<onebot_connect_interface::ClosedReason, String>,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
    ) -> Result<(), crate::Error> {
        Ok(msg_tx
            .send(RecvMessage::Close(result))
            .map_err(OCError::closed)?)
    }
}

pub struct HttpCreate {
    config: HttpConfig,
    addr: SocketAddr,
}

impl HttpCreate {
    pub fn new(addr: impl Into<SocketAddr>) -> Self {
        Self {
            config: Default::default(),
            addr: addr.into(),
        }
    }
}

impl Create for HttpCreate {
    type Error = OCError;
    type Source = RxMessageSource;
    type Provider = HttpImplProvider;
    type Message = ();

    async fn create(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let mut app_provider = HttpImplProvider::new(cmd_tx);

        HttpServerTask::create_with_channel(
            self.addr,
            HttpHandler::new(app_provider.provide()?),
            OB12HttpProc::new(self.config, parse_req),
            cmd_rx,
            msg_tx,
        )
        .await;
        Ok((RxMessageSource::new(msg_rx), app_provider, ()))
    }

    fn with_authorization(mut self, access_token: impl Into<String>) -> Self {
        let token = access_token.into();
        self.config.authorization = Some((format!("Bearer {}", token), token));
        self
    }
}

type SharedEventTx = oneshot::Sender<Arc<VecDeque<RawEvent>>>;

pub struct HttpImplInner {
    queue: Mutex<VecDeque<RawEvent>>,
    waiting_queue: Mutex<Vec<SharedEventTx>>,
    cmd_tx: CmdSender,
}

/// An shared OneBot implementation http server.
/// Events are stored in a queue, waiting for `get_latest_events` action.
#[derive(Clone)]
pub struct HttpImpl {
    inner: Arc<HttpImplInner>,
}
impl Deref for HttpImpl {
    type Target = HttpImplInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl HttpImpl {
    pub fn new(cmd_tx: CmdSender) -> Self {
        Self {
            inner: HttpImplInner {
                queue: Default::default(),
                cmd_tx,
                waiting_queue: Default::default(),
            }
            .into(),
        }
    }

    pub fn get_events(
        &self,
        limit: usize,
        timeout: Duration,
    ) -> impl Future<Output = Arc<VecDeque<RawEvent>>> + Send + '_ {
        async move {
            let mut queue = self.queue.lock();
            if queue.len() > 0 {
                if limit == 0 {
                    let events = std::mem::take(&mut *queue);
                    events.into()
                } else {
                    Arc::new(queue.drain(..limit).collect())
                }
            } else {
                let (tx, rx) = oneshot::channel();

                self.waiting_queue.lock().push(tx);
                tokio::select! {
                    _ = tokio::time::sleep(timeout) => {
                        Default::default()
                    },
                    event = rx => {
                        event.unwrap()
                    }
                }
            }
        }
    }
}

impl OBImpl for HttpImpl {
    fn send_event_impl(
        &self,
        event: RawEvent,
    ) -> impl std::future::Future<Output = Result<(), OCError>> + Send + '_ {
        async move {
            self.queue.lock().push_back(event);
            Ok(())
        }
    }

    async fn respond(&self, echo: ActionEcho, data: ActionResponse) -> Result<(), OCError> {
        self.cmd_tx
            .send(Command::Respond(echo, data))
            .map_err(OCError::closed)
    }

    async fn close(&self) -> Result<(), OCError> {
        Ok(self.inner.cmd_tx.send(Command::Close)?)
    }
}

pub struct HttpImplProvider {
    http_impl: HttpImpl,
}

impl HttpImplProvider {
    pub fn new(cmd_tx: CmdSender) -> Self {
        Self {
            http_impl: HttpImpl::new(cmd_tx),
        }
    }
}

impl OBImplProvider for HttpImplProvider {
    type Output = HttpImpl;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        Ok(self.http_impl.clone())
    }
}
