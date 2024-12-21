use std::env;

use clap::{Parser, Subcommand};
use onebot_connect::app::{
    compat::{http::OB11HttpConnect, ws::OB11WSConnect, ws_re::OB11WSReConnect},
    HttpConnect, WSConnect, WSReConnect, WebhookConnect,
};

#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    conn: Option<Connection>,
}

#[derive(Subcommand, Debug)]
enum Connection {
    Ws {
        #[clap(long, short)]
        addr: String,
        #[clap(long)]
        ob11: bool,
    },
    WsRe {
        #[clap(long, short)]
        port: u16,
        #[clap(long)]
        ob11: bool,
    },
    Http {
        #[clap(long, short)]
        url: String,
    },
    Webhook {
        #[clap(long, short)]
        port: u16,
    },
    OB11Http {
        #[clap(long, short)]
        http_url: String,
        #[clap(long, short)]
        post_port: u16,
    },
}

impl Default for Connection {
    fn default() -> Self {
        Self::WsRe {
            port: 5001,
            ob11: true,
        }
    }
}

async fn run_conn(conn: Connection) -> eyre::Result<()> {
    use onebot_connect::app::{Connect, MessageSource};
    log::info!("using connection: {conn:?}");
    match conn {
        Connection::Ws { addr, ob11 } => {
            let mut msg_src = if ob11 {
                OB11WSConnect::new(addr).connect().await?.0
            } else {
                WSConnect::new(addr).connect().await?.0
            };
            while let Some(msg) = msg_src.poll_message().await {
                log::info!("recv: {msg:?}");
            }
        }
        Connection::WsRe { port, ob11 } => {
            let mut msg_src = if ob11 {
                OB11WSReConnect::new(format!("0.0.0.0:{port}"))
                    .connect()
                    .await?
                    .0
            } else {
                WSReConnect::new(format!("localhost:{port}"))
                    .connect()
                    .await?
                    .0
            };
            while let Some(msg) = msg_src.poll_message().await {
                log::info!("recv: {msg:?}");
            }
        }
        Connection::Http { url } => {
            let mut msg_src = HttpConnect::new(url).connect().await?.0;
            while let Some(msg) = msg_src.poll_message().await {
                log::info!("recv: {msg:?}");
            }
        }
        Connection::Webhook { port } => {
            let mut msg_src = WebhookConnect::new(([0, 0, 0, 0], port))
                .connect()
                .await?
                .0;

            while let Some(msg) = msg_src.poll_message().await {
                log::info!("recv: {msg:?}");
            }
        }
        Connection::OB11Http {
            http_url,
            post_port,
        } => {
            let mut msg_src = OB11HttpConnect::new(([0, 0, 0, 0], post_port), http_url)
                .connect()
                .await?
                .0;

            while let Some(msg) = msg_src.poll_message().await {
                log::info!("recv: {msg:?}");
            }
        }
    }

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> eyre::Result<()> {
    let args = Args::parse();
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info")
    }
    env_logger::init();

    run_conn(args.conn.unwrap_or_default()).await
}
