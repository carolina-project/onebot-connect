use super::*;
use crate::{
    app::HttpInnerShared,
    common::{http_s::HttpResponse, *},
    Error as AllErr,
};
use data::AppData;
use hyper::StatusCode;
use onebot_connect_interface::{app::RecvMessage, ClosedReason};
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

pub struct OB12HttpApp {
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
    app: OB12HttpApp,
}

impl RecvHandler<(RawEvent, HttpPostResponder), RecvMessage> for HttpPostHandler {
    async fn handle_recv(
        &mut self,
        recv: (RawEvent, HttpPostResponder),
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        _state: ConnState,
    ) -> Result<(), crate::Error> {
        let (event, respond) = recv;
        let converted = self.data.convert_event(event, msg_tx.clone(), &self.app).await?;
        msg_tx.send(RecvMessage::Event(converted));
        respond.send(HttpResponse::Other {
            status: StatusCode::NO_CONTENT,
        });
        Ok(())
    }
}

impl CmdHandler<HttpPostCommand, HttpPostRecv> for HttpPostHandler {}

impl CloseHandler<HttpPostRecv> for HttpPostHandler {}
