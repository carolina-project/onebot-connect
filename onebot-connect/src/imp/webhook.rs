use std::sync::Arc;

use ::http::{header::*, HeaderMap, HeaderValue};
use onebot_connect_interface::imp::{Action, Create};

use super::*;

type MessageTx = mpsc::UnboundedSender<RecvMessage>;

pub struct WebhookCreate {
    url: String,
    user_agent: String,
    auth_header: Option<String>,
    impl_name: String,
}

impl WebhookCreate {
    pub fn new(url: impl Into<String>) -> Self {
        Self::with_config(url, "OneBot/12 (Webhook) OneBot-Connect-Rust/0.1.0", "rs")
    }

    pub fn with_config(
        url: impl Into<String>,
        user_agent: impl Into<String>,
        impl_name: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            user_agent: user_agent.into(),
            auth_header: None,
            impl_name: impl_name.into(),
        }
    }
}

impl Create for WebhookCreate {
    type Source = RxMessageSource;
    type Error = OCError;
    type Provider = WebhookImplProvider;
    type Message = ();

    async fn create(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let mut headers = HeaderMap::new();
        if let Some(header) = self.auth_header {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&header).map_err(OCError::other)?,
            );
        }
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&self.user_agent).map_err(OCError::other)?,
        );
        headers.insert(
            "X-OneBot-Version",
            HeaderValue::from_str("12").map_err(OCError::other)?,
        );
        headers.insert(
            "X-Impl",
            HeaderValue::from_str(&self.impl_name).map_err(OCError::other)?,
        );
        let http = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()
            .map_err(OCError::other)?;

        let (msg_tx, msg_rx) = mpsc::unbounded_channel();

        Ok((
            RxMessageSource::new(msg_rx),
            WebhookImplProvider::new(self.url, msg_tx, http),
            (),
        ))
    }

    fn with_authorization(mut self, access_token: impl Into<String>) -> Self {
        self.auth_header = Some(format!("Bearer {}", access_token.into()));
        self
    }
}

struct WebhookImplInner {
    url: String,
    http: reqwest::Client,
}

#[derive(Clone)]
pub struct WebhookImpl {
    inner: Arc<WebhookImplInner>,
    msg_tx: MessageTx,
}

impl OBImpl for WebhookImpl {
    fn respond_supported(&self) -> bool {
        false
    }

    async fn send_event_impl(&self, event: RawEvent) -> Result<(), OCError> {
        let resp: Vec<Action> = self
            .inner
            .http
            .post(&self.inner.url)
            .json(&event)
            .send()
            .await
            .map_err(OCError::other)?
            .json()
            .await
            .map_err(OCError::other)?;

        for ele in resp {
            self.msg_tx
                .send(RecvMessage::Action(ele))
                .map_err(OCError::closed)?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<(), OCError> {
        Ok(())
    }

    async fn respond_impl(&self, _: ActionEcho, _: RespArgs) -> Result<(), OCError> {
        Err(OCError::not_supported("respond action"))
    }
}

pub struct WebhookImplProvider {
    impl_inner: Arc<WebhookImplInner>,
    msg_tx: MessageTx,
}

impl WebhookImplProvider {
    fn new(url: impl Into<String>, msg_tx: MessageTx, http: reqwest::Client) -> Self {
        Self {
            impl_inner: WebhookImplInner {
                url: url.into(),
                http,
            }
            .into(),
            msg_tx,
        }
    }
}

impl OBImplProvider for WebhookImplProvider {
    type Output = WebhookImpl;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        Ok(WebhookImpl {
            inner: self.impl_inner.clone(),
            msg_tx: self.msg_tx.clone(),
        })
    }
}
