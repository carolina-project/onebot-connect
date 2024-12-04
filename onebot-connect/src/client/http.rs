use onebot_connect_interface::client::Connect;

use super::*;

pub struct HttpClient<'a> {
    http: reqwest::Client,
    url: &'a str,
}

impl<'cl> Client for HttpClient<'cl> {
    async fn send_action_impl(
        &self,
        action: onebot_types::ob12::action::ActionType,
        self_: Option<onebot_types::ob12::BotSelf>,
    ) -> Result<Option<serde_value::Value>, OCError>
    {
        self.http.post(self.url).send().await
    }

    async fn get_config<'a, 'b: 'a>(
        &'a self,
        key: impl AsRef<str> + Send + 'b,
    ) -> Option<serde_value::Value> {
        todo!()
    }

    async fn set_config<'a, 'b: 'a>(
        &'a self,
        key: impl AsRef<str> + Send + 'b,
        value: serde_value::Value,
    ) -> Result<(), onebot_connect_interface::ConfigError>
    {
        todo!()
    }
}

pub struct HttpClientProvider {
    http: reqwest::Client,

}

pub struct HttpConnect {}

impl Connect for HttpConnect {
    type Source = RxMessageSource;
    type Provider = TxClientProvider;
    type Message = ();

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {}
}
