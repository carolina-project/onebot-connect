use ::http::HeaderValue;
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use hyper::header::AUTHORIZATION;
use onebot_connect_interface::{ClosedReason, ConfigError};
use onebot_types::ob12::action::{RespData, RespStatus, RetCode};
use serde_value::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::oneshot,
    task::JoinHandle,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, client::IntoClientRequest, Error as WsError},
    WebSocketStream,
};

use super::*;

pub struct WSCreate<R>
where
    R: IntoClientRequest + Unpin,
{
    req: R,
    access_token: Option<String>,
}
impl<R: IntoClientRequest + Unpin> WSCreate<R> {
    pub fn new(req: R) -> Self {
        Self {
            req,
            access_token: None,
        }
    }
}

impl<R> Create for WSCreate<R>
where
    R: IntoClientRequest + Unpin,
{
    type Error = crate::Error;
    type Source = RxMessageSource;
    type Provider = TxImplProvider;
    type Message = ();

    async fn create(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let Self { req, access_token } = self;
        let mut req = req.into_client_request()?;
        if let Some(token) = access_token {
            req.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }
        let (ws, _) = connect_async(req).await?;
        let (_tasks, WSConnectionHandle { msg_rx, cmd_tx }) = WSTaskHandle::create(ws);
        Ok((
            RxMessageSource::new(msg_rx),
            TxImplProvider::new(cmd_tx),
            (),
        ))
    }

    fn with_authorization(self, access_token: impl Into<String>) -> Self {
        Self {
            access_token: Some(access_token.into()),
            ..self
        }
    }
}

enum Signal {
    Close(oneshot::Sender<Result<(), String>>),
}
enum RecvData {
    Action(Action),
}

/// WebSocket connection task handle
#[allow(unused)]
pub(crate) struct WSTaskHandle {
    pub recv_handle: JoinHandle<Result<(), crate::Error>>,
    pub send_handle: JoinHandle<Result<(), crate::Error>>,
    pub manage_handle: JoinHandle<Result<(), crate::Error>>,
}

/// Wrapper for `RxMessageSource` and `TxClient`
pub(crate) struct WSConnectionHandle {
    pub msg_rx: mpsc::UnboundedReceiver<RecvMessage>,
    pub cmd_tx: mpsc::UnboundedSender<Command>,
}

pub(crate) trait WsStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T> WsStream for T where T: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl WSTaskHandle {
    pub(crate) fn create<T>(ws: WebSocketStream<T>) -> (Self, WSConnectionHandle)
    where
        T: WsStream,
    {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let (write, read) = ws.split();

        let (signal_tx_recv, signal_rx_recv) = mpsc::unbounded_channel();
        let (signal_tx_send, signal_rx_send) = mpsc::unbounded_channel();

        let (recv_data_tx, recv_data_rx) = mpsc::unbounded_channel();
        let (action_tx, action_rx) = mpsc::unbounded_channel();

        let recv_handle = tokio::spawn(Self::recv_task(read, recv_data_tx, signal_rx_recv));
        let send_handle = tokio::spawn(Self::send_task(write, action_rx, signal_rx_send));
        let manage_handle = tokio::spawn(Self::manage_task(
            msg_tx,
            cmd_rx,
            signal_tx_recv,
            signal_tx_send,
            recv_data_rx,
            action_tx,
        ));

        (
            Self {
                recv_handle,
                send_handle,
                manage_handle,
            },
            WSConnectionHandle { msg_rx, cmd_tx },
        )
    }

    #[inline(always)]
    async fn handle_close(
        signal_tx_send: &mpsc::UnboundedSender<Signal>,
        signal_tx_recv: &mpsc::UnboundedSender<Signal>,
        tx: oneshot::Sender<Result<ClosedReason, String>>,
    ) {
        let (close_tx_recv, close_rx_recv) = oneshot::channel();
        let (close_tx_send, close_rx_send) = oneshot::channel();
        signal_tx_recv.send(Signal::Close(close_tx_recv)).unwrap();
        signal_tx_send.send(Signal::Close(close_tx_send)).unwrap();

        let (recv_result, send_result) =
            (close_rx_recv.await.unwrap(), close_rx_send.await.unwrap());

        if recv_result.is_err() && send_result.is_err() {
            tx.send(Err(format!(
                "recv task error: {} \n send task error: {}",
                recv_result.err().unwrap(),
                send_result.err().unwrap()
            )))
            .unwrap();
        } else {
            let mut err_str = String::new();
            if let Err(e) = recv_result {
                err_str.push_str(&format!("recv task error: {}\n", e));
            }

            if let Err(e) = send_result {
                err_str.push_str(&format!("send task error: {}", e));
            }

            tx.send(Ok(ClosedReason::Partial(err_str))).unwrap();
        }
    }

    #[inline(always)]
    async fn handle_command(
        command: Command,
        data_tx: &mpsc::UnboundedSender<Vec<u8>>,
        signal_tx_send: &mpsc::UnboundedSender<Signal>,
        signal_tx_recv: &mpsc::UnboundedSender<Signal>,
    ) -> Result<(), crate::Error> {
        match command {
            Command::Close(tx) => {
                Self::handle_close(signal_tx_send, signal_tx_recv, tx).await;
                Ok(())
            }
            Command::GetConfig(_, tx) => tx.send(None).map_err(|_| crate::Error::ChannelClosed),
            Command::SetConfig((key, _), tx) => tx
                .send(Err(ConfigError::UnknownKey(key)))
                .map_err(|_| crate::Error::ChannelClosed),
            Command::Respond(echo, resp) => {
                let (status, retcode, message, data) = match resp {
                    ActionResponse::Ok(data) => {
                        (RespStatus::Ok, RetCode::Success, Default::default(), data)
                    }
                    ActionResponse::Error { retcode, message } => (
                        RespStatus::Failed,
                        retcode,
                        message,
                        Value::Map(Default::default()),
                    ),
                };
                let echo = match echo {
                    ActionEcho::Inner(_) => None,
                    ActionEcho::Outer(echo) => Some(echo),
                };

                let data = serde_json::to_vec(&RespData {
                    status,
                    retcode,
                    data,
                    message,
                    echo,
                })
                .map_err(OCError::serialize)?;

                data_tx.send(data).map_err(OCError::closed)?;
                Ok(())
            }
            Command::Event(event) => Ok(data_tx
                .send(serde_json::to_vec(&event).map_err(OCError::serialize)?)
                .map_err(OCError::closed)?),
        }
    }

    async fn manage_task(
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        mut cmd_rx: mpsc::UnboundedReceiver<Command>,
        signal_tx_recv: mpsc::UnboundedSender<Signal>,
        signal_tx_send: mpsc::UnboundedSender<Signal>,
        mut recv_data_rx: mpsc::UnboundedReceiver<RecvData>,
        data_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<(), crate::Error> {
        loop {
            tokio::select! {
                command = cmd_rx.recv() => {
                    let Some(command) = command else {
                        break Err(crate::Error::ChannelClosed)
                    };
                    Self::handle_command(
                        command,
                        &data_tx,
                        &signal_tx_send,
                        &signal_tx_recv
                    ).await?;
                }
                recv_data = recv_data_rx.recv() => {
                    let Some(data) = recv_data else {
                        break Err(crate::Error::ChannelClosed)
                    };

                    match data {
                        RecvData::Action(action) => {
                            msg_tx.send(RecvMessage::Action(action)).map_err(OCError::closed)?;
                        }
                    }
                }
            }
        }
    }

    async fn send_task<T>(
        mut stream: SplitSink<WebSocketStream<T>, tokio_tungstenite::tungstenite::Message>,
        mut data_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        mut signal_rx: mpsc::UnboundedReceiver<Signal>,
    ) -> Result<(), crate::Error>
    where
        T: WsStream,
    {
        loop {
            tokio::select! {
                data = data_rx.recv() => {
                    let Some(data) = data else {
                        break Err(crate::Error::ChannelClosed)
                    };

                    match stream.send(
                        tungstenite::Message::from(data)
                    ).await {
                        Ok(_) => {},
                        Err(WsError::ConnectionClosed) => {
                            break Err(crate::Error::ConnectionClosed)
                        },
                        Err(e) => {
                            log::error!("ws error: {}", e)
                        }
                    };

                }
                signal = signal_rx.recv() => {
                    if let Some(Signal::Close(tx)) = signal {
                        tx.send(Ok(())).unwrap();
                        break Ok(())
                    }
                }
            }
        }
    }

    fn parse_data(data: Vec<u8>) -> Result<RecvData, crate::RecvError> {
        Ok(RecvData::Action(serde_json::from_slice(&data)?))
    }

    async fn recv_task<T>(
        mut stream: SplitStream<WebSocketStream<T>>,
        data_tx: mpsc::UnboundedSender<RecvData>,
        mut signal_rx: mpsc::UnboundedReceiver<Signal>,
    ) -> Result<(), crate::Error>
    where
        T: WsStream,
    {
        loop {
            tokio::select! {
                data = stream.next() => {
                    let Some(data) = data else {
                        break Err(crate::Error::ConnectionClosed)
                    };

                    match data {
                        Ok(msg) => match Self::parse_data(msg.into()) {
                            Ok(data) => {
                                data_tx.send(data).unwrap();
                            }
                            Err(e) => {
                                log::error!("error occurred while parsing ws data: {}", e)
                            }
                        },
                        Err(e) => {
                            log::error!("error occured while ws recv: {}", e)
                        }
                    }
                }
                signal = signal_rx.recv() => {
                    if let Some(Signal::Close(tx)) = signal {
                        tx.send(Ok(())).unwrap();
                        break Ok(())
                    }
                }
            }
        }
    }
}
