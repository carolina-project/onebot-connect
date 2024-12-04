use onebot_connect_interface::client::Connect;
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_tungstenite::WebSocketStream;

use super::ws;

pub type WebSocketConn = WebSocketStream<TcpStream>;

pub struct WSReConnect<A: ToSocketAddrs> {
    addr: A
}

impl<A: ToSocketAddrs> Connect for WSReConnect<A> {
    type CErr = ws::WsConnectError;
    async fn connect(
        self,
    ) -> Result<(impl Into<Self::Source>, impl Into<Self::Client>), Self::CErr> {
        let listener = TcpListener::bind(self.addr).await?;

        let (stream, addr) = listener.accept().await?;
        let ws_stream = tokio_tungstenite::accept_async(stream).await?;

        ()
    }
}

struct WSReTaskHandle {

}
