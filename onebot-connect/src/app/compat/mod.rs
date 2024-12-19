use onebot_connect_interface::{app::OBApp, Error as OCErr};
use onebot_types::ob11::RawEvent;

pub mod data;
#[cfg(all(feature = "hyper", feature = "http"))]
pub mod http;
#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub mod ws_re;
