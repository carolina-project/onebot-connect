use std::net::SocketAddr;

use http::{header::AUTHORIZATION, StatusCode};
use onebot_connect_interface::client::Connect;
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_tungstenite::WebSocketStream;

use super::{
    ws::{self, WSTaskHandle},
    RxMessageSource, TxClient,
};

pub type WebSocketConn = WebSocketStream<TcpStream>;

pub struct WSReConnect<A: ToSocketAddrs> {
    addr: A,
    access_token: Option<String>,
}

impl<A: ToSocketAddrs> Connect for WSReConnect<A> {
    type Err = ws::WsConnectError;
    type Message = SocketAddr;
    type Source = RxMessageSource;
    type Client = TxClient;

    async fn connect(
        self,
    ) -> Result<(Self::Source, Self::Client, Self::Message), Self::Err> {
        let listener = TcpListener::bind(self.addr).await?;

        let (stream, addr) = listener.accept().await?;
        let ws_stream = tokio_tungstenite::accept_hdr_async(
            stream,
            |req: &http::Request<()>, response: http::Response<()>| {
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
                    Err(http::Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(Some("Invalid access token".into()))
                        .unwrap())
                }
            },
        )
        .await?;

        let (_task_handle, conn_handle) = WSTaskHandle::create(ws_stream);

        Ok((
            RxMessageSource::new(conn_handle.msg_rx),
            TxClient::new(conn_handle.cmd_tx),
            addr
        ))
    }

    fn with_authorization(self, access_token: impl AsRef<str>) -> Self {
        Self {
            access_token: Some(access_token.as_ref().to_string()),
            ..self
        }
    }
}
