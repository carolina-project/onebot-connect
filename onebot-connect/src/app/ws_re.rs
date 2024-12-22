use std::net::SocketAddr;

use onebot_connect_interface::app::Connect;
use tokio::net::ToSocketAddrs;

use crate::common::ws::{wait_for_ws, WSTask};

use super::{ws::WSHandler, RxMessageSource, TxAppProvider};

pub struct WSReConnect<A: ToSocketAddrs> {
    addr: A,
    access_token: Option<String>,
}

impl<A: ToSocketAddrs> WSReConnect<A> {
    pub fn new(addr: A) -> Self {
        Self {
            addr,
            access_token: None,
        }
    }
}

impl<A: ToSocketAddrs + Send> Connect for WSReConnect<A> {
    type Error = crate::Error;
    type Message = SocketAddr;
    type Source = RxMessageSource;
    type Provider = TxAppProvider;

    async fn connect(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let Self { addr, access_token } = self;

        let (ws_stream, addr) = wait_for_ws(addr, access_token.as_deref()).await?;
        let (cmd_tx, msg_rx) = WSTask::create(ws_stream, WSHandler::default()).await;

        Ok((
            RxMessageSource::new(msg_rx),
            TxAppProvider::new(cmd_tx),
            addr,
        ))
    }

    fn with_authorization(self, access_token: impl Into<String>) -> Self {
        Self {
            access_token: Some(access_token.into()),
            ..self
        }
    }
}
