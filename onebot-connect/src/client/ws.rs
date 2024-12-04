use std::io;

use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use http::{
    header::{InvalidHeaderValue, AUTHORIZATION},
    HeaderValue,
};
use onebot_connect_interface::{
    client::{ActionArgs, ClosedReason, Command, Connect, RecvMessage},
    ConfigError, Error,
};
use onebot_types::ob12::{self, event::Event};
use serde_json::Value as Json;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, client::IntoClientRequest, Error as WsError},
    MaybeTlsStream, WebSocketStream,
};

pub type WebSocketConn = WebSocketStream<MaybeTlsStream<TcpStream>>;

use crate::client::generate_echo;

use super::{ActionMap, RxMessageSource, TxClient};

#[derive(Debug, Error)]
pub enum WsConnectError {
    #[error(transparent)]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error(transparent)]
    HeaderValue(#[from] InvalidHeaderValue),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("ws closed")]
    ConnectionClosed,
    #[error("communication channel closed")]
    ChannelClosed,
}

/// OneBot Connect Websocket Generator
pub struct WSConnect<R>
where
    R: IntoClientRequest + Unpin,
{
    req: R,
    access_token: Option<String>,
}

impl<R: IntoClientRequest + Unpin> WSConnect<R> {
    pub fn new(req: R) -> Self {
        Self {
            req,
            access_token: None,
        }
    }
}

impl<R: IntoClientRequest + Unpin> Connect for WSConnect<R> {
    type Err = WsConnectError;
    type Message = ();
    type Client = TxClient;
    type Source = RxMessageSource;

    async fn connect(
        self,
    ) -> Result<(Self::Source, Self::Client, Self::Message), Self::Err> {
        let mut req = self.req.into_client_request()?;
        if let Some(token) = self.access_token {
            req.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }
        let (ws, _) = connect_async(req).await?;
        let (_tasks, WSConnectionHandle { msg_rx, cmd_tx }) = WSTaskHandle::create(ws);
        Ok((RxMessageSource::new(msg_rx), TxClient::new(cmd_tx), ()))
    }

    fn with_authorization(self, access_token: impl AsRef<str>) -> Self {
        Self {
            access_token: Some(access_token.as_ref().to_owned()),
            ..self
        }
    }
}

enum Signal {
    Close(oneshot::Sender<Result<(), String>>),
}
enum RecvData {
    Event(Event),
    /// Response data and `echo`
    Response((ob12::action::RespData, String)),
}

/// WebSocket connection task handle
#[allow(unused)]
pub(crate) struct WSTaskHandle {
    pub recv_handle: JoinHandle<Result<(), WsConnectError>>,
    pub send_handle: JoinHandle<Result<(), WsConnectError>>,
    pub manage_handle: JoinHandle<Result<(), WsConnectError>>,
}

/// Wrapper for `RxMessageSource` and `TxClient`
pub(crate) struct WSConnectionHandle {
    pub msg_rx: mpsc::Receiver<RecvMessage>,
    pub cmd_tx: mpsc::Sender<Command>,
}

#[derive(Error, Debug)]
pub enum RecvError {
    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),
    #[error("invalid data type: {0}")]
    InvalidData(String),
}

impl RecvError {
    pub fn invalid_data(msg: impl AsRef<str>) -> RecvError {
        Self::InvalidData(msg.as_ref().to_owned())
    }
}

pub(crate) trait WsStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T> WsStream for T where T: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl WSTaskHandle {
    pub(crate) fn create<T>(ws: WebSocketStream<T>) -> (Self, WSConnectionHandle)
    where
        T: WsStream,
    {
        let (msg_tx, msg_rx) = mpsc::channel(8);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);

        let (write, read) = ws.split();

        let (signal_tx_recv, signal_rx_recv) = mpsc::channel(4);
        let (signal_tx_send, signal_rx_send) = mpsc::channel(4);

        let (recv_data_tx, recv_data_rx) = mpsc::channel(8);
        let (action_tx, action_rx) = mpsc::channel(8);

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

    async fn handle_action(
        args: ActionArgs,
        map: &mut ActionMap,
        action_tx: &mpsc::Sender<Vec<u8>>,
    ) {
        let ActionArgs {
            action,
            self_,
            resp_tx,
        } = args;
        let echo = generate_echo(8, &map);

        let res = serde_json::to_vec(&ob12::action::Action {
            action,
            echo: Some(echo.clone()),
            self_,
        });
        match res {
            Ok(data) => {
                map.insert(echo, resp_tx);
                action_tx.send(data).await.unwrap();
            }
            Err(e) => {
                resp_tx.send(Err(Error::other(e))).unwrap();
            }
        }
    }

    #[inline(always)]
    async fn handle_close(
        signal_tx_send: &mpsc::Sender<Signal>,
        signal_tx_recv: &mpsc::Sender<Signal>,
        tx: oneshot::Sender<Result<ClosedReason, String>>,
    ) {
        let (close_tx_recv, close_rx_recv) = oneshot::channel();
        let (close_tx_send, close_rx_send) = oneshot::channel();
        signal_tx_recv
            .send(Signal::Close(close_tx_recv))
            .await
            .unwrap();
        signal_tx_send
            .send(Signal::Close(close_tx_send))
            .await
            .unwrap();

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
        action_map: &mut ActionMap,
        action_tx: &mpsc::Sender<Vec<u8>>,
        signal_tx_send: &mpsc::Sender<Signal>,
        signal_tx_recv: &mpsc::Sender<Signal>,
    ) {
        match command {
            Command::Action(args) => {
                Self::handle_action(args, action_map, action_tx).await;
            }
            Command::Close(tx) => {
                Self::handle_close(signal_tx_send, signal_tx_recv, tx).await;
            }
            Command::GetConfig(_, tx) => tx.send(None).unwrap(),
            Command::SetConfig((key, _), tx) => tx.send(Err(ConfigError::UnknownKey(key))).unwrap(),
        }
    }

    async fn manage_task(
        msg_tx: mpsc::Sender<RecvMessage>,
        mut cmd_rx: mpsc::Receiver<Command>,
        signal_tx_recv: mpsc::Sender<Signal>,
        signal_tx_send: mpsc::Sender<Signal>,
        mut recv_data_rx: mpsc::Receiver<RecvData>,
        action_tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<(), WsConnectError> {
        let mut action_map = ActionMap::default();
        loop {
            tokio::select! {
                command = cmd_rx.recv() => {
                    let Some(command) = command else {
                        break Err(WsConnectError::ChannelClosed)
                    };
                    Self::handle_command(
                        command,
                        &mut action_map,
                        &action_tx,
                        &signal_tx_send,
                        &signal_tx_recv
                    ).await;
                }
                recv_data = recv_data_rx.recv() => {
                    let Some(data) = recv_data else {
                        break Err(WsConnectError::ChannelClosed)
                    };

                    match data {
                        RecvData::Event(event) => {
                            msg_tx.send(RecvMessage::Event(event)).await.unwrap()
                        },
                        RecvData::Response((resp, echo)) => {
                            if let Some(resp_tx) = action_map.remove(&echo) {
                                if resp.is_success() {
                                    resp_tx.send(Ok(resp.data)).unwrap();
                                } else {
                                    resp_tx.send(Err(Error::Resp(resp.into()))).unwrap();
                                }
                            }
                        },
                    }
                }
            }
        }
    }

    async fn send_task<T>(
        mut stream: SplitSink<WebSocketStream<T>, tokio_tungstenite::tungstenite::Message>,
        mut action_rx: mpsc::Receiver<Vec<u8>>,
        mut signal_rx: mpsc::Receiver<Signal>,
    ) -> Result<(), WsConnectError>
    where
        T: WsStream,
    {
        loop {
            tokio::select! {
                action = action_rx.recv() => {
                    let Some(action) = action else {
                        break Err(WsConnectError::ChannelClosed)
                    };

                    match stream.send(
                        tungstenite::Message::from(action)
                    ).await {
                        Ok(_) => {},
                        Err(WsError::ConnectionClosed) => {
                            break Err(WsConnectError::ConnectionClosed)
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

    fn parse_data(data: Vec<u8>) -> Result<RecvData, RecvError> {
        let Json::Object(data) = serde_json::from_slice::<Json>(&data)? else {
            return Err(RecvError::invalid_data("expected object"));
        };
        if data.get("echo").is_some() {
            Ok(RecvData::Response(serde_json::from_value(Json::Object(
                data,
            ))?))
        } else if data.get("type").is_some() {
            Ok(RecvData::Event(serde_json::from_value(Json::Object(data))?))
        } else {
            Err(RecvError::invalid_data("unknown data type"))
        }
    }

    async fn recv_task<T>(
        mut stream: SplitStream<WebSocketStream<T>>,
        data_tx: mpsc::Sender<RecvData>,
        mut signal_rx: mpsc::Receiver<Signal>,
    ) -> Result<(), WsConnectError>
    where
        T: WsStream,
    {
        loop {
            tokio::select! {
                data = stream.next() => {
                    let Some(data) = data else {
                        break Err(WsConnectError::ConnectionClosed)
                    };

                    match data {
                        Ok(msg) => match Self::parse_data(msg.into()) {
                            Ok(data) => {
                                data_tx.send(data).await.unwrap();
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
