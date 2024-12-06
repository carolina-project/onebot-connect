use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{header::AUTHORIZATION, server::conn::http1, service::service_fn, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
extern crate http as http_lib;

use crate::{
    common::http::{mk_resp, Req, ReqQuery, Response},
    Authorization, Error as AllErr,
};

use onebot_connect_interface::{app::Connect, ConfigError};
use onebot_types::ob12::{action::Action, event::Event};
use parking_lot::{Mutex, RwLock};
use std::{convert::Infallible, net::SocketAddr, sync::Arc};

type EventResponder = oneshot::Sender<Vec<Action>>;
type EventCallTx = mpsc::UnboundedSender<(Event, EventResponder)>;

/// Authorization header and `access_token` query param, choose one to use
type WebhookConfShared = Arc<RwLock<WebhookConfig>>;

#[derive(Debug, Clone, Default)]
pub struct WebhookConfig {
    authorization: Authorization,
}

#[derive(Clone)]
pub struct WebhookServer {
    config: WebhookConfShared,
    /// Event transmitter channel, send event and its callback(actions)
    event_tx: EventCallTx,
}

impl WebhookServer {
    pub fn new(event_tx: EventCallTx, config: WebhookConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            event_tx,
        }
    }

    async fn handle_req(self, req: Req) -> Result<Response, Response> {
        let Self { config, event_tx } = self;
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

        // parse event
        let event: Event = serde_json::from_slice(&data.to_bytes()).map_err(|e| {
            mk_resp(
                StatusCode::BAD_REQUEST,
                Some(format!("invalid data: {}", e)),
            )
        })?;

        let (tx, rx) = oneshot::channel();
        event_tx.send((event, tx)).unwrap();
        // acquire response
        let actions = rx.await.unwrap();
        if actions.len() > 0 {
            let json = serde_json::to_vec(&actions).map_err(|e| {
                mk_resp(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Some(format!("error while serializing actions: {}", e)),
                )
            })?;
            Ok(mk_resp(StatusCode::OK, Some(json)))
        } else {
            Ok(mk_resp(StatusCode::NO_CONTENT, None::<Bytes>))
        }
    }
}

type ActionsMap = FxHashMap<String, EventResponder>;
pub struct WebhookConnect {
    config: WebhookConfig,
    addr: SocketAddr,
}
impl WebhookConnect {
    pub fn new(addr: impl Into<SocketAddr>) -> Self {
        Self {
            config: Default::default(),
            addr: addr.into(),
        }
    }

    async fn server_task(
        addr: SocketAddr,
        event_tx: EventCallTx,
        config: WebhookConfig,
    ) -> Result<(), AllErr> {
        async fn service(
            serv: WebhookServer,
            req: Req,
        ) -> Result<hyper::Response<Full<Bytes>>, Infallible> {
            Ok(serv.handle_req(req).await.unwrap_or_else(|e| e))
        }

        let listener = TcpListener::bind(addr).await?;
        let serv = WebhookServer::new(event_tx, config);
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
        mut event_rx: mpsc::UnboundedReceiver<(Event, EventResponder)>,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    ) -> Result<(), crate::Error> {
        let mut actions_map = ActionsMap::default();
        loop {
            tokio::select! {
                result = event_rx.recv() => {
                    if let Some((event, actions_tx)) = result {
                        actions_map.insert(event.id.clone(), actions_tx);

                        msg_tx
                            .send(RecvMessage::Event(event))
                            .map_err(|_| AllErr::ChannelClosed)?
                    } else {
                        break
                    }
                }
                cmd = cmd_rx.recv() => {
                    if let Some(cmd) = cmd {
                        Self::handle_cmd(cmd, &mut actions_map).await?;
                    } else {
                        break
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_cmd(cmd: Command, actions_map: &mut ActionsMap) -> Result<(), AllErr> {
        match cmd {
            Command::Action(_, tx) => tx
                .send(Err(OCError::not_supported("send action actively").into()))
                .map_err(|_| AllErr::ChannelClosed),
            Command::Respond(id, actions) => {
                if let Some(tx) = actions_map.remove(&id) {
                    tx.send(
                        actions
                            .into_iter()
                            .map(|ActionArgs { action, self_ }| Action {
                                action,
                                echo: None,
                                self_,
                            })
                            .collect(),
                    )
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
            Command::Close(_) => todo!(),
        }
    }
}
impl Connect for WebhookConnect {
    type Error = crate::Error;

    type Message = ();

    type Provider = WebhookAppProvider;

    type Source = RxMessageSource;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        tokio::spawn(Self::manage_task(event_rx, msg_tx.clone(), cmd_rx));
        tokio::spawn(Self::server_task(self.addr, event_tx, self.config));

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
}

pub struct WebhookApp {
    is_owner: bool,
    inner: Arc<WebhookAppInner>,
    cmd_tx: mpsc::UnboundedSender<Command>,
}
impl WebhookApp {
    pub fn new(event_id: impl Into<String>, cmd_tx: mpsc::UnboundedSender<Command>) -> Self {
        Self {
            is_owner: true,
            inner: WebhookAppInner {
                event_id: event_id.into(),
                actions: Default::default(),
            }
            .into(),
            cmd_tx,
        }
    }
}
impl App for WebhookApp {
    fn response_supported(&self) -> bool {
        false
    }

    async fn send_action_impl(
        &self,
        action: onebot_types::ob12::action::ActionType,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<serde_value::Value>, OCError> {
        self.inner.actions.lock().push(ActionArgs { action, self_ });
        Ok(None)
    }

    fn clone_app(&self) -> Self {
        Self {
            is_owner: false,
            inner: self.inner.clone(),
            cmd_tx: self.cmd_tx.clone(),
        }
    }

    async fn release(&mut self) -> Result<(), OCError> {
        if self.is_owner {
            let actions = std::mem::take(&mut *self.inner.actions.lock());
            self.cmd_tx
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

    pub fn set_event(&mut self, id: String) {
        self.event_id = Some(id);
    }
}
impl AppProvider for WebhookAppProvider {
    type Output = WebhookApp;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        if let Some(id) = self.event_id.take() {
            Ok(WebhookApp::new(id, self.cmd_tx.clone()))
        } else {
            Err(OCError::not_supported("send action actively not supported"))
        }
    }
}