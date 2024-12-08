use std::marker::PhantomData;

use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{de::DeserializeOwned, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::{self};
use tokio_tungstenite::WebSocketStream;

use crate::Error as AllErr;
use onebot_connect_interface::{ClosedReason, Error as OCErr};

use super::*;

pub(crate) struct WSTaskHandle<Body, Resp, Cmd, Msg, CHandler, RHandler>
where
    CHandler: CmdHandler<(Cmd, mpsc::UnboundedSender<tungstenite::Message>), Msg>,
    Body: DeserializeOwned + Send + 'static,
    Resp: Serialize + Send + 'static,
    RHandler: RecvHandler<Body, Msg>,
{
    _phantom: PhantomData<(CHandler, Body, Resp, RHandler, Msg, Cmd)>,
}

pub trait WsStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T> WsStream for T where T: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<B, R, C, M, CH, RH> WSTaskHandle<B, R, C, M, CH, RH>
where
    CH: CmdHandler<(C, mpsc::UnboundedSender<tungstenite::Message>), M> + Send + 'static,
    B: DeserializeOwned + Send + 'static,
    R: Serialize + Send + 'static,
    RH: RecvHandler<B, M> + Send + 'static,
    M: Send + 'static,
    C: Send + 'static,
{
    pub async fn create<S>(
        ws: WebSocketStream<S>,
        recv_handler: RH,
        cmd_handler: CH,
    ) -> (mpsc::UnboundedSender<C>, mpsc::UnboundedReceiver<M>)
    where
        S: WsStream,
    {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        tokio::spawn(Self::manage_task(
            ws,
            cmd_rx,
            msg_tx,
            recv_handler,
            cmd_handler,
        ));

        (cmd_tx, msg_rx)
    }

    /// Manages the WebSocket task, handling command and message processing.
    async fn manage_task<S>(
        ws: WebSocketStream<S>,
        mut cmd_rx: mpsc::UnboundedReceiver<C>,
        msg_tx: mpsc::UnboundedSender<M>,
        mut recv_handler: RH,
        mut cmd_handler: CH,
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
            let msg_tx = msg_tx.clone();
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    cmd_handler.handle_cmd((cmd, send_tx.clone()), msg_tx, state).await?;
                },
                Some(msg) = recv_rx.recv() => {
                    let recv: B = match serde_json::from_slice(&msg.into_data()) {
                        Ok(recv) => {recv},
                        Err(e) => {
                            log::error!("error while deserializing data: {}", e);
                            continue;
                        },
                    };
                    recv_handler.handle_recv(recv, msg_tx, state).await?;
                },
                else => return Err(AllErr::ChannelClosed),
            }
        }

        async fn close(tx: mpsc::UnboundedSender<Signal>) -> Result<ClosedReason, AllErr> {
            let (cb_tx, cb_rx) = oneshot::channel();
            match tx.send(Signal::Close(cb_tx)) {
                Ok(_) => match cb_rx.await {
                    Ok(_) => Ok(ClosedReason::Ok),
                    Err(_) => Ok(ClosedReason::Partial("close callback closed".into())),
                },
                Err(_) => Ok(ClosedReason::Partial("signal channel closed".into())),
            }
        }

        let ss_res = close(signal_send_tx).await;
        let sr_res = close(signal_recv_tx).await;
        ss_res.and(sr_res)
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
