use onebot_types::ob11::Event;

pub mod http;
pub mod ws;
pub mod ws_re;

pub enum OB11Recv {
    Event(Event)
}
