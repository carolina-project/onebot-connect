use std::net::SocketAddr;

use ::http::{StatusCode, header::*};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_tungstenite::WebSocketStream;
use ws_re::WSHandler;

use crate::common::ws::WSTask;

use super::*;

extern crate http as http_lib;

pub type WebSocketConn = WebSocketStream<TcpStream>;

pub struct WSReConnect<A: ToSocketAddrs> {
    addr: A,
    access_token: Option<String>,
}

impl<A: ToSocketAddrs> Create for WSReConnect<A> {
    type Error = crate::Error;
    type Message = SocketAddr;
    type Source = RxMessageSource;
    type Provider = TxImplProvider;

    async fn create(self) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        let listener = TcpListener::bind(self.addr).await?;

        let (stream, addr) = listener.accept().await?;
        let ws = tokio_tungstenite::accept_hdr_async(
            stream,
            |req: &http_lib::Request<()>, response: http_lib::Response<()>| {
                let Some(token) = &self.access_token else {
                    return Ok(response);
                };

                if req
                    .headers()
                    .get(AUTHORIZATION)
                    .map(|r| {
                        r.to_str()
                            .map(|h_token| h_token == token)
                            .unwrap_or_default()
                    })
                    .unwrap_or_default()
                {
                    Ok(response)
                } else {
                    Err(http_lib::Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(Some("Invalid access token".into()))
                        .unwrap())
                }
            },
        )
        .await?;

        let (cmd_tx, msg_rx) = WSTask::create(ws, WSHandler::default()).await;

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
