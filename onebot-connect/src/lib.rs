use futures::{channel::mpsc, StreamExt};
use onebot_connect_interface::client::{MessageSource, RecvMessage};

pub mod client;
pub mod compat;

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
        self.rx.next()
    }
}
