use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use http::{header::AUTHORIZATION, HeaderValue};
use onebot_connect_interface::{
    app::{ActionArgs, ActionResponder, Command, Connect, RecvMessage}, ClosedReason, ConfigError, Error as OCError
};
use onebot_types::ob12::{self, event::Event};
use serde_json::Value as Json;
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

use crate::app::generate_echo;

use super::{ActionMap, RxMessageSource, TxAppProvider};

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
    type Error = crate::Error;
    type Message = ();
    type Provider = TxAppProvider;
    type Source = RxMessageSource;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let mut req = self.req.into_client_request()?;
        if let Some(token) = self.access_token {
            req.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }
        let (ws, _) = connect_async(req).await?;
        let (_tasks, WSConnectionHandle { msg_rx, cmd_tx }) = WSTaskHandle::create(ws);
        Ok((
            RxMessageSource::new(msg_rx),
            TxAppProvider::new(cmd_tx),
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
    Event(Event),
    /// Response data and `echo`
    Response((ob12::action::RespData, String)),
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

#[derive(thiserror::Error, Debug)]
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

    async fn handle_action(
        args: ActionArgs,
        map: &mut ActionMap,
        action_tx: &mpsc::UnboundedSender<Vec<u8>>,
        responder: ActionResponder,
    ) {
        let ActionArgs { action, self_ } = args;
        let echo = generate_echo(8, &map);

        let res = serde_json::to_vec(&ob12::action::Action {
            action,
            echo: Some(echo.clone()),
            self_,
        });
        match res {
            Ok(data) => {
                map.insert(echo, responder);
                action_tx.send(data).unwrap();
            }
            Err(e) => {
                responder.send(Err(OCError::other(e))).unwrap();
            }
        }
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
        action_map: &mut ActionMap,
        action_tx: &mpsc::UnboundedSender<Vec<u8>>,
        signal_tx_send: &mpsc::UnboundedSender<Signal>,
        signal_tx_recv: &mpsc::UnboundedSender<Signal>,
    ) {
        match command {
            Command::Action(args, responder) => {
                Self::handle_action(args, action_map, action_tx, responder).await;
            }
            Command::Close(tx) => {
                Self::handle_close(signal_tx_send, signal_tx_recv, tx).await;
            }
            Command::GetConfig(_, tx) => tx.send(None).unwrap(),
            Command::SetConfig((key, _), tx) => tx.send(Err(ConfigError::UnknownKey(key))).unwrap(),
            Command::Respond(_, _) => log::error!("command not supported: respond"),
        }
    }

    async fn manage_task(
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        mut cmd_rx: mpsc::UnboundedReceiver<Command>,
        signal_tx_recv: mpsc::UnboundedSender<Signal>,
        signal_tx_send: mpsc::UnboundedSender<Signal>,
        mut recv_data_rx: mpsc::UnboundedReceiver<RecvData>,
        action_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<(), crate::Error> {
        let mut action_map = ActionMap::default();
        loop {
            tokio::select! {
                command = cmd_rx.recv() => {
                    let Some(command) = command else {
                        break Err(crate::Error::ChannelClosed)
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
                        break Err(crate::Error::ChannelClosed)
                    };

                    match data {
                        RecvData::Event(event) => {
                            msg_tx.send(RecvMessage::Event(event)).unwrap()
                        },
                        RecvData::Response((resp, echo)) => {
                            if let Some(resp_tx) = action_map.remove(&echo) {
                                if resp.is_success() {
                                    resp_tx.send(Ok(resp.data)).unwrap();
                                } else {
                                    resp_tx.send(Err(OCError::Resp(resp.into()))).unwrap();
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
        mut action_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        mut signal_rx: mpsc::UnboundedReceiver<Signal>,
    ) -> Result<(), crate::Error>
    where
        T: WsStream,
    {
        loop {
            tokio::select! {
                action = action_rx.recv() => {
                    let Some(action) = action else {
                        break Err(crate::Error::ChannelClosed)
                    };

                    match stream.send(
                        tungstenite::Message::from(action)
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
