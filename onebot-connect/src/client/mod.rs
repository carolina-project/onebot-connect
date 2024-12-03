use fxhash::FxHashMap;
use onebot_connect_interface::{
    client::{ActionArgs, Client, Command, MessageSource, RecvMessage},
    ActionResult,
};
use rand::Rng;
use tokio::sync::{mpsc, oneshot};

pub mod compat;
pub mod http;
pub mod webhook;

#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub mod ws_re;

pub(crate) static ACTION_ECHO_CHARSET: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

pub(crate) type ActionMap = FxHashMap<String, oneshot::Sender<ActionResult<serde_value::Value>>>;

pub fn generate_echo(len: usize, map: &ActionMap) -> String {
    let mut rng = rand::thread_rng();

    loop {
        let echo: String = (0..len)
            .map(|_| {
                let idx = rng.gen_range(0..ACTION_ECHO_CHARSET.len());
                ACTION_ECHO_CHARSET.chars().nth(idx).unwrap()
            })
            .collect();
        if !map.contains_key(&echo) {
            break echo;
        }
    }
}

pub struct RxMessageSource {
    rx: mpsc::Receiver<RecvMessage>,
}

impl RxMessageSource {
    pub fn new(rx: mpsc::Receiver<RecvMessage>) -> Self {
        Self { rx }
    }
}

impl MessageSource for RxMessageSource {
    fn poll_event(&mut self) -> impl std::future::Future<Output = Option<RecvMessage>> + Send + '_ {
        self.rx.recv()
    }
}

pub struct TxClient {
    tx: mpsc::Sender<Command>,
}

impl TxClient {
    pub fn new(tx: mpsc::Sender<Command>) -> Self {
        Self { tx }
    }
}

impl Client for TxClient {
    async fn send_action_impl(
        &self,
        action: onebot_types::ob12::action::ActionType,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<serde_value::Value, onebot_connect_interface::Error> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Action(ActionArgs {
                action,
                self_,
                resp_tx: tx,
            }))
            .await
            .unwrap();
        rx.await.unwrap()
    }

    async fn close_impl(&mut self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Command::Close(tx)).await.unwrap();

        rx.await.unwrap().map(|_| ())
    }
}
