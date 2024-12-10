
use super::*;
use crate::{
    common::{http_s::HttpResponse, *},
    Error as AllErr,
};
use onebot_connect_interface::ClosedReason;
use onebot_connect_interface::Error as OCErr;
use onebot_types::{
    compat::{event::IntoOB12EventAsync, message::IntoOB12Seg},
    ob11::{event::{EventKind, MessageEvent}, Event as OB11Event, MessageSeg},
    ob12::{event as ob12e, message as ob12m},
};
use parking_lot::RwLock;
use tokio::sync::{mpsc, oneshot};


// OneBot 11 HTTP POST side
pub enum HttpPostRecv {
    Event(OB11Event),
    Close(Result<ClosedReason, String>),
}

type HttpPostResponder = oneshot::Sender<HttpResponse<()>>;

pub enum HttpPostCommand {
    Close,
}

pub struct HttpPostHandler {
    data: AppData,
}

impl RecvHandler<(Event, HttpPostResponder), HttpPostRecv> for HttpPostHandler {
    async fn handle_recv(
        &mut self,
        recv: (Event, HttpPostResponder),
        msg_tx: mpsc::UnboundedSender<HttpPostRecv>,
        state: ConnState,
    ) -> Result<(), crate::Error> {
        let (event, respond) = recv;
        match event.kind {
            EventKind::Message(_) => todo!(),
            EventKind::Meta(_) => todo!(),
            EventKind::Request(_) => todo!(),
            EventKind::Notice(_) => todo!(),
        }
    }
}

impl CmdHandler<HttpPostCommand, HttpPostRecv> for HttpPostHandler {}

impl CloseHandler<HttpPostRecv> for HttpPostHandler {}

