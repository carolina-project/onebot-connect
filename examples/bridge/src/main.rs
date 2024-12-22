use std::env;

use eyre::eyre;
use onebot_connect::{
    app::compat::http::OB11HttpConnect, imp::ws::WSCreate, wrap::bridge::OBridge,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> eyre::Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info")
    }
    env_logger::init();

    let bridge = OBridge::create(
        OB11HttpConnect::new(([0, 0, 0, 0], 4001), "http://127.0.0.1:4000"),
        || WSCreate::new("0.0.0.0:6001"),
        32,
    )
    .await
    .map_err(|e| eyre!("{e}"))?;
    log::info!("connection established.");
    let (a_res, i_res) = bridge.join().await;
    a_res?;
    i_res?;

    Ok(())
}
