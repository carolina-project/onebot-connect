use std::{marker::PhantomData, net::SocketAddr};

use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use http::{header::AUTHORIZATION, StatusCode};
use serde::de::DeserializeOwned;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::{self};
use tokio_tungstenite::WebSocketStream;

use crate::Error as AllErr;
use onebot_connect_interface::{ClosedReason, Error as OCErr};

use super::*;

pub(crate) async fn wait_for_ws(
    addr: impl ToSocketAddrs,
    access_token: Option<&str>,
) -> Result<(WebSocketStream<TcpStream>, SocketAddr), crate::Error> {
    let listener = TcpListener::bind(addr).await?;

    let (stream, addr) = listener.accept().await?;
    Ok((
        tokio_tungstenite::accept_hdr_async(
            stream,
            |req: &http::Request<()>, response: http::Response<()>| {
                let Some(token) = access_token else {
                    return Ok(response);
                };

                if req
                    .headers()
                    .get(AUTHORIZATION)
                    .map(|r| {
                        r.to_str()
                            .map(|h_token| h_token == format!("Bearer {token}"))
                            .unwrap_or_default()
                    })
                    .unwrap_or_default()
                {
                    Ok(response)
                } else {
                    Err(http::Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(Some("Invalid access token".into()))
                        .unwrap())
                }
            },
        )
        .await?,
        addr,
    ))
}

pub(crate) trait WSTaskHandler<Cmd, Body, Msg>:
    CmdHandler<(Cmd, mpsc::UnboundedSender<tungstenite::Message>)>
    + RecvHandler<Body, Msg>
    + CloseHandler<Msg>
    + Send
    + Clone
    + 'static
{
}
impl<C, B, M, T> WSTaskHandler<C, B, M> for T where
    T: CmdHandler<(C, mpsc::UnboundedSender<tungstenite::Message>)>
        + RecvHandler<B, M>
        + CloseHandler<M>
        + Send
        + Clone
        + 'static
{
}

pub(crate) struct WSTask<Body, Cmd, Msg, Handler>
where
    Body: DeserializeOwned + Send + 'static,
    Handler: WSTaskHandler<Cmd, Body, Msg>,
{
    _phantom: PhantomData<(Handler, Body, Msg, Cmd)>,
}

pub trait WsStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T> WsStream for T where T: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<B, C, M, HL> WSTask<B, C, M, HL>
where
    HL: WSTaskHandler<C, B, M>,
    B: DeserializeOwned + Send + 'static,
    M: Send + 'static,
    C: Send + 'static,
{
    pub async fn create_with_channel<S>(
        ws: WebSocketStream<S>,
        cmd_rx: mpsc::UnboundedReceiver<C>,
        msg_tx: mpsc::UnboundedSender<M>,
        handler: HL,
    ) where
        S: WsStream,
    {
        tokio::spawn(Self::manage_task(ws, cmd_rx, msg_tx, handler));
    }

    pub async fn create<S>(
        ws: WebSocketStream<S>,
        handler: HL,
    ) -> (mpsc::UnboundedSender<C>, mpsc::UnboundedReceiver<M>)
    where
        S: WsStream,
    {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        Self::create_with_channel(ws, cmd_rx, msg_tx, handler).await;

        (cmd_tx, msg_rx)
    }

    /// Manages the WebSocket task, handling command and message processing.
    async fn manage_task<S>(
        ws: WebSocketStream<S>,
        mut cmd_rx: mpsc::UnboundedReceiver<C>,
        msg_tx: mpsc::UnboundedSender<M>,
        mut handler: HL,
    ) -> Result<ClosedReason, AllErr>
    where
        S: WsStream,
    {
        let (sink, stream) = ws.split();

        let (signal_send_tx, signal_send_rx) = mpsc::unbounded_channel();
        let (signal_recv_tx, signal_recv_rx) = mpsc::unbounded_channel();
        // channel for sending ws message
        let (send_tx, send_rx) = mpsc::unbounded_channel();
        // channel for receiving ws message
        let (recv_tx, mut recv_rx) = mpsc::unbounded_channel();

        tokio::spawn(Self::send_task(sink, signal_send_rx, send_rx));
        tokio::spawn(Self::recv_task(stream, signal_recv_rx, recv_tx));

        let state = ConnState::default();

        while state.is_active() {
            let state = state.clone();
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    let send_tx = send_tx.clone();
                    let mut handler = handler.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handler.handle_cmd((cmd, send_tx), state).await {
                            log::error!("cmd handler err: {e}")
                        }
                    });
                },
                Some(msg) = recv_rx.recv() => {
                    let handler = handler.clone();
                    let msg_tx = msg_tx.clone();
                    log::trace!("recv: {}", msg);
                    tokio::spawn(async move {
                        let recv: B = match serde_json::from_slice(&msg.into_data()) {
                            Ok(recv) => recv,
                            Err(e) => {
                                log::error!("error while deserializing data: {e}");
                                return
                            },
                        };
                        if let Err(e) = handler.handle_recv(recv, msg_tx, state).await {
                            log::error!("recv handler err: {e}")
                        }
                    });
                },
                else => return Err(AllErr::ChannelClosed),
            }
        }

        async fn close(tx: mpsc::UnboundedSender<Signal>) -> Result<ClosedReason, String> {
            let (cb_tx, cb_rx) = oneshot::channel();
            match tx.send(Signal::Close(cb_tx)) {
                Ok(_) => match cb_rx.await {
                    Ok(_) => Ok(ClosedReason::Ok),
                    Err(_) => Ok(ClosedReason::Partial("close callback closed".into())),
                },
                Err(_) => Ok(ClosedReason::Partial("signal channel closed".into())),
            }
        }

        let ss_res: Result<ClosedReason, String> = close(signal_send_tx).await;
        let sr_res: Result<ClosedReason, String> = close(signal_recv_tx).await;
        let res = ss_res.and(sr_res);

        if let Err(e) = handler.handle_close(res.clone(), msg_tx).await {
            log::error!("error occurred while handling conn close: {}", e);
        }
        res.map_err(AllErr::other)
    }

    /// Handles sending messages over the WebSocket connection.
    async fn send_task<S>(
        mut sink: SplitSink<WebSocketStream<S>, tungstenite::Message>,
        mut signal_rx: mpsc::UnboundedReceiver<Signal>,
        mut send_rx: mpsc::UnboundedReceiver<tungstenite::Message>,
    ) -> Result<(), AllErr>
    where
        S: WsStream,
    {
        loop {
            tokio::select! {
                Some(signal) = signal_rx.recv() => {
                    match signal {
                        Signal::Close(callback) => {
                            callback.send(Ok(())).map_err(|_| AllErr::ChannelClosed)?;
                            break Ok(());
                        }
                    }
                },
                Some(data) = send_rx.recv() => {
                    sink.send(data).await?;
                }
            }
        }
    }

    /// Handles receiving messages from the WebSocket connection.
    async fn recv_task<S>(
        mut stream: SplitStream<WebSocketStream<S>>,
        mut signal_rx: mpsc::UnboundedReceiver<Signal>,
        recv_tx: mpsc::UnboundedSender<tungstenite::Message>,
    ) -> Result<(), AllErr>
    where
        S: WsStream,
    {
        loop {
            tokio::select! {
                Some(signal) = signal_rx.recv() => {
                    match signal {
                        Signal::Close(sender) => {
                            sender.send(Ok(())).map_err(|_| AllErr::ChannelClosed)?;
                            break Ok(());
                        }
                    }
                },
                Some(msg) = stream.next() => {
                    recv_tx.send(msg?).map_err(OCErr::closed)?;
                },
                else => break Err(AllErr::ChannelClosed),
            }
        }
    }
}
