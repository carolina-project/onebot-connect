use std::{collections::VecDeque, convert::Infallible, net::SocketAddr, sync::Arc};

use bytes::Bytes;
use fxhash::FxHashMap;
use http_body_util::{BodyExt, Full};
use hyper::{header::AUTHORIZATION, server::conn::http1, service::service_fn, StatusCode};
use hyper_util::rt::TokioIo;
use onebot_connect_interface::{imp::Action as ImpAction, ConfigError};
use onebot_types::ob12::{
    self,
    action::{RespData, RespStatus, RetCode},
};
use parking_lot::{Mutex, RwLock};
use rand::{thread_rng, Rng};
use serde_value::Value;
use tokio::{net::TcpListener, sync::oneshot};

use crate::{
    common::http_s::{mk_resp, Req, ReqQuery},
    Authorization,
};

extern crate http as http_lib;

use super::*;
use crate::Error as AllErr;

type EventQueue = Arc<Mutex<VecDeque<Event>>>;

#[derive(Debug, Clone, Default)]
pub struct HttpConfig {
    authorization: Authorization,
}

type HttpConfShared = Arc<RwLock<HttpConfig>>;

type ActionResponder = oneshot::Sender<RespData>;
type ActionRespTx = mpsc::UnboundedSender<(ImpAction, ActionResponder)>;
type ActionRespRx = mpsc::UnboundedReceiver<(ImpAction, ActionResponder)>;
type Response = hyper::Response<Full<Bytes>>;

#[derive(Clone)]
pub struct HttpServer {
    config: HttpConfShared,
    /// Event transmitter channel, send event and its callback(actions)
    action_tx: ActionRespTx,
}

impl HttpServer {
    pub fn new(action_tx: ActionRespTx, config: HttpConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            action_tx,
        }
    }

    async fn handle_req(self, req: Req) -> Result<Response, Response> {
        let Self { config, action_tx } = self;
        // check access token
        let mut passed = false;
        if let Some((header_token, query_token)) = &config.read().authorization {
            if let Some(header) = req.headers().get(AUTHORIZATION) {
                if header == header_token {
                    passed = true;
                }
            } else if let Some(query) = req.uri().query() {
                let params: ReqQuery = serde_qs::from_str(query).map_err(|e| {
                    mk_resp(
                        StatusCode::BAD_REQUEST,
                        Some(format!("invalid request query: {}", e)),
                    )
                })?;
                if params.access_token == query_token {
                    passed = true;
                }
            }
        }

        if !passed {
            return Err(mk_resp(StatusCode::UNAUTHORIZED, Some("Unauthorized")));
        }

        let data = req.into_body().collect().await.map_err(|e| {
            mk_resp(
                StatusCode::INTERNAL_SERVER_ERROR,
                Some(format!("http body err: {e}")),
            )
        })?;

        // parse action
        let action: Action = serde_json::from_slice(&data.to_bytes()).map_err(|e| {
            mk_resp(
                StatusCode::BAD_REQUEST,
                Some(format!("invalid data: {}", e)),
            )
        })?;

        let (tx, rx) = oneshot::channel();
        action_tx.send((action, tx)).unwrap();
        // acquire response
        let resp = serde_json::to_vec(&rx.await.unwrap()).map_err(|e| {
            mk_resp(
                StatusCode::INTERNAL_SERVER_ERROR,
                Some(format!("error while serializing response: {}", e)),
            )
        })?;

        Ok(mk_resp(StatusCode::OK, Some(resp)))
    }
}

pub struct HttpCreate {
    config: HttpConfig,
    addr: SocketAddr,
}

type RespMap = FxHashMap<ActionEcho, oneshot::Sender<RespData>>;

impl HttpCreate {
    pub fn new(addr: impl Into<SocketAddr>) -> Self {
        Self {
            config: Default::default(),
            addr: addr.into(),
        }
    }

    async fn server_task(
        addr: SocketAddr,
        resp_tx: ActionRespTx,
        config: HttpConfig,
    ) -> Result<(), AllErr> {
        async fn service(
            serv: HttpServer,
            req: Req,
        ) -> Result<hyper::Response<Full<Bytes>>, Infallible> {
            Ok(serv.handle_req(req).await.unwrap_or_else(|e| e))
        }

        let listener = TcpListener::bind(addr).await?;
        let serv = HttpServer::new(resp_tx, config);
        loop {
            let (tcp, _) = listener.accept().await?;
            let io = TokioIo::new(tcp);

            let serv = serv.clone();
            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(io, service_fn(|r| service(serv.clone(), r)))
                    .await
                {
                    println!("Failed to serve connection: {:?}", err);
                }
            });
        }
    }

    async fn manage_task(
        mut resp_rx: ActionRespRx,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    ) -> Result<(), crate::Error> {
        let mut resp_map = RespMap::default();

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

        loop {
            tokio::select! {
                result = resp_rx.recv() => {
                    if let Some((action, resp_tx)) = result {
                        resp_map.insert(random_echo(&resp_map), resp_tx);

                        msg_tx
                            .send(RecvMessage::Action(action))
                            .map_err(|_| AllErr::ChannelClosed)?
                    } else {
                        break
                    }
                }
                cmd = cmd_rx.recv() => {
                    if let Some(cmd) = cmd {
                        Self::handle_cmd(cmd, &mut resp_map).await?;
                    } else {
                        break
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_cmd(cmd: Command, actions_map: &mut RespMap) -> Result<(), AllErr> {
        match cmd {
            Command::GetConfig(_, tx) => tx.send(None).map_err(|_| AllErr::ChannelClosed),
            Command::SetConfig((key, _), tx) => tx
                .send(Err(ConfigError::UnknownKey(key)))
                .map_err(|_| AllErr::ChannelClosed),
            Command::Close(_) => todo!(),
            Command::Respond(echo, response) => {
                if let Some(responder) = actions_map.remove(&echo) {
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
                            data: Value::Map(Default::default()),
                            message,
                            echo,
                        },
                    };
                    responder.send(response).map_err(|_| AllErr::ChannelClosed)
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

impl Create for HttpCreate {
    type Error = OCError;
    type Source = RxMessageSource;
    type Provider = HttpImplProvider;
    type Message = ();

    async fn create(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (resp_tx, resp_rx) = mpsc::unbounded_channel();

        tokio::spawn(Self::manage_task(resp_rx, msg_tx.clone(), cmd_rx));
        tokio::spawn(Self::server_task(self.addr, resp_tx, self.config));

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

pub struct HttpImpl {
    queue: EventQueue,
    cmd_tx: CmdSender,
}
impl HttpImpl {
    pub fn new(queue: EventQueue, cmd_tx: CmdSender) -> Self {
        Self { queue, cmd_tx }
    }

    pub fn drain_events(&mut self) -> VecDeque<Event> {
        std::mem::take(&mut self.queue.lock())
    }
}

impl Impl for HttpImpl {
    fn send_event_impl(
        &self,
        event: Event,
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
    queue: EventQueue,
    cmd_tx: CmdSender,
}

impl HttpImplProvider {
    pub fn new(cmd_tx: CmdSender) -> Self {
        Self {
            queue: Default::default(),
            cmd_tx,
        }
    }
}

impl ImplProvider for HttpImplProvider {
    type Output = HttpImpl;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        Ok(HttpImpl::new(self.queue.clone(), self.cmd_tx.clone()))
    }
}
