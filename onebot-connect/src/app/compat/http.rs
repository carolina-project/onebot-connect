use super::*;
use crate::{
    app::{HttpInnerShared, RxMessageSource},
    common::{http_s::HttpResponse, *},
};
use data::AppData;
use http_s::HttpServerTask;
use hyper::StatusCode;
use onebot_connect_interface::{
    app::{Connect, OBAppProvider, RecvMessage, RespArgs},
    ClosedReason,
};
use onebot_types::{
    ob11::{action as ob11a, RawEvent as OB11Event},
    ob12::{
        self,
        action::{self as ob12a, RespError, RetCode},
    },
};
use tokio::sync::{mpsc, oneshot};

pub struct HttpConnect;

impl Connect for HttpConnect {
    type Source = RxMessageSource;
    type Error = crate::Error;
    type Message = ();
    type Provider = HttpAppProvider;

    async fn connect(
        self,
    ) -> Result<(Self::Source, Self::Provider, Self::Message), Self::Error> {
        HttpServerTask::create()
    }

    fn with_authorization(self, access_token: impl Into<String>) -> Self {
        todo!()
    }
    
}

#[derive(Clone)]
pub struct OB11HttpApp {
    inner: HttpInnerShared,
    data: AppData,
}

impl OBApp for OB11HttpApp {
    async fn send_action_impl(
        &self,
        action: ob12a::ActionDetail,
        _self_: Option<ob12::BotSelf>,
    ) -> Result<Option<serde_value::Value>, OCErr> {
        match self.data.convert_action(action, self).await? {
            data::ActionConverted::Send(ob11a::ActionDetail { action, params }) => {
                let inner = &self.inner;
                let response = self
                    .inner
                    .http
                    .post(format!("{}/{}", inner.url, action))
                    .json(&params)
                    .send()
                    .await
                    .map_err(OCErr::other)?;

                let data: ob11a::RespData = match response.status() {
                    StatusCode::OK => response
                        .json()
                        .await
                        .map_err(|e| OCErr::other(format!("json parse error: {:?}", e)))?,
                    StatusCode::NOT_FOUND => {
                        return Err(RespError::new(
                            RetCode::UnsupportedAction,
                            format!("unsupported action: `{action}`"),
                        )
                        .into())
                    }
                    StatusCode::BAD_REQUEST => {
                        return Err(RespError::new(RetCode::BadParam, "post body error").into())
                    }
                    code => {
                        return Err(RespError::new(
                            RetCode::OtherError(code.as_u16() as u32),
                            "http error",
                        )
                        .into())
                    }
                };

                if !data.is_success() {
                    return Err(RespError {
                        retcode: RetCode::OtherError(data.retcode as u32),
                        message: "OneBot 11 error".into(),
                        echo: data.echo,
                    }
                    .into());
                }

                Ok(Some(self.data.convert_resp_data(&action, data.data).await?))
            }
            data::ActionConverted::Respond(RespArgs {
                status,
                retcode,
                data,
                message,
            }) => {
                let resp = ob12a::RespData {
                    status,
                    retcode,
                    data,
                    message,
                    echo: None,
                };

                if !resp.is_success() {
                    Err(RespError::from(resp).into())
                } else {
                    Ok(Some(resp.data))
                }
            }
        }
    }

    fn clone_app(&self) -> Self
    where
        Self: 'static,
    {
        self.clone()
    }
}

pub struct HttpAppProvider {
    app: OB11HttpApp
}

impl OBAppProvider for HttpAppProvider {
    type Output = OB11HttpApp;

    fn provide(&mut self) -> Result<Self::Output, OCErr> {
        Ok(self.app.clone())
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
    app: OB11HttpApp,
}

impl RecvHandler<(RawEvent, HttpPostResponder), RecvMessage> for HttpPostHandler {
    async fn handle_recv(
        &mut self,
        recv: (RawEvent, HttpPostResponder),
        msg_tx: mpsc::UnboundedSender<RecvMessage>,
        _state: ConnState,
    ) -> Result<(), crate::Error> {
        let (event, respond) = recv;
        msg_tx.send(RecvMessage::Event(
            self.data
                .convert_event(event, msg_tx.clone(), &self.app)
                .await?,
        ))?;
        // ignore quick action
        respond
            .send(HttpResponse::Other {
                status: StatusCode::NO_CONTENT,
            })
            .map_err(|_| crate::Error::ChannelClosed)?;
        Ok(())
    }
}

impl CmdHandler<HttpPostCommand, HttpPostRecv> for HttpPostHandler {
    async fn handle_cmd(
        &mut self,
        cmd: HttpPostCommand,
        state: ConnState,
    ) -> Result<(), crate::Error> {
        match cmd {
            HttpPostCommand::Close => state.set_active(false),
        }

        Ok(())
    }
}

impl CloseHandler<HttpPostRecv> for HttpPostHandler {
    async fn handle_close(
        &mut self,
        result: Result<ClosedReason, String>,
        msg_tx: mpsc::UnboundedSender<HttpPostRecv>,
    ) -> Result<(), crate::Error> {
        msg_tx.send(HttpPostRecv::Close(result))?;
        Ok(())
    }
}
