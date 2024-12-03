use onebot_connect_interface::client::Connect;

pub struct WebSocketConnect {
    url: String,
}

impl Connect for WebSocketConnect {
    type CErr;

    type Client;

    type ESource;

    fn connect(self) -> Result<(Self::ESource, Self::Client), Self::CErr> {
        todo!()
    }
}
