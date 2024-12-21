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

/// Handle http request and make an connection message sending to `RecvHandler`.
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

pub(crate) async fn parse_req<B>(req: Req) -> Result<B, Response>
where
    B: DeserializeOwned + Send,
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

/// Handling OneBot 12 Webhook/Http server request，check authorization and process requests.
#[derive(Clone)]
pub(crate) struct OB12HttpProc<P>
where
    P: HttpReqProc,
{
    config: HttpConfShared,
    proc: P,
}

impl<P: HttpReqProc> OB12HttpProc<P> {
    pub fn new(conf: HttpConfig, proc: P) -> Self {
        Self {
            config: Arc::new(RwLock::new(conf)),
            proc,
        }
    }
}

impl<R, P> HttpReqProc for OB12HttpProc<P>
where
    P: HttpReqProc<Output = R>,
    R: Send + Sync + 'static,
{
    type Output = R;

    async fn process_request(&self, req: Req) -> Result<Self::Output, Response> {
        if let Some((header_token, query_token)) = &self.config.read().authorization {
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
        } else {
            self.proc.process_request(req).await
        }
    }
}

struct HttpServer<Body, Resp, Proc>
where
    Body: Send + 'static,
    Resp: Serialize + Send + 'static,
    Proc: HttpReqProc<Output = Body>,
{
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
    pub fn new(body_tx: mpsc::UnboundedSender<HttpResponder<B, R>>, proc: P) -> Self {
        Self { body_tx, proc }
    }

    async fn handle_req(self, req: Req) -> Result<Response, Response> {
        let Self { body_tx, proc } = self;
        let body: B = proc.process_request(req).await?;

        let (tx, rx) = oneshot::channel();
        body_tx.send((body, tx)).unwrap();
        // acquire response
        let resp = match rx.await {
            Ok(resp) => resp,
            Err(e) => {
                log::error!("resp channel closed, please look out the error log.");
                return Err(mk_resp(StatusCode::INTERNAL_SERVER_ERROR, None::<String>));
            }
        };
        match resp {
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
    CmdHandler<Cmd>
    + RecvHandler<(Body, oneshot::Sender<HttpResponse<Resp>>), Msg>
    + CloseHandler<Msg>
    + Send
    + Clone
    + 'static
where
    Resp: Send + Serialize + 'static,
{
}
impl<B, R, C, M, T> HttpTaskHandler<B, R, C, M> for T
where
    T: CmdHandler<C>
        + RecvHandler<HttpResponder<B, R>, M>
        + CloseHandler<M>
        + Send
        + Clone
        + 'static,
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

impl<Body, Resp, Cmd, Msg, Handler> HttpServerTask<Body, Resp, Cmd, Msg, Handler>
where
    Handler: HttpTaskHandler<Body, Resp, Cmd, Msg>,
    Body: Send + 'static,
    Resp: Serialize + Send + 'static,
    Msg: Send + 'static,
    Cmd: Send + 'static,
{
    pub async fn create_with_channel<Proc: HttpReqProc<Output = Body>>(
        addr: impl Into<SocketAddr>,
        handler: Handler,
        req_proc: Proc,
        cmd_rx: mpsc::UnboundedReceiver<Cmd>,
        msg_tx: mpsc::UnboundedSender<Msg>,
    ) -> Result<(), AllErr> {
        let (body_tx, body_rx) = mpsc::unbounded_channel();
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();

        let listener = TcpListener::bind(addr.into()).await?;
        tokio::spawn(async move {
            match Self::server_task(listener, body_tx, signal_rx, req_proc).await {
                Ok(()) => {}
                Err(e) => {
                    log::error!("server task stopped with err: {e}");
                }
            }
        });
        tokio::spawn(async move {
            match Self::manage_task(body_rx, msg_tx, cmd_rx, signal_tx, handler).await {
                Ok(reason) => {
                    log::info!("manage task stopped({reason:?})")
                }
                Err(e) => log::error!("manage task stopped with err: {e}"),
            }
        });

        Ok(())
    }

    pub async fn create<Proc: HttpReqProc<Output = Body>>(
        addr: impl Into<SocketAddr>,
        handler: Handler,
        req_proc: Proc,
    ) -> Result<(mpsc::UnboundedSender<Cmd>, mpsc::UnboundedReceiver<Msg>), AllErr> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        Self::create_with_channel(addr, handler, req_proc, cmd_rx, msg_tx).await?;
        Ok((cmd_tx, msg_rx))
    }

    async fn server_task<Proc>(
        listener: TcpListener,
        body_tx: mpsc::UnboundedSender<HttpResponder<Body, Resp>>,
        mut signal_rx: mpsc::UnboundedReceiver<Signal>,
        req_proc: Proc,
    ) -> Result<(), AllErr>
    where
        Proc: HttpReqProc<Output = Body> + Send + 'static,
    {
        let serv = HttpServer::new(body_tx, req_proc);

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
                                serv.clone().handle_req(r).await.unwrap_err(),
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
        mut body_rx: mpsc::UnboundedReceiver<HttpResponder<Body, Resp>>,
        msg_tx: mpsc::UnboundedSender<Msg>,
        mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
        signal_tx: mpsc::UnboundedSender<Signal>,
        mut handler: Handler,
    ) -> Result<ClosedReason, AllErr> {
        let state = ConnState::default();

        while state.is_active() {
            let state = state.clone();
            tokio::select! {
                Some((body, resp_tx)) = body_rx.recv() => {
                    let mut handler = handler.clone();
                    let msg_tx = msg_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handler.handle_recv((body, resp_tx), msg_tx, state).await {
                            log::error!("recv handler error: {e}")
                        }
                    });
                }
                Some(cmd) = cmd_rx.recv() => {
                    let handler = handler.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handler.clone().handle_cmd(cmd, state).await {
                            log::error!("cmd handler error: {e}")
                        }
                    });
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
