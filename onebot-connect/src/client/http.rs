use std::{sync::Arc, time::Duration};

use ::http::{header::AUTHORIZATION, HeaderMap, HeaderValue};
use onebot_connect_interface::{
    client::{ClientExt, Connect},
    ConfigError,
};
use onebot_types::ob12::action::{Action, RespData, RespError};
use serde_value::Value;

use super::*;

pub struct HttpMessageSource {
    client: HttpClient,
    timeout: i64,
    limit: i64,
    interval: f64,
}

impl HttpMessageSource {
    pub fn new(client: HttpClient) -> Self {
        Self::with_setting(client, 0, 0, 0.5)
    }

    pub fn with_setting(client: HttpClient, timeout: i64, limit: i64, interval: f64) -> Self {
        Self {
            client,
            timeout,
            limit,
            interval,
        }
    }
}

impl MessageSource for HttpMessageSource {
    async fn poll_message(&mut self) -> Option<RecvMessage> {
        loop {
            match self
                .client
                .get_latest_events(self.limit, self.timeout, None)
                .await
            {
                Ok(resp) => return Some(RecvMessage::Event(resp)),
                Err(e) => {
                    log::error!("error while polling events: {}", e);
                }
            }

            tokio::time::sleep(Duration::from_secs_f64(self.interval)).await;
        }
    }
}

pub struct HttpClient {
    http: reqwest::Client,
    url: Arc<String>,
}

impl HttpClient {
    pub fn new(conn: reqwest::Client, url: Arc<String>) -> Self {
        Self { http: conn, url }
    }
}

impl Client for HttpClient {
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

    async fn get_config<'a, 'b: 'a>(
        &'a self,
        _key: impl AsRef<str> + Send + 'b,
    ) -> Option<serde_value::Value> {
        None
    }

    async fn set_config<'a, 'b: 'a>(
        &'a self,
        key: impl AsRef<str> + Send + 'b,
        _value: serde_value::Value,
    ) -> Result<(), onebot_connect_interface::ConfigError> {
        Err(ConfigError::UnknownKey(key.as_ref().into()))
    }
}

pub struct HttpClientProvider {
    http: reqwest::Client,
    url: Arc<String>,
}

impl HttpClientProvider {
    pub fn new(http: reqwest::Client, url: Arc<String>) -> Self {
        Self { http, url }
    }
}
impl ClientProvider for HttpClientProvider {
    type Output = HttpClient;

    fn provide(&mut self) -> Result<Self::Output, OCError> {
        Ok(HttpClient::new(self.http.clone(), self.url.clone()))
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
    type Provider = HttpClientProvider;
    type Message = ();
    type Error = crate::Error;

    fn with_authorization(self, access_token: impl AsRef<str>) -> Self {
        Self {
            access_token: Some(access_token.as_ref().into()),
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
        let mut provider = HttpClientProvider::new(http, Arc::new(self.url));
        Ok((HttpMessageSource::new(provider.provide()?), provider, ()))
    }
}
