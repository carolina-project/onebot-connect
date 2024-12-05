use super::*;
use bytes::Bytes;
use hyper::{header::AUTHORIZATION, service::Service, StatusCode};
extern crate http as http_lib;

use onebot_connect_interface::{client::Connect, ConfigError};
use onebot_types::ob12::{action::Action, event::Event};
use parking_lot::Mutex;
use std::{future::Future, pin::Pin, sync::Arc};

type ActionsTx = oneshot::Sender<Vec<Action>>;
type ActionsRx = oneshot::Receiver<Vec<Action>>;
type Response = hyper::Response<Option<Bytes>>;

pub struct WebhookServer {
    /// Authorization header and `access_token` query param, choose one to use
    access_token: Arc<Option<(String, String)>>,
    /// Event transmitter channel, send event and its callback(actions)
    event_tx: mpsc::Sender<(Event, ActionsTx)>,
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

type Req = http_lib::Request<Bytes>;

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

pub struct WebhookConnect {
    access_token: Option<String>,
}
impl WebhookConnect {
    pub fn new() {}
}
impl Connect for WebhookConnect {
    type Error = crate::Error;

    type Message = ();

    type Provider = WebhookClientProvider;

    type Source = RxMessageSource;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        todo!()
    }

    fn with_authorization(self, access_token: impl AsRef<str>) -> Self {
        todo!()
    }
}

pub struct WebhookClient {
    action_tx: ActionsTx,
    actions: Arc<Mutex<Vec<Action>>>,
}
impl WebhookClient {
    pub fn new(action_tx: ActionsTx) -> Self {
        Self {
            action_tx,
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
        self.actions.lock().push(Action {
            action,
            echo: None,
            self_,
        });
        Ok(None)
    }

    async fn release(self) -> Result<(), OCError>
    where
        Self: Sized,
    {
        self.action_tx
            .send(std::mem::take(&mut self.actions.lock()))
            .unwrap();
        Ok(())
    }
}

pub struct WebhookClientProvider {
    action_tx: Option<ActionsTx>,
}
impl WebhookClientProvider {
    pub fn new() -> Self {
        Self { action_tx: None }
    }

    pub fn set_sender(&mut self, sender: ActionsTx) {
        self.action_tx = Some(sender);
    }
}
impl ClientProvider for WebhookClientProvider {
    type Output = WebhookClient;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        if let Some(tx) = self.action_tx.take() {
            Ok(WebhookClient::new(tx))
        } else {
            Err(OCError::not_supported("send action actively not supported"))
        }
    }
}
