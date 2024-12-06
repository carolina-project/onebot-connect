use std::{future::Future, pin::Pin};

use onebot_types::ob12::{action::Action, event::EventType};
use serde_value::Value;

use crate::Error;

pub trait EventSender {
    fn send_event(
        &mut self,
        event: EventType,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;

    fn close(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
}

pub trait ActionHandler {
    fn handle(action: Action) -> impl Future<Output = Value>;
}
