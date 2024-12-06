use std::{collections::VecDeque, sync::Arc, time::Duration};

use ::http::{header::AUTHORIZATION, HeaderMap, HeaderValue};
use onebot_connect_interface::app::{AppExt, Connect};
use onebot_types::ob12::{
    action::{Action, RespData, RespError},
    event::Event,
};
use serde_value::Value;

use super::*;

pub struct HttpMessageSource {
    client: HttpApp,
    timeout: i64,
    limit: i64,
    interval_ms: u64,
    events: VecDeque<Event>,
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

pub struct HttpApp {
    http: reqwest::Client,
    url: Arc<String>,
}

impl HttpApp {
    pub fn new(conn: reqwest::Client, url: Arc<String>) -> Self {
        Self { http: conn, url }
    }
}

impl App for HttpApp {
    async fn send_action_impl(
        &self,
        action: onebot_types::ob12::action::ActionType,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<Value>, OCError> {
        let http_resp = self
            .http
            .post(self.url.as_ref())
            .json(&Action {
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
                message: "http error".into(),
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
}

pub struct HttpAppProvider {
    http: reqwest::Client,
    url: Arc<String>,
}

impl HttpAppProvider {
    pub fn new(http: reqwest::Client, url: Arc<String>) -> Self {
        Self { http, url }
    }
}
impl AppProvider for HttpAppProvider {
    type Output = HttpApp;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        Ok(HttpApp::new(self.http.clone(), self.url.clone()))
    }
}

pub struct HttpConnect {
    access_token: Option<String>,
    url: String,
}
impl HttpConnect {
    pub fn new(url: String) -> Self {
        Self {
            access_token: None,
            url,
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
        let mut provider = HttpAppProvider::new(http, Arc::new(self.url));
        Ok((HttpMessageSource::new(provider.provide()?), provider, ()))
    }
}
