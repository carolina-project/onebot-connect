use dashmap::DashMap;
use onebot_connect_interface::Error as OCError;
use onebot_types::ob12::action::{ActionDetail, RespData};
use rand::Rng;
use tokio::sync::{mpsc, oneshot};

pub use onebot_connect_interface::app::*;

#[cfg(feature = "compat")]
pub mod compat;

#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "hyper")]
pub mod webhook;

#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub mod ws_re;

#[cfg(feature = "http")]
pub use http::*;
#[cfg(feature = "hyper")]
pub use webhook::*;
#[cfg(feature = "ws")]
pub use {ws::WSConnect, ws_re::WSReConnect};

pub(crate) static ACTION_ECHO_CHARSET: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

pub fn generate_echo<V>(len: usize, map: &DashMap<String, V>) -> String {
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
impl OBAppProvider for TxAppProvider {
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
impl OBApp for TxAppSide {
    async fn send_action_impl(
        &self,
        action: ActionDetail,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<RespData>, OCError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Action(ActionArgs { action, self_ }, tx))
            .map_err(OCError::closed)?;
        rx.await.map_err(OCError::closed)?.map(|r| Some(r))
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
        self.tx.send(Command::SetConfig(entry, tx))?;
        Ok(rx.await.map_err(OCError::closed)??)
    }

    async fn close(&self) -> Result<(), OCError> {
        Ok(self.tx.send(Command::Close)?)
    }

    fn clone_app(&self) -> Self {
        self.clone()
    }
}
