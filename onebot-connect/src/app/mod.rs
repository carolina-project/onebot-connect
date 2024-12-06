use fxhash::FxHashMap;
use onebot_connect_interface::{
    app::{ActionArgs, App, AppProvider, Command, MessageSource, RecvMessage},
    ActionResult, Error as OCError,
};
use rand::Rng;
use tokio::sync::{mpsc, oneshot};

pub mod compat;

#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "http")]
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
    rx: mpsc::UnboundedReceiver<RecvMessage>,
}

impl RxMessageSource {
    pub fn new(rx: mpsc::UnboundedReceiver<RecvMessage>) -> Self {
        Self { rx }
    }
}

impl MessageSource for RxMessageSource {
    fn poll_message(
        &mut self,
    ) -> impl std::future::Future<Output = Option<RecvMessage>> + Send + '_ {
        self.rx.recv()
    }
}

pub struct TxAppProvider {
    tx: mpsc::UnboundedSender<Command>,
}
impl TxAppProvider {
    pub fn new(tx: mpsc::UnboundedSender<Command>) -> Self {
        Self { tx }
    }
}
impl AppProvider for TxAppProvider {
    type Output = TxAppSide;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        Ok(TxAppSide::new(self.tx.clone()))
    }
}

#[derive(Clone)]
pub struct TxAppSide {
    tx: mpsc::UnboundedSender<Command>,
}
impl TxAppSide {
    pub fn new(tx: mpsc::UnboundedSender<Command>) -> Self {
        Self { tx }
    }
}
impl App for TxAppSide {
    async fn send_action_impl(
        &self,
        action: onebot_types::ob12::action::ActionType,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<serde_value::Value>, OCError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Action(ActionArgs { action, self_ }, tx))
            .map_err(OCError::closed)?;
        rx.await.unwrap().map(|r| Some(r))
    }

    async fn get_config<'a, 'b: 'a>(
        &'a self,
        key: impl Into<String> + Send + 'b,
    ) -> Result<Option<serde_value::Value>, OCError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::GetConfig(key.into(), tx))
            .map_err(OCError::closed)?;

        rx.await.map_err(OCError::closed)
    }

    async fn set_config<'a, 'b: 'a>(
        &'a self,
        key: impl Into<String> + Send + 'b,
        value: serde_value::Value,
    ) -> Result<(), OCError> {
        let (tx, rx) = oneshot::channel();
        let entry = (key.into(), value);
        self.tx.send(Command::SetConfig(entry, tx)).unwrap();
        Ok(rx.await.map_err(OCError::closed)??)
    }

    fn clone_app(&self) -> Self {
        self.clone()
    }
}