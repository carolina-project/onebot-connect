use super::*;
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{body::Incoming, header::AUTHORIZATION, server::conn::http1, service::Service, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
extern crate http as http_lib;

use crate::Error as AllErr;

use onebot_connect_interface::{client::Connect, ConfigError};
use onebot_types::ob12::{action::Action, event::Event};
use parking_lot::Mutex;
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};

type ActionsTx = oneshot::Sender<Vec<Action>>;
type ActionsRx = oneshot::Receiver<Vec<Action>>;
type Response = hyper::Response<Option<Bytes>>;
type EventCallTx = mpsc::Sender<(Event, ActionsTx)>;

#[derive(Clone)]
pub struct WebhookServer {
    /// Authorization header and `access_token` query param, choose one to use
    access_token: Arc<Option<(String, String)>>,
    /// Event transmitter channel, send event and its callback(actions)
    event_tx: EventCallTx,
}

impl WebhookServer {
    pub fn new(event_tx: EventCallTx) -> Self {
        Self {
            access_token: Default::default(),
            event_tx,
        }
    }

    pub fn with_authorization(&mut self, access_token: impl Into<String>) {
        let access_token = access_token.into();
        self.access_token = Arc::new(Some((format!("Bearer {}", access_token), access_token)));
    }
}

#[inline]
fn mk_resp<'a>(status: hyper::StatusCode, data: Option<impl Into<Bytes>>) -> Response {
    hyper::Response::builder()
        .status(status)
        .body(data.map(Into::into))
        .unwrap()
}

#[derive(Debug, serde::Deserialize)]
struct ReqQuery<'a> {
    access_token: &'a str,
}

type Req = http_lib::Request<Incoming>;

impl Service<Req> for WebhookServer {
    type Response = Response;

    type Error = Response;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Req) -> Self::Future {
        let access_token = self.access_token.clone();
        let event_tx = self.event_tx.clone();
        Box::pin(async move {
            let req = hyper::Request::from(req);

            // check access token
            let mut passed = false;
            if let Some((header_token, query_token)) = access_token.as_ref() {
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

            let data = req.into_body().collect().await?;
            // parse event
            let event: Event = serde_json::from_slice(&req.into_body()).map_err(|e| {
                mk_resp(
                    StatusCode::BAD_REQUEST,
                    Some(format!("invalid data: {}", e)),
                )
            })?;

            let (tx, rx) = oneshot::channel();
            event_tx.send((event, tx)).await.unwrap();
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
        })
    }
}

type ActionsMap = FxHashMap<String, ActionsTx>;
pub struct WebhookConnect {
    access_token: Option<String>,
    addr: SocketAddr,
}
impl WebhookConnect {
    pub fn new(addr: impl Into<SocketAddr>) -> Self {
        Self {
            access_token: None,
            addr: addr.into(),
        }
    }

    async fn server_task(
        addr: SocketAddr,
        event_tx: EventCallTx,
        access_token: Option<impl Into<String>>,
    ) -> Result<(), AllErr> {
        let listener = TcpListener::bind(addr).await?;

        let mut serv = WebhookServer::new(event_tx);
        if let Some(access_token) = access_token {
            serv.with_authorization(access_token);
        }

        loop {
            let (tcp, _) = listener.accept().await?;
            let io = TokioIo::new(tcp);

            let serv_clone = serv.clone();
            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new().serve_connection(io, serv_clone).await {
                    println!("Failed to serve connection: {:?}", err);
                }
            });
        }
    }

    async fn manage_task(
        mut event_rx: mpsc::Receiver<(Event, ActionsTx)>,
        msg_tx: mpsc::Sender<RecvMessage>,
        mut cmd_rx: mpsc::Receiver<Command>,
    ) -> Result<(), crate::Error> {
        let mut actions_map = ActionsMap::default();

        loop {
            tokio::select! {
                result = event_rx.recv() => {
                    if let Some((event, actions_tx)) = result {
                        actions_map.insert(event.id.clone(), actions_tx);

                        msg_tx
                            .send(RecvMessage::Event(event))
                            .await
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

    type Provider = WebhookClientProvider;

    type Source = RxMessageSource;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        todo!()
    }

    fn with_authorization(self, access_token: impl Into<String>) -> Self {
        Self {
            access_token: Some(access_token.into()),
        }
    }
}

pub struct WebhookClient {
    event_id: String,
    cmd_tx: mpsc::Sender<Command>,
    actions: Arc<Mutex<Vec<ActionArgs>>>,
}
impl WebhookClient {
    pub fn new(event_id: impl Into<String>, cmd_tx: mpsc::Sender<Command>) -> Self {
        Self {
            event_id: event_id.into(),
            cmd_tx,
            actions: Default::default(),
        }
    }
}
impl Client for WebhookClient {
    fn response_supported(&self) -> bool {
        false
    }

    async fn send_action_impl(
        &self,
        action: onebot_types::ob12::action::ActionType,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<serde_value::Value>, OCError> {
        self.actions.lock().push(ActionArgs { action, self_ });
        Ok(None)
    }

    async fn release(self) -> Result<(), OCError>
    where
        Self: Sized,
    {
        let actions = std::mem::take(&mut *self.actions.lock());
        self.cmd_tx
            .send(Command::Respond(self.event_id, actions))
            .await
            .map_err(OCError::closed)
    }
}

pub struct WebhookClientProvider {
    event_id: Option<String>,
    cmd_tx: mpsc::Sender<Command>,
}
impl WebhookClientProvider {
    pub fn new(cmd_tx: mpsc::Sender<Command>) -> Self {
        Self {
            event_id: None,
            cmd_tx,
        }
    }

    pub fn set_event(&mut self, id: String) {
        self.event_id = Some(id);
    }
}
impl ClientProvider for WebhookClientProvider {
    type Output = WebhookClient;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        if let Some(id) = self.event_id.take() {
            Ok(WebhookClient::new(id, self.cmd_tx.clone()))
        } else {
            Err(OCError::not_supported("send action actively not supported"))
        }
    }
}
