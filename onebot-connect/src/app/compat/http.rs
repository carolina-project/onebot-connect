use super::*;
use crate::common::{http_s::HttpResponse, CloseHandler, CmdHandler, RecvHandler};
use onebot_connect_interface::ClosedReason;
use onebot_types::ob11::Event as OB11Event;
use tokio::sync::oneshot;

// OneBot 11 HTTP POST side

pub enum HttpPostRecv {
    Event(OB11Event),
    Close(Result<ClosedReason, String>),
}

type HttpPostResponder = oneshot::Sender<HttpResponse<()>>;

pub enum HttpPostCommand {
    Close
}


pub struct HttpPostHandler;

impl RecvHandler<(Event, HttpPostResponder), HttpPostRecv> for HttpPostHandler {}

impl CmdHandler<HttpPostCommand, HttpPostRecv> for HttpPostHandler {}

impl CloseHandler<HttpPostRecv> for HttpPostHandler {}
