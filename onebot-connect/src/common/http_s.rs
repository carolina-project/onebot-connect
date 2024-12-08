use std::{convert::Infallible, marker::PhantomData, net::SocketAddr, sync::Arc};

use bytes::Bytes;
use http::{header::AUTHORIZATION, StatusCode};
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
pub struct HttpConfig {
    authorization: Authorization,
}
type HttpConfShared = Arc<RwLock<HttpConfig>>;

struct HttpServer<Body: DeserializeOwned + Send + 'static, Resp: Serialize + Send + 'static> {
    config: HttpConfShared,
    body_tx: mpsc::UnboundedSender<(Body, oneshot::Sender<Resp>)>,
}

impl<B: DeserializeOwned + Send + 'static, R: Serialize + Send + 'static> Clone
    for HttpServer<B, R>
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            body_tx: self.body_tx.clone(),
        }
    }
}

impl<B: DeserializeOwned + Send + 'static, R: Serialize + Send + 'static> HttpServer<B, R> {
    pub fn new(
        body_tx: mpsc::UnboundedSender<(B, oneshot::Sender<R>)>,
        config: HttpConfig,
    ) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            body_tx,
        }
    }

    async fn handle_req(self, req: Req) -> Result<Response, Response> {
        let Self { config, body_tx } = self;
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

        let body: B = serde_json::from_slice(&data.to_bytes()).map_err(|e| {
            mk_resp(
                StatusCode::BAD_REQUEST,
                Some(format!("invalid data: {}", e)),
            )
        })?;

        let (tx, rx) = oneshot::channel();
        body_tx.send((body, tx)).unwrap();
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

pub(crate) struct HttpServerTask<Body, Resp, Cmd, Msg, CHandler, RHandler>
where
    CHandler: CmdHandler<Cmd, Msg>,
    Body: DeserializeOwned + Send + 'static,
    Resp: Serialize + Send + 'static,
    RHandler: RecvHandler<(Body, oneshot::Sender<Resp>), Msg>,
{
    _phantom: PhantomData<(CHandler, Body, Resp, RHandler, Msg, Cmd)>,
}

impl<B, R, C, M, CH, RH> HttpServerTask<B, R, C, M, CH, RH>
where
    CH: CmdHandler<C, M> + Send + 'static,
    B: DeserializeOwned + Send + 'static,
    R: Serialize + Send + 'static,
    RH: RecvHandler<(B, oneshot::Sender<R>), M> + Send + 'static,
    M: Send + 'static,
    C: Send + 'static,
{
    pub async fn create(
        addr: impl Into<SocketAddr>,
        config: HttpConfig,
        recv_handler: RH,
        cmd_handler: CH,
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
        ));
        tokio::spawn(Self::manage_task(
            body_rx,
            msg_tx,
            cmd_rx,
            cmd_handler,
            recv_handler,
            signal_tx,
        ));

        (cmd_tx, msg_rx)
    }

    async fn server_task(
        addr: SocketAddr,
        body_tx: mpsc::UnboundedSender<(B, oneshot::Sender<R>)>,
        mut signal_rx: mpsc::UnboundedReceiver<Signal>,
        config: HttpConfig,
    ) -> Result<(), AllErr> {
        let listener = TcpListener::bind(addr).await?;
        let serv = HttpServer::new(body_tx, config);

        async fn handle_http<
            Body: DeserializeOwned + Send + 'static,
            Resp: Serialize + Send + 'static,
        >(
            conn: TcpStream,
            serv: HttpServer<Body, Resp>,
        ) -> Result<(), AllErr> {
            let io = TokioIo::new(conn);

            let serv_clone = serv.clone();
            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|r| async {
                            Ok::<Response, Infallible>(
                                serv_clone.clone().handle_req(r).await.unwrap_or_else(|e| e),
                            )
                        }),
                    )
                    .await
                {
                    println!("Failed to serve connection: {:?}", err);
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
        mut body_rx: mpsc::UnboundedReceiver<(B, oneshot::Sender<R>)>,
        msg_tx: mpsc::UnboundedSender<M>,
        mut cmd_rx: mpsc::UnboundedReceiver<C>,
        mut cmd_handler: CH,
        mut recv_handler: RH,
        signal_tx: mpsc::UnboundedSender<Signal>,
    ) -> Result<ClosedReason, AllErr> {
        let state = ConnState::default();

        while state.is_active() {
            let msg_tx = msg_tx.clone();
            let state = state.clone();
            tokio::select! {
                Some((body, resp_tx)) = body_rx.recv() => {
                    recv_handler.handle_recv((body, resp_tx), msg_tx, state).await?;
                }
                Some(cmd) = cmd_rx.recv() => {
                    cmd_handler.handle_cmd(cmd, msg_tx, state).await?;
                }
                else => {
                    state.set_active(false);
                    return Err(AllErr::ChannelClosed)
                }
            }
        }

        let (tx, rx) = oneshot::channel();
        match signal_tx.send(Signal::Close(tx)) {
            Ok(_) => match rx.await {
                Ok(_) => Ok(ClosedReason::Ok),
                Err(_) => Ok(ClosedReason::Partial("close callback closed".into())),
            },
            Err(_) => Ok(ClosedReason::Partial("signal channel closed".into())),
        }
    }
}
