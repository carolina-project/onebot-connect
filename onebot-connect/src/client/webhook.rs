use super::*;
use hyper::{body::Incoming as IncomingBody, service::Service};
use onebot_connect_interface::ConfigError;
use onebot_types::ob12::action::Action;
use parking_lot::Mutex;
use std::{future::Future, pin::Pin, sync::Arc};

type ActionsSender = oneshot::Sender<Vec<Action>>;

pub struct WebhookServer {
    access_token: Option<String>,
}

impl Service<IncomingBody> for WebhookServer {
    type Response = hyper::Response<IncomingBody>;

    type Error = hyper::Error;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: IncomingBody) -> Self::Future {
        Box::pin(async move {

            Ok(())
        })
    }
}

pub struct WebhookClient {
    action_tx: oneshot::Sender<Vec<Action>>,
    actions: Arc<Mutex<Vec<Action>>>,
}
impl WebhookClient {
    pub fn new(action_tx: ActionsSender) -> Self {
        Self {
            action_tx,
            actions: Default::default(),
        }
    }
}
impl Client for WebhookClient {
    fn response_supported(&self) -> bool {
        false
    }

    async fn send_action_impl(
        &self,
        action: onebot_types::ob12::action::ActionType,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<serde_value::Value>, OCError> {
        self.actions.lock().push(Action {
            action,
            echo: None,
            self_,
        });
        Ok(None)
    }

    async fn get_config<'a, 'b: 'a>(
        &'a self,
        _: impl AsRef<str> + Send + 'b,
    ) -> Option<serde_value::Value> {
        None
    }

    async fn set_config<'a, 'b: 'a>(
        &'a self,
        key: impl AsRef<str> + Send + 'b,
        _: serde_value::Value,
    ) -> Result<(), ConfigError> {
        Err(ConfigError::UnknownKey(key.as_ref().into()))
    }

    async fn release(self) -> Result<(), OCError>
        where
            Self: Sized, {
        self.action_tx.send(std::mem::take(&mut self.actions.lock())).unwrap();
        Ok(())
    }
}

pub struct WebhookClientProvider {
    action_tx: Option<ActionsSender>,
}
impl WebhookClientProvider {
    pub fn set_sender(&mut self, sender: ActionsSender) {
        self.action_tx = Some(sender);
    }
}
impl ClientProvider for WebhookClientProvider {
    type Output = WebhookClient;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        if let Some(tx) = self.action_tx.take() {
            Ok(WebhookClient::new(tx))
        } else {
            Err(OCError::not_supported("send action actively not supported"))
        }
    }
}
