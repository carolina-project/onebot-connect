use std::net::SocketAddr;

use onebot_connect_interface::imp::Create;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_tungstenite::WebSocketStream;
use ws_re::WSHandler;

use crate::common::ws::{wait_for_ws, WSTask};

use super::*;

extern crate http as http_lib;

pub type WebSocketConn = WebSocketStream<TcpStream>;

pub struct WSCreate<A: ToSocketAddrs> {
    addr: A,
    access_token: Option<String>,
}

impl<A: ToSocketAddrs> WSCreate<A> {
    pub fn new(addr: A) -> Self {
        Self {
            addr,
            access_token: None,
        }
    }
}

impl<A: ToSocketAddrs + Send> Create for WSCreate<A> {
    type Error = crate::Error;
    type Message = SocketAddr;
    type Source = RxMessageSource;
    type Provider = TxImplProvider;

    async fn create(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let Self { addr, access_token } = self;
        log::debug!("waiting for websocket connection...");
        let (ws, addr) = wait_for_ws(addr, access_token.as_deref()).await?;
        log::debug!("connected ({addr})");

        let (cmd_tx, msg_rx) = WSTask::create(ws, WSHandler).await;
        Ok((
            RxMessageSource::new(msg_rx),
            TxImplProvider::new(cmd_tx),
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
