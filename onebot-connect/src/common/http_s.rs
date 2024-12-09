use std::{convert::Infallible, marker::PhantomData, net::SocketAddr, sync::Arc};

use bytes::Bytes;
use http::{
    header::{AUTHORIZATION, CONTENT_TYPE},
    StatusCode,
};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use onebot_connect_interface::ClosedReason;
use parking_lot::RwLock;
use serde::{de::DeserializeOwned, Serialize};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};

use crate::Error as AllErr;

use super::*;

/// auth header and query param
type Authorization = Option<(String, String)>;
pub(crate) type Response = hyper::Response<Full<Bytes>>;
pub(crate) type Req = http::Request<Incoming>;

pub(crate) fn mk_resp(status: hyper::StatusCode, data: Option<impl Into<Bytes>>) -> Response {
    hyper::Response::builder()
        .status(status)
        .body(data.map(|b| Full::new(b.into())).unwrap_or_default())
        .unwrap()
}

pub(crate) async fn parse_req<B>(req: Req) -> Result<B, Response>
where
    B: DeserializeOwned + Send + 'static,
{
    let bytes = req.into_body().collect().await.map_err(|e| {
        log::debug!("error while collecting data: {}", e);
        mk_resp(StatusCode::BAD_REQUEST, None::<String>)
    })?;
    match serde_json::from_slice::<B>(&bytes.to_bytes()) {
        Ok(action) => Ok(action),
        Err(e) => {
            log::debug!("error while deserializing data: {}", e);
            Err(mk_resp(StatusCode::BAD_REQUEST, None::<String>))
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ReqQuery<'a> {
    pub access_token: &'a str,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HttpConfig {
    pub authorization: Authorization,
}

type HttpConfShared = Arc<RwLock<HttpConfig>>;

pub(crate) type HttpResponder<B, R> = (B, oneshot::Sender<HttpResponse<R>>);

pub enum HttpResponse<R>
where
    R: Serialize + Send + 'static,
{
    Ok(R),
    Other { status: StatusCode },
}

pub(crate) trait HttpReqProc: Clone + Send + Sync + 'static {
    type Output;

    fn process_request(
        &self,
        req: Req,
    ) -> impl Future<Output = Result<Self::Output, Response>> + Send + '_;
}

impl<R, F, FR> HttpReqProc for F
where
    F: (Fn(Req) -> FR) + Clone + Send + Sync + 'static,
    FR: Future<Output = Result<R, Response>> + Send + 'static,
{
    type Output = R;

    #[inline]
    fn process_request(
        &self,
        req: Req,
    ) -> impl Future<Output = Result<Self::Output, Response>> + Send + '_ {
        self(req)
    }
}

struct HttpServer<Body, Resp, Proc>
where
    Body: Send + 'static,
    Resp: Serialize + Send + 'static,
    Proc: HttpReqProc<Output = Body>,
{
    config: HttpConfShared,
    body_tx: mpsc::UnboundedSender<HttpResponder<Body, Resp>>,
    proc: Proc,
}

impl<B, R, P> Clone for HttpServer<B, R, P>
where
    B: Send + 'static,
    R: Serialize + Send + 'static,
    P: HttpReqProc<Output = B> + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            body_tx: self.body_tx.clone(),
            proc: self.proc.clone(),
        }
    }
}

impl<B, R, P> HttpServer<B, R, P>
where
    B: Send + 'static,
    R: Serialize + Send + 'static,
    P: HttpReqProc<Output = B> + Send + 'static,
{
    pub fn new(
        body_tx: mpsc::UnboundedSender<HttpResponder<B, R>>,
        config: HttpConfig,
        proc: P,
    ) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            body_tx,
            proc,
        }
    }

    async fn handle_req(self, req: Req) -> Result<Response, Response> {
        let Self {
            config,
            body_tx,
            proc,
        } = self;
        // check access token
        if let Some((header_token, query_token)) = &config.read().authorization {
            if let Some(header) = req.headers().get(AUTHORIZATION) {
                if header != header_token {
                    return Err(mk_resp(StatusCode::FORBIDDEN, None::<Vec<u8>>));
                }
            } else if let Some(query) = req.uri().query() {
                let params: ReqQuery = serde_qs::from_str(query).map_err(|e| {
                    mk_resp(
                        StatusCode::BAD_REQUEST,
                        Some(format!("invalid request query: {}", e)),
                    )
                })?;
                if params.access_token != query_token {
                    return Err(mk_resp(StatusCode::FORBIDDEN, None::<Vec<u8>>));
                }
            } else {
                return Err(mk_resp(StatusCode::UNAUTHORIZED, None::<Vec<u8>>));
            }
        }

        let Some(content_type) = req.headers().get(CONTENT_TYPE) else {
            return Err(mk_resp(
                StatusCode::BAD_REQUEST,
                Some("missing header `Content-Type`"),
            ));
        };
        if content_type
            .to_str()
            .map_err(|e| mk_resp(StatusCode::BAD_REQUEST, Some(e.to_string())))?
            != "application/json"
        {
            return Err(mk_resp(StatusCode::NOT_ACCEPTABLE, None::<String>));
        }

        let body: B = proc.process_request(req).await?;

        let (tx, rx) = oneshot::channel();
        body_tx.send((body, tx)).unwrap();
        // acquire response
        match rx.await.unwrap() {
            HttpResponse::Ok(data) => {
                let resp = serde_json::to_vec(&data).map_err(|e| {
                    mk_resp(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Some(format!("error while serializing response: {}", e)),
                    )
                })?;
                Ok(mk_resp(StatusCode::OK, Some(resp)))
            }
            HttpResponse::Other { status } => Ok(mk_resp(status, None::<Vec<u8>>)),
        }
    }
}

pub(crate) trait HttpTaskHandler<Body, Resp, Cmd, Msg>:
    CmdHandler<Cmd, Msg>
    + RecvHandler<(Body, oneshot::Sender<HttpResponse<Resp>>), Msg>
    + CloseHandler<Msg>
    + Send
    + 'static
where
    Resp: Send + Serialize + 'static,
{
}
impl<B, R, C, M, T> HttpTaskHandler<B, R, C, M> for T
where
    T: CmdHandler<C, M> + RecvHandler<HttpResponder<B, R>, M> + CloseHandler<M> + Send + 'static,
    R: Send + Serialize + 'static,
{
}

/// HttpServerTask is responsible for managing the lifecycle of the HTTP server and handling incoming requests.
/// `Body`: Http request body
/// `Resp`: Http response
/// `Cmd`: Commands send to http server
/// `Msg`: Messages produced by http server
/// `Handler`: Handling http server, converting request body into message, process commands.
pub(crate) struct HttpServerTask<Body, Resp, Cmd, Msg, Handler>
where
    Body: Send + 'static,
    Resp: Serialize + Send + 'static,
    Handler: HttpTaskHandler<Body, Resp, Cmd, Msg>,
{
    _phantom: PhantomData<(Handler, Body, Resp, Msg, Cmd)>,
}

impl<B, R, C, M, H> HttpServerTask<B, R, C, M, H>
where
    H: HttpTaskHandler<B, R, C, M>,
    B: Send + 'static,
    R: Serialize + Send + 'static,
    M: Send + 'static,
    C: Send + 'static,
{
    pub async fn create<Proc: HttpReqProc<Output = B>>(
        addr: impl Into<SocketAddr>,
        config: HttpConfig,
        handler: H,
        req_proc: Proc,
    ) -> (mpsc::UnboundedSender<C>, mpsc::UnboundedReceiver<M>) {
        let (body_tx, body_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();

        tokio::spawn(Self::server_task(
            addr.into(),
            body_tx,
            signal_rx,
            config.clone(),
            req_proc,
        ));
        tokio::spawn(Self::manage_task(
            body_rx, msg_tx, cmd_rx, signal_tx, handler,
        ));

        (cmd_tx, msg_rx)
    }

    async fn server_task<Proc>(
        addr: SocketAddr,
        body_tx: mpsc::UnboundedSender<HttpResponder<B, R>>,
        mut signal_rx: mpsc::UnboundedReceiver<Signal>,
        config: HttpConfig,
        req_proc: Proc,
    ) -> Result<(), AllErr>
    where
        Proc: HttpReqProc<Output = B> + Send + 'static,
    {
        let listener = TcpListener::bind(addr).await?;
        let serv = HttpServer::new(body_tx, config, req_proc);

        async fn handle_http<
            Body: Send + 'static,
            Resp: Serialize + Send + 'static,
            Proc: HttpReqProc<Output = Body>,
        >(
            conn: TcpStream,
            serv: HttpServer<Body, Resp, Proc>,
        ) -> Result<(), AllErr> {
            let io = TokioIo::new(conn);
            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|r| async {
                            Ok::<Response, Infallible>(
                                serv.clone().handle_req(r).await.unwrap_or_else(|e| e),
                            )
                        }),
                    )
                    .await
                {
                    log::error!("Failed to serve connection: {:?}", err);
                }
            });
            Ok(())
        }

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((conn, _)) => {
                            handle_http(conn, serv.clone()).await?;
                        },
                        Err(e) => {
                            log::error!("faild to accept tcp conn: {}", e);
                        },
                    }
                },
                Some(signal) = signal_rx.recv() => {
                    match signal {
                        Signal::Close(tx) => {
                            tx.send(Ok(())).map_err(|_| AllErr::ChannelClosed)?;
                            break Ok(())
                        },
                    }
                }
                else => break Err(AllErr::ChannelClosed)
            }
        }
    }

    async fn manage_task(
        mut body_rx: mpsc::UnboundedReceiver<HttpResponder<B, R>>,
        msg_tx: mpsc::UnboundedSender<M>,
        mut cmd_rx: mpsc::UnboundedReceiver<C>,
        signal_tx: mpsc::UnboundedSender<Signal>,
        mut handler: H,
    ) -> Result<ClosedReason, AllErr> {
        let state = ConnState::default();

        while state.is_active() {
            let state = state.clone();
            tokio::select! {
                Some((body, resp_tx)) = body_rx.recv() => {
                    handler.handle_recv((body, resp_tx), msg_tx.clone(), state).await?;
                }
                Some(cmd) = cmd_rx.recv() => {
                    handler.handle_cmd(cmd, state).await?;
                }
                else => {
                    state.set_active(false);
                    return Err(AllErr::ChannelClosed)
                }
            }
        }

        let (tx, rx) = oneshot::channel();
        let result: Result<ClosedReason, String> = match signal_tx.send(Signal::Close(tx)) {
            Ok(_) => match rx.await {
                Ok(_) => Ok(ClosedReason::Ok),
                Err(_) => Ok(ClosedReason::Partial("close callback closed".into())),
            },
            Err(_) => Ok(ClosedReason::Partial("signal channel closed".into())),
        };

        if let Err(e) = handler.handle_close(result.clone(), msg_tx).await {
            log::error!("error occurred while handling conn close: {}", e);
        }
        result.map_err(AllErr::other)
    }
}
