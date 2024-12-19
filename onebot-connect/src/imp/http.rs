use std::{collections::VecDeque, net::SocketAddr, ops::Deref, sync::Arc, time::Duration};

use fxhash::FxHashMap;
use onebot_connect_interface::{imp::Action as ImpAction, ConfigError};
use onebot_types::ob12::{
    self,
    action::{RawAction, RespData, RespStatus, RetCode},
    event::RawEvent,
};
use parking_lot::Mutex;
use rand::{thread_rng, Rng};
use tokio::sync::oneshot;

use crate::common::{http_s::*, *};

extern crate http as http_lib;

use super::*;
use crate::Error as AllErr;

type EventQueue = Mutex<VecDeque<RawEvent>>;

type ActionResponder = oneshot::Sender<HttpResponse<RespData>>;
type RespMap = FxHashMap<ActionEcho, ActionResponder>;
type ActionRecv = (RawAction, ActionResponder);

#[derive(Default)]
struct HttpHandler {
    resp_map: RespMap,
}
impl CmdHandler<Command> for HttpHandler {
    async fn handle_cmd(&mut self, cmd: Command, state: ConnState) -> Result<(), crate::Error> {
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
                if let Some(responder) = self.resp_map.remove(&echo) {
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
        &mut self,
        recv: ActionRecv,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        _state: crate::common::ConnState,
    ) -> Result<(), crate::Error> {
        fn random_echo(map: &RespMap) -> ActionEcho {
            let mut rand = thread_rng();
            loop {
                let echo = ActionEcho::Inner(rand.gen());
                if !map.contains_key(&echo) {
                    break echo;
                } else {
                    continue;
                }
            }
        }
        let (action, resp_tx) = recv;
        let echo = match action.echo {
            Some(echo) => ActionEcho::Outer(echo),
            None => random_echo(&self.resp_map),
        };
        self.resp_map.insert(echo.clone(), resp_tx);

        msg_tx
            .send(RecvMessage::Action(ImpAction {
                action: action.action,
                echo,
                self_: action.self_,
            }))
            .map_err(|_| AllErr::ChannelClosed)
    }
}
impl CloseHandler<RecvMessage> for HttpHandler {
    async fn handle_close(
        &mut self,
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
        let (cmd_tx, msg_rx) = HttpServerTask::create(
            self.addr,
            HttpHandler::default(),
            OB12HttpProc::new(self.config, parse_req),
        )
        .await;

        Ok((
            RxMessageSource::new(msg_rx),
            HttpImplProvider::new(cmd_tx),
            (),
        ))
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

    pub async fn get_events(&mut self, limit: usize, timeout: Duration) -> Arc<VecDeque<RawEvent>> {
        let mut queue = self.queue.lock();
        if queue.len() > 0 {
            if limit == 0 {
                Arc::new(std::mem::take(&mut queue))
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
