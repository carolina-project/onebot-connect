use super::*;
use crate::{
    app::HttpInnerShared,
    common::{http_s::HttpResponse, *},
    Error as AllErr,
};
use data::AppData;
use onebot_connect_interface::ClosedReason;
use onebot_types::{
    compat::{event::IntoOB12EventAsync, message::IntoOB12Seg},
    ob11::{
        event::{EventKind, MessageEvent},
        MessageSeg, RawEvent as OB11Event,
    },
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

pub struct OB12HttpApp {
    http: reqwest::Client,
    inner: HttpInnerShared,
}

impl OBApp for OB12HttpApp {
    async fn send_action_impl(
        &self,
        action: ob12::action::ActionType,
        self_: Option<ob12::BotSelf>,
    ) -> Result<Option<serde_value::Value>, OCErr> {
    }

    fn clone_app(&self) -> Self {
        todo!()
    }
}
