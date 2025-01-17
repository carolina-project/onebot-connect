#[cfg(feature = "hyper")]
pub mod http;
#[cfg(feature = "http")]
pub mod webhook;

#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub mod ws_re;

pub use onebot_connect_interface::imp::*;

use onebot_connect_interface::{Error as OCError, RespArgs};
use onebot_types::ob12::event::RawEvent;
use tokio::sync::mpsc;

type CmdSender = mpsc::UnboundedSender<Command>;
type MessageRecv = mpsc::UnboundedReceiver<RecvMessage>;

pub struct TxImpl {
    tx: CmdSender,
}
impl TxImpl {
    pub fn new(tx: CmdSender) -> Self {
        Self { tx }
    }
}
impl OBImpl for TxImpl {
    async fn send_event_impl(&self, event: RawEvent) -> Result<(), OCError> {
        self.tx.send(Command::Event(event)).map_err(OCError::closed)
    }

    async fn close(&self) -> Result<(), OCError> {
        Ok(self.tx.send(Command::Close)?)
    }

    async fn respond_impl(&self, echo: ActionEcho, data: RespArgs) -> Result<(), OCError> {
        self.tx
            .send(Command::Respond(echo, data))
            .map_err(OCError::closed)
    }
}

pub struct TxImplProvider {
    tx: CmdSender,
}
impl TxImplProvider {
    pub fn new(tx: CmdSender) -> Self {
        Self { tx }
    }
}
impl OBImplProvider for TxImplProvider {
    type Output = TxImpl;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        Ok(TxImpl {
            tx: self.tx.clone(),
        })
    }
}

pub struct RxMessageSource {
    rx: MessageRecv,
}
impl RxMessageSource {
    pub fn new(rx: MessageRecv) -> Self {
        Self { rx }
    }
}
impl MessageSource for RxMessageSource {
    async fn poll_message(&mut self) -> Option<RecvMessage> {
        self.rx.recv().await
    }
}
