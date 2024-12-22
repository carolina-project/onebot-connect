use std::net::SocketAddr;

use onebot_connect_interface::app::Connect;
use tokio::net::ToSocketAddrs;

use crate::{
    app::{RxMessageSource, TxAppProvider},
    common::ws::wait_for_ws,
};

use super::ws::connect_ws;

pub struct OB11WSReConnect<A: ToSocketAddrs> {
    addr: A,
    access_token: Option<String>,
}

impl<A: ToSocketAddrs> OB11WSReConnect<A> {
    pub fn new(addr: A) -> Self {
        Self {
            addr,
            access_token: None,
        }
    }
}

impl<A: ToSocketAddrs + Send> Connect for OB11WSReConnect<A> {
    type Error = crate::Error;
    type Message = SocketAddr;
    type Provider = TxAppProvider;
    type Source = RxMessageSource;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let Self { addr, access_token } = self;
        let (ws, addr) = wait_for_ws(addr, access_token.as_deref()).await?;
        let (src, provider, _) = connect_ws(ws).await?;
        Ok((src, provider, addr))
    }

    fn with_authorization(mut self, access_token: impl Into<String>) -> Self {
        self.access_token = Some(access_token.into());
        self
    }
}
