#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "http")]
pub mod webhook;

#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub mod ws_re;


use onebot_connect_interface::imp::MessageSource;
use onebot_connect_interface::imp::*;
use onebot_connect_interface::Error as OCError;
use onebot_types::ob12::event::Event;
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
impl Impl for TxImpl {
    async fn send_event_impl(&self, event: Event) -> Result<(), OCError> {
        self.tx.send(Command::Event(event)).map_err(OCError::closed)
    }

    async fn respond_action(
        &self,
        echo: ActionEcho,
        value: serde_value::Value,
    ) -> Result<(), OCError> {
        self.tx
            .send(Command::Respond(echo, value))
            .map_err(OCError::closed)
    }
}

pub struct RxMessageSource {
    rx: MessageRecv,
}
impl MessageSource for RxMessageSource {
    async fn poll_message(&mut self) -> Option<RecvMessage> {
        self.rx.recv().await
    }
}
