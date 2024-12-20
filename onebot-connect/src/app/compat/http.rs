use onebot_connect_interface::app::OBApp;
use onebot_connect_interface::Error as OCErr;
use std::{future::Future, net::SocketAddr, ops::Deref, sync::Arc};

use super::*;
use crate::{
    app::{HttpInner, HttpInnerShared, RxMessageSource},
    common::{http_s::HttpResponse, *},
};
use ::http::HeaderValue;
use data::AppData;
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use http_s::{mk_resp, HttpReqProc, HttpServerTask, Req, Response};
use hyper::{header::AUTHORIZATION, HeaderMap, StatusCode};
use onebot_connect_interface::{
    app::{Command, Connect, OBAppProvider, RecvMessage},
    ClosedReason, ConfigError,
};
use onebot_types::{
    ob11::{action as ob11a, RawEvent as OB11Event},
    ob12::{
        self,
        action::{self as ob12a, RespData, RespError, RetCode},
    },
};
use parking_lot::RwLock;
use sha1::Sha1;
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot,
};

#[derive(Debug)]
pub struct OB11PostConf {
    secret: Option<String>,
}
type OB11HttpConfShared = Arc<RwLock<OB11PostConf>>;

pub struct OB11HttpProc<B, F, R>
where
    F: (Fn(bytes::Bytes) -> R) + Clone + Send + Sync,
    R: Future<Output = Result<B, Response>> + Send + 'static,
{
    conf: OB11HttpConfShared,
    proc: F,
}

impl<B, F, R> Clone for OB11HttpProc<B, F, R>
where
    F: (Fn(bytes::Bytes) -> R) + Clone + Send + Sync,
    R: Future<Output = Result<B, Response>> + Send,
{
    fn clone(&self) -> Self {
        Self {
            conf: self.conf.clone(),
            proc: self.proc.clone(),
        }
    }
}

impl<B, F, R> HttpReqProc for OB11HttpProc<B, F, R>
where
    F: (Fn(bytes::Bytes) -> R) + Clone + Send + Sync + 'static,
    R: Future<Output = Result<B, Response>> + Send,
    B: 'static,
{
    type Output = B;

    async fn process_request(&self, req: Req) -> Result<Self::Output, Response> {
        let read_body = |req: Req| async move {
            Ok(req
                .collect()
                .await
                .map_err(|e| {
                    log::debug!("error while collecting data: {}", e);
                    mk_resp(StatusCode::BAD_REQUEST, None::<String>)
                })?
                .to_bytes())
        };

        let body = if self.conf.read().secret.is_some() {
            // check signature
            let sig = req
                .headers()
                .get("X-Signature")
                .ok_or_else(|| mk_resp(StatusCode::BAD_REQUEST, Some("missing signature")))?
                .to_str()
                .map_err(|_| mk_resp(StatusCode::BAD_REQUEST, Some("invalid signature")))?
                .to_owned();

            type HmacSha1 = Hmac<Sha1>;
            let mut hash =
                HmacSha1::new_from_slice(self.conf.read().secret.as_ref().unwrap().as_bytes())
                    .map_err(|e| {
                        log::error!("hmac err: {:?}", e);
                        mk_resp(StatusCode::INTERNAL_SERVER_ERROR, None::<String>)
                    })?;

            let body = read_body(req).await?;
            hash.update(&body);
            let result = hex::encode(hash.finalize().into_bytes());

            if sig[4..] != result {
                return Err(mk_resp(
                    StatusCode::UNAUTHORIZED,
                    Some("signature check failed"),
                ));
            }
            body
        } else {
            read_body(req).await?
        };

        (self.proc)(body).await
    }
}

pub struct HttpConnect<A: Into<SocketAddr>> {
    self_addr: A,
    impl_url: String,
    secret: Option<String>,
    auth: Option<String>,
}

impl<A: Into<SocketAddr>> HttpConnect<A> {
    pub fn new(self_addr: A, impl_url: impl Into<String>) -> Self {
        Self {
            self_addr,
            impl_url: impl_url.into(),
            secret: None,
            auth: None,
        }
    }

    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = Some(secret.into());
        self
    }

    #[allow(unused)]
    async fn cmd_bridge(
        mut src: UnboundedReceiver<Command>,
        dest: UnboundedSender<HttpPostCommand>,
    ) {
        while let Some(cmd) = src.recv().await {
            match cmd {
                Command::Action(_, tx) => tx
                    .send(Err(OCErr::not_supported("http post, send action")))
                    .unwrap(),
                Command::Respond(_, _) => {
                    log::error!("unexpected cmd received: respond not supported")
                }
                Command::GetConfig(_, tx) => tx.send(None).unwrap(),
                Command::SetConfig((k, _), tx) => tx.send(Err(ConfigError::UnknownKey(k))).unwrap(),
                Command::Close => dest.send(HttpPostCommand::Close).unwrap(),
            }
        }
    }
}

impl<A: Into<SocketAddr>> Connect for HttpConnect<A> {
    type Source = RxMessageSource;
    type Error = crate::Error;
    type Message = ();
    type Provider = HttpAppProvider;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let conf = OB11PostConf {
            secret: self.secret,
        };
        let data = AppData::default();
        let mut app_provider = HttpAppProvider::new(
            self.impl_url,
            self.auth
                .map(|r| HeaderValue::from_str(&format!("Bearer {}", r)))
                .transpose()?,
            data.clone(),
        )?;
        let handler = HttpPostHandler {
            inner: HttpPostHandlerInner {
                data,
                app: app_provider.provide()?,
            }
            .into(),
        };
        let (_cmd_tx, msg_rx) = HttpServerTask::create(
            self.self_addr,
            handler,
            OB11HttpProc {
                conf: RwLock::new(conf).into(),
                proc: |data| async move {
                    match serde_json::from_slice(&data) {
                        Ok(action) => Ok(action),
                        Err(e) => {
                            log::debug!("error while deserializing data: {}", e);
                            Err(mk_resp(StatusCode::BAD_REQUEST, None::<String>))
                        }
                    }
                },
            },
        )
        .await;

        Ok((RxMessageSource::new(msg_rx), app_provider, ()))
    }

    fn with_authorization(mut self, access_token: impl Into<String>) -> Self {
        self.auth = Some(access_token.into());
        self
    }
}

#[derive(Clone)]
pub struct OB11HttpApp {
    inner: HttpInnerShared,
    data: AppData,
}

impl OBApp for OB11HttpApp {
    async fn send_action_impl(
        &self,
        action: ob12a::ActionDetail,
        _self_: Option<ob12::BotSelf>,
    ) -> Result<Option<RespData>, OCErr> {
        match self.data.convert_action(action, self).await? {
            data::ActionConverted::Send(ob11a::ActionDetail { action, params }) => {
                let inner = &self.inner;
                let response = self
                    .inner
                    .http
                    .post(format!("{}/{}", inner.url, action))
                    .json(&params)
                    .send()
                    .await
                    .map_err(OCErr::other)?;

                let data: ob11a::RespData = match response.status() {
                    StatusCode::OK => response
                        .json()
                        .await
                        .map_err(|e| OCErr::other(format!("json parse error: {:?}", e)))?,
                    StatusCode::NOT_FOUND => {
                        return Err(RespError::new(
                            RetCode::UnsupportedAction,
                            format!("unsupported action: `{action}`"),
                        )
                        .into())
                    }
                    StatusCode::BAD_REQUEST => {
                        return Err(RespError::new(RetCode::BadParam, "post body error").into())
                    }
                    code => {
                        return Err(RespError::new(
                            RetCode::OtherError(code.as_u16() as u32),
                            "http error",
                        )
                        .into())
                    }
                };

                if !data.is_success() {
                    return Err(RespError {
                        retcode: RetCode::OtherError(data.retcode as u32),
                        message: "OneBot 11 error".into(),
                        echo: data.echo,
                    }
                    .into());
                }

                let data = self.data.convert_resp_data(&action, data.data).await?;

                Ok(Some(RespData::success(data, None)))
            }
            data::ActionConverted::Respond(resp) => Ok(Some(resp.into_resp_data(None))),
        }
    }

    async fn close(&self) -> Result<(), OCErr> {
        Ok(())
    }

    fn clone_app(&self) -> Self
    where
        Self: 'static,
    {
        self.clone()
    }
}

pub struct HttpAppProvider {
    app: OB11HttpApp,
}

impl HttpAppProvider {
    pub fn new(
        url: impl Into<String>,
        auth_header: Option<HeaderValue>,
        data: AppData,
    ) -> Result<Self, reqwest::Error> {
        let mut headers = HeaderMap::default();
        if let Some(header) = auth_header {
            headers.insert(AUTHORIZATION, header);
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;
        Ok(Self {
            app: OB11HttpApp {
                inner: HttpInner {
                    url: url.into(),
                    http,
                }
                .into(),
                data,
            },
        })
    }
}

impl OBAppProvider for HttpAppProvider {
    type Output = OB11HttpApp;

    fn provide(&mut self) -> Result<Self::Output, OCErr> {
        Ok(self.app.clone())
    }
}

// OneBot 11 HTTP POST side
pub enum HttpPostRecv {
    Event(OB11Event),
    Close(Result<ClosedReason, String>),
}

type HttpPostResponder = oneshot::Sender<HttpResponse<()>>;

pub enum HttpPostCommand {
    Close,
}

struct HttpPostHandlerInner {
    data: AppData,
    app: OB11HttpApp,
}

#[derive(Clone)]
struct HttpPostHandler {
    inner: Arc<HttpPostHandlerInner>,
}
impl Deref for HttpPostHandler {
    type Target = HttpPostHandlerInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl RecvHandler<(OB11Event, HttpPostResponder), RecvMessage> for HttpPostHandler {
    async fn handle_recv(
        &self,
        recv: (OB11Event, HttpPostResponder),
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        _state: ConnState,
    ) -> Result<(), crate::Error> {
        let (event, respond) = recv;
        msg_tx.send(RecvMessage::Event(
            self.data.convert_event(event, &msg_tx, &self.app).await?,
        ))?;
        // ignore quick action
        respond
            .send(HttpResponse::Other {
                status: StatusCode::NO_CONTENT,
            })
            .map_err(|_| crate::Error::ChannelClosed)?;
        Ok(())
    }
}

impl CmdHandler<HttpPostCommand> for HttpPostHandler {
    async fn handle_cmd(&self, cmd: HttpPostCommand, state: ConnState) -> Result<(), crate::Error> {
        match cmd {
            HttpPostCommand::Close => state.set_active(false),
        }

        Ok(())
    }
}

impl CloseHandler<RecvMessage> for HttpPostHandler {
    async fn handle_close(
        &self,
        result: Result<ClosedReason, String>,
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
    ) -> Result<(), crate::Error> {
        msg_tx.send(RecvMessage::Close(result))?;
        Ok(())
    }
}
