use onebot_types::ob11::RawEvent;
use onebot_connect_interface::{app::OBApp, Error as OCErr};

pub mod data;
pub mod http;
pub mod ws;
pub mod ws_re;

enum OB11Recv {
    Event(RawEvent),
}
