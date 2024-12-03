use futures::{channel::mpsc, StreamExt};
use onebot_connect_interface::client::EventSource;
use onebot_types::ob12::event::Event;

pub mod client;
pub mod compat;

pub struct RxEventSource {
    rx: mpsc::Receiver<Event>,
}

impl RxEventSource {
    pub fn new(rx: mpsc::Receiver<Event>) -> Self {
        Self { rx }
    }
}

impl EventSource for RxEventSource {
    fn poll_event(&mut self) -> impl std::future::Future<Output = Option<Event>> + Send + '_ {
        self.rx.next()
    }
}
