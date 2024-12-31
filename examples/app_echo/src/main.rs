use std::env;

use clap::{Parser, Subcommand};
use eyre::OptionExt;
use onebot_connect::{
    app::{
        compat::{http::OB11HttpConnect, ws::OB11WSConnect, ws_re::OB11WSReConnect},
        HttpConnect, OBApp, RecvMessage, WSConnect, WSReConnect, WebhookConnect,
    },
    ob12::{
        event::{MessageEvent, RawEvent},
        message::Text,
    },
    select_msg, MessageChain,
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

async fn debug_cmd(
    args: impl AsRef<str>,
    event: MessageEvent,
    app: impl OBApp,
) -> eyre::Result<()> {
    let msg = match args.as_ref().trim() {
        "msg" => {
            format!("{event:?}")
        }
        _ => "unknown debug type".into(),
    };

    use onebot_connect::app::AppExt;

    app.send_message(
        event.get_chat_target().ok_or_eyre("unknown chat target")?,
        MessageChain::try_from_msg_trait(Text::new(msg))?,
        None,
    )
    .await?;

    Ok(())
}

async fn handle_msg_event(mut event: MessageEvent, app: impl OBApp) -> eyre::Result<()> {
    let Some(messages) = event.message_mut() else {
        return Ok(());
    };

    if !messages.is_empty() {
        let msg = messages.remove_raw(0);
        select_msg!(msg, {
            Text = text => {
                if text.text.starts_with("!debug") {
                    debug_cmd(&text.text[6..].to_owned(), event, app).await?;
                }
            }
        })
    }

    Ok(())
}

async fn handle_msg(msg: RecvMessage, app: impl OBApp) -> eyre::Result<()> {
    match msg {
        RecvMessage::Event(event) => {
            let RawEvent { event, .. } = event;

            if event.r#type == "message" {
                match MessageEvent::try_from(event) {
                    Ok(event) => handle_msg_event(event, app).await?,
                    Err(e) => log::error!("parse err: {e}"),
                }
            }

            Ok(())
        }
        RecvMessage::Close(_) => Ok(()),
    }
}

async fn run_conn(conn: Connection) -> eyre::Result<()> {
    use onebot_connect::app::{Connect, MessageSource, OBAppProvider};
    log::info!("using connection: {conn:?}");
    match conn {
        Connection::Ws { addr, ob11 } => {
            let (mut msg_src, mut app_prov, _) = if ob11 {
                OB11WSConnect::new(addr).connect().await?
            } else {
                WSConnect::new(addr).connect().await?
            };
            while let Some(msg) = msg_src.poll_message().await {
                handle_msg(msg, app_prov.provide()?).await?;
            }
        }
        Connection::WsRe { port, ob11 } => {
            let (mut msg_src, mut app_prov, _) = if ob11 {
                OB11WSReConnect::new(format!("0.0.0.0:{port}"))
                    .connect()
                    .await?
            } else {
                WSReConnect::new(format!("0.0.0.0:{port}"))
                    .connect()
                    .await?
            };
            while let Some(msg) = msg_src.poll_message().await {
                handle_msg(msg, app_prov.provide()?).await?;
            }
        }
        Connection::Http { url } => {
            let (mut msg_src, mut app_prov, _) = HttpConnect::new(url).connect().await?;
            while let Some(msg) = msg_src.poll_message().await {
                handle_msg(msg, app_prov.provide()?).await?;
            }
        }
        Connection::Webhook { port } => {
            let (mut msg_src, mut app_prov, _) =
                WebhookConnect::new(([0, 0, 0, 0], port)).connect().await?;

            while let Some(msg) = msg_src.poll_message().await {
                handle_msg(msg, app_prov.provide()?).await?;
            }
        }
        Connection::OB11Http {
            http_url,
            post_port,
        } => {
            let (mut msg_src, mut app_prov, _) =
                OB11HttpConnect::new(([0, 0, 0, 0], post_port), http_url)
                    .connect()
                    .await?;

            while let Some(msg) = msg_src.poll_message().await {
                handle_msg(msg, app_prov.provide()?).await?;
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
