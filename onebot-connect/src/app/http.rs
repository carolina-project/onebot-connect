use std::{collections::VecDeque, sync::Arc, time::Duration};

use ::http::{header::AUTHORIZATION, HeaderMap, HeaderValue};
use onebot_connect_interface::app::{AppExt, Connect};
use onebot_types::ob12::{
    action::{RawAction, RespData, RespError},
    event::RawEvent,
};

use super::*;

pub struct HttpMessageSource {
    client: HttpApp,
    timeout: i64,
    limit: i64,
    interval_ms: u64,
    events: VecDeque<RawEvent>,
}

impl HttpMessageSource {
    pub fn new(client: HttpApp) -> Self {
        Self::with_setting(client, 0, 0, 500)
    }

    pub fn with_setting(client: HttpApp, timeout: i64, limit: i64, interval_ms: u64) -> Self {
        Self {
            client,
            timeout,
            limit,
            interval_ms,
            events: VecDeque::new(),
        }
    }
}

impl MessageSource for HttpMessageSource {
    async fn poll_message(&mut self) -> Option<RecvMessage> {
        if let Some(event) = self.events.pop_front() {
            return Some(RecvMessage::Event(event));
        }

        loop {
            match self
                .client
                .get_latest_events(self.limit, self.timeout, None)
                .await
            {
                Ok(mut resp) => {
                    if resp.len() > 0 {
                        let first = resp.remove(0);
                        self.events.extend(resp);
                        return Some(RecvMessage::Event(first));
                    }
                }
                Err(e) => {
                    log::error!("error while polling events: {}", e);
                }
            }

            tokio::time::sleep(Duration::from_millis(self.interval_ms)).await;
        }
    }
}

pub(crate) struct HttpInner {
    pub url: String,
    pub http: reqwest::Client,
}

pub(crate) type HttpInnerShared = Arc<HttpInner>;

#[derive(Clone)]
pub struct HttpApp {
    inner: HttpInnerShared,
}

impl OBApp for HttpApp {
    async fn send_action_impl(
        &self,
        action: ActionDetail,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<serde_value::Value>, OCError> {
        let http_resp = self
            .inner
            .http
            .post(&self.inner.url)
            .json(&RawAction {
                action,
                echo: None,
                self_,
            })
            .send()
            .await
            .map_err(OCError::other)?;
        if !http_resp.status().is_success() {
            return Err(OCError::Resp(RespError {
                retcode: http_resp.status().as_u16().into(),
                message: "".into(),
                echo: None,
            }));
        } else {
            let response = http_resp.json::<RespData>().await.map_err(OCError::other)?;

            if response.is_success() {
                Ok(Some(response.data))
            } else {
                Err(OCError::Resp(response.into()))
            }
        }
    }

    async fn close(&self) -> Result<(), OCError> {
        Ok(())
    }

    fn clone_app(&self) -> Self {
        self.clone()
    }
}

pub struct HttpAppProvider {
    app: HttpApp,
}

impl HttpAppProvider {
    pub fn new(http: reqwest::Client, url: impl Into<String>) -> Self {
        let app = HttpApp {
            inner: HttpInner {
                url: url.into(),
                http,
            }
            .into(),
        };
        Self { app }
    }
}
impl OBAppProvider for HttpAppProvider {
    type Output = HttpApp;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        Ok(self.app.clone())
    }
}

pub struct HttpConnect {
    access_token: Option<String>,
    url: String,
}
impl HttpConnect {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            access_token: None,
            url: url.into(),
        }
    }
}

impl Connect for HttpConnect {
    type Source = HttpMessageSource;
    type Provider = HttpAppProvider;
    type Message = ();
    type Error = crate::Error;

    fn with_authorization(self, access_token: impl Into<String>) -> Self {
        Self {
            access_token: Some(access_token.into()),
            ..self
        }
    }

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let mut headers = HeaderMap::new();
        if let Some(token) = self.access_token {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", token))?,
            );
        }

        let http = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()?;
        let mut provider = HttpAppProvider::new(http, self.url);
        Ok((HttpMessageSource::new(provider.provide()?), provider, ()))
    }
}
