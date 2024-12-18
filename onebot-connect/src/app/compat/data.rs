use std::{collections::HashMap, fmt::Display, ops::Deref, sync::Arc};

use http::{header::ToStrError, HeaderMap};
use onebot_connect_interface::{app::RecvMessage as AppMsg, upload::*};
use parking_lot::RwLock;
use serde::{de::Error as DeErr, ser::Error as SerErr};
use serde_value::{DeserializerError, SerializerError};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{common::storage::OBFileStorage, Error as AllErr};

use onebot_types::{
    base::RawMessageSeg,
    compat::{
        action::{
            CompatAction, FromOB11Resp, IntoOB11Action, IntoOB11ActionAsync, SUPPORTED_ACTIONS,
        },
        event::{IntoOB12Event, IntoOB12EventAsync},
        message::{
            CompatSegment, FileSeg, IntoOB11Seg, IntoOB11SegAsync, IntoOB12Seg, IntoOB12SegAsync,
        },
    },
    ob11::{
        action::{self as ob11a, RawAction},
        event::{
            EventKind, MessageEvent, MetaDetail, MetaEvent, NoticeDetail, NoticeEvent,
            RequestDetail, RequestEvent,
        },
        message as ob11m, MessageSeg, RawEvent,
    },
    ob12::{self, action as ob12a, event as ob12e, message as ob12m},
};

use onebot_connect_interface::{
    app::{OBApp, RespArgs},
    Error as OCErr,
};

mod convert {
    use super::*;

    pub fn ob11_to_12_seg<P, M>(msg: M, param: P) -> Result<ob12m::MessageSeg, SerializerError>
    where
        M: IntoOB12Seg<P>,
        <M::Output as TryInto<ob12m::MessageSeg>>::Error: Display,
    {
        msg.into_ob12(param)?
            .try_into()
            .map_err(SerializerError::custom)
    }

    pub async fn ob11_to_12_seg_async<P: Send, M>(
        msg: M,
        param: P,
    ) -> Result<ob12m::MessageSeg, SerializerError>
    where
        M: IntoOB12SegAsync<P>,
        <M::Output as TryInto<ob12m::MessageSeg>>::Error: Display,
    {
        msg.into_ob12(param)
            .await?
            .try_into()
            .map_err(SerializerError::custom)
    }

    pub fn ob11_to_12_seg_default<M>(msg: M) -> Result<ob12m::MessageSeg, SerializerError>
    where
        M: IntoOB12Seg<()>,
        <M::Output as TryInto<ob12m::MessageSeg>>::Error: Display,
    {
        msg.into_ob12(())?
            .try_into()
            .map_err(SerializerError::custom)
    }

    pub async fn ob12_to_11_seg_async<M, P: Send>(
        msg: M,
        param: P,
    ) -> Result<ob11m::MessageSeg, DeserializerError>
    where
        M: IntoOB11SegAsync<P>,
        <M::Output as TryInto<ob11m::MessageSeg>>::Error: Display,
    {
        msg.into_ob11(param)
            .await?
            .try_into()
            .map_err(DeErr::custom)
    }

    pub fn ob12_to_11_seg<M>(msg: M) -> Result<ob11m::MessageSeg, DeserializerError>
    where
        M: IntoOB11Seg,
        <M::Output as TryInto<ob11m::MessageSeg>>::Error: Display,
    {
        msg.into_ob11()?.try_into().map_err(DeErr::custom)
    }

    pub fn convert_action_default<A>(action: A) -> Result<ob11a::ActionType, DeserializerError>
    where
        A: IntoOB11Action<()>,
        <A::Output as TryInto<ob11a::ActionType>>::Error: Display,
    {
        action
            .into_ob11(())?
            .try_into()
            .map_err(DeserializerError::custom)
    }

    pub async fn convert_action_async<A, P: Send>(
        action: A,
        param: P,
    ) -> Result<ob11a::ActionType, DeserializerError>
    where
        A: IntoOB11ActionAsync<P>,
        <A::Output as TryInto<ob11a::ActionType>>::Error: Display,
    {
        action
            .into_ob11(param)
            .await?
            .try_into()
            .map_err(DeserializerError::custom)
    }
}

#[derive(Debug, Clone)]
pub enum ActionConverted {
    Send(RawAction),
    Respond(RespArgs),
}

#[derive(Default)]
struct AppDataInner {
    bot_state: RwLock<ob12::BotState>,
    version_info: RwLock<ob12::VersionInfo>,
    storage: OBFileStorage,
}

/// Connection status of OneBot 11 connection
#[derive(Default, Clone)]
pub(crate) struct AppData(Arc<AppDataInner>);

impl Deref for AppData {
    type Target = AppDataInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[inline]
fn mk_response<R: serde::Serialize>(resp: R) -> Result<ActionConverted, AllErr> {
    Ok(ActionConverted::Respond(RespArgs::success(resp)?))
}
#[inline]
fn mk_missing_file() -> Result<ActionConverted, AllErr> {
    Ok(ActionConverted::Respond(RespArgs::failed(
        ob12a::RetCode::FilesystemError(33404),
        "cannot find specified file",
    )))
}

fn send_check<T: Send>(_: T) {}

fn headers_to_hashmap(headers: HeaderMap) -> Result<HashMap<String, String>, ToStrError> {
    let mut curr_header = "".to_string();
    let mut map: HashMap<String, String> = HashMap::new();
    for (name, value) in headers.into_iter() {
        if let Some(name) = name {
            curr_header = name.to_string();
        }

        match map.get_mut(&curr_header) {
            Some(v) => {
                v.push_str(value.to_str()?);
            }
            None => {
                map.insert(curr_header.clone(), value.to_str()?.to_owned());
            }
        }
    }

    Ok(map)
}

impl AppData {
    async fn trace_file(&self, file: FileSeg) -> Result<String, UploadError> {
        match file.url {
            Some(url) => {
                self.storage
                    .upload(file.file, UploadKind::Url(UrlUpload { url, headers: None }))
                    .await
            }
            None => Err(UploadError::unsupported("send file")),
        }
    }

    async fn find_file(&self, id: &str) -> Result<String, UploadError> {
        match self.storage.get_store_state(&id).await? {
            StoreState::NotCached(url) => Ok(url.url),
            StoreState::Cached(path) => Ok(path),
        }
    }

    async fn ob12_to_ob11_seg<A: OBApp>(&self, msg: RawMessageSeg) -> Result<RawMessageSeg, OCErr> {
        use convert::{ob12_to_11_seg, ob12_to_11_seg_async};
        let msg: ob12m::MessageSeg = msg.try_into()?;
        let find = |name: String| async move { self.find_file(&name).await.map_err(DeErr::custom) };

        let ob12seg = match msg {
            ob12m::MessageSeg::Text(text) => ob12_to_11_seg(text)?,
            ob12m::MessageSeg::Mention(mention) => ob12_to_11_seg(mention)?,
            ob12m::MessageSeg::MentionAll(mention_all) => ob12_to_11_seg(mention_all)?,
            ob12m::MessageSeg::Location(location) => ob12_to_11_seg(location)?,
            ob12m::MessageSeg::Reply(reply) => ob12_to_11_seg(reply)?,
            ob12m::MessageSeg::Image(image) => ob12_to_11_seg_async(image, find).await?,
            ob12m::MessageSeg::Voice(voice) => ob12_to_11_seg_async(voice, find).await?,
            ob12m::MessageSeg::Audio(audio) => ob12_to_11_seg_async(audio, find).await?,
            ob12m::MessageSeg::Video(video) => ob12_to_11_seg_async(video, find).await?,
            ob12m::MessageSeg::File(_) => Err(OCErr::not_supported(
                "`file` message seg conversion not supported",
            ))?,
            ob12m::MessageSeg::Other(RawMessageSeg { r#type, data }) => {
                CompatSegment::parse_data(r#type, data)
                    .map_err(OCErr::other)?
                    .into()
            }
        };

        ob12seg.try_into().map_err(OCErr::serialize)
    }

    fn ob11_to_ob12_seg<'a, A: OBApp + Send>(
        &'a self,
        msg: RawMessageSeg,
        app: &'a A,
    ) -> impl std::future::Future<Output = Result<RawMessageSeg, OCErr>> + Send + 'a {
        async move {
            use convert::*;
            let msg: ob11m::MessageSeg = msg.try_into()?;

            let trace_fn = |file: FileSeg| async move {
                self.trace_file(file)
                    .await
                    .map(|r| r.to_string())
                    .map_err(SerializerError::custom)
            };

            let ob11msg = match msg {
                MessageSeg::Text(text) => ob11_to_12_seg_default(text)?,
                MessageSeg::Face(face) => ob11_to_12_seg_default(face)?,
                MessageSeg::Image(image) => ob11_to_12_seg_async(image, trace_fn).await?,
                MessageSeg::Record(record) => ob11_to_12_seg_async(record, trace_fn).await?,
                MessageSeg::Video(video) => ob11_to_12_seg_async(video, trace_fn).await?,
                MessageSeg::At(at) => ob11_to_12_seg_default(at)?,
                MessageSeg::Rps(rps) => ob11_to_12_seg_default(rps)?,
                MessageSeg::Dice(dice) => ob11_to_12_seg_default(dice)?,
                MessageSeg::Shake(shake) => ob11_to_12_seg_default(shake)?,
                MessageSeg::Poke(poke) => ob11_to_12_seg_default(poke)?,
                MessageSeg::Anonymous(anonymous) => ob11_to_12_seg_default(anonymous)?,
                MessageSeg::Share(share) => ob11_to_12_seg_default(share)?,
                MessageSeg::Contact(contact) => ob11_to_12_seg_default(contact)?,
                MessageSeg::Location(location) => ob11_to_12_seg_default(location)?,
                MessageSeg::Music(music) => ob11_to_12_seg_default(music)?,
                MessageSeg::Reply(reply) => {
                    use onebot_connect_interface::app::AppExt;

                    let resp = app
                        .call_action(
                            ob11a::GetMsg {
                                message_id: reply.id,
                            },
                            Some(self.bot_state.read().self_.clone()),
                        )
                        .await?;

                    reply
                        .into_ob12(resp.sender.user_id().map(|r| r.to_string()))?
                        .into()
                }
                MessageSeg::Forward(forward) => ob11_to_12_seg_default(forward)?,
                MessageSeg::Node(forward_node) => ob11_to_12_seg_default(forward_node)?,
                MessageSeg::Xml(xml) => ob11_to_12_seg_default(xml)?,
                MessageSeg::Json(json) => ob11_to_12_seg_default(json)?,
            };

            ob11msg.try_into().map_err(OCErr::serialize)
        }
    }

    async fn convert_msg_event<A: OBApp + Send>(
        &self,
        msg: MessageEvent,
        app: &A,
    ) -> Result<ob12e::MessageEvent, OCErr> {
        let msg_convert = |msg| async move {
            self.ob11_to_ob12_seg(msg, app)
                .await
                .map_err(SerErr::custom)
        };
        let id = self.bot_state.read().self_.user_id.clone();
        Ok(msg
            .into_ob12((id, msg_convert))
            .await
            .map_err(OCErr::serialize)?)
    }

    async fn convert_meta_event<A: OBApp>(
        &self,
        time: f64,
        meta: MetaDetail,
        cmd_tx: mpsc::UnboundedSender<AppMsg>,
        _app: &A,
    ) -> Result<ob12e::MetaEvent, OCErr> {
        let meta: MetaEvent = meta.try_into()?;

        let (event, update) = meta.into_ob12(&self.version_info.read())?;

        if let Some(status) = update {
            let good = status.good;
            let state =
                ob12::BotState::from_ob11(status, self.bot_state.read().self_.user_id.clone())?;

            if state != *self.bot_state.read() {
                // if latest bot state is different
                let update = ob12e::meta::StatusUpdate {
                    status: ob12::Status {
                        good,
                        bots: vec![state],
                        extra: Default::default(),
                    },
                    extra: Default::default(),
                };
                cmd_tx.send(AppMsg::Event(ob12e::RawEvent {
                    id: Uuid::new_v4().to_string(),
                    time,
                    event: ob12e::MetaEvent::StatusUpdate(update).try_into()?,
                }))?;
            }
        }

        Ok(event)
    }

    async fn convert_req_event<A: OBApp>(
        &self,
        req: RequestDetail,
        _app: &A,
    ) -> Result<ob12e::RequestEvent, OCErr> {
        let req: RequestEvent = req.try_into()?;
        Ok(req.into_ob12(self.bot_state.read().self_.user_id.clone())?)
    }

    fn convert_notice_event<'a, A: OBApp + Send>(
        &'a self,
        notice: NoticeDetail,
        _app: &'a A,
    ) -> impl std::future::Future<Output = Result<ob12e::Event, OCErr>> + Send + 'a {
        async move {
            let notice: NoticeEvent = notice.try_into()?;

            Ok(notice
                .into_ob12((
                    self.bot_state.read().self_.user_id.clone(),
                    |_| async move { Uuid::new_v4().to_string() },
                ))
                .await?)
        }
    }

    pub async fn convert_event<A: OBApp>(
        &self,
        raw_event: RawEvent,
        cmd_tx: mpsc::UnboundedSender<AppMsg>,
        app: &A,
    ) -> Result<ob12e::RawEvent, OCErr> {
        let event_k: EventKind = raw_event.detail.try_into()?;
        let time = raw_event.time as f64;

        let event: ob12e::EventDetail = match event_k {
            EventKind::Message(msg) => self
                .convert_msg_event(msg.try_into()?, app)
                .await?
                .try_into()?,
            EventKind::Meta(meta) => self
                .convert_meta_event(time, meta, cmd_tx, app)
                .await?
                .try_into()?,
            EventKind::Request(req) => self.convert_req_event(req, app).await?.try_into()?,
            EventKind::Notice(notice) => {
                self.convert_notice_event(notice, app).await?.try_into()?
            }
        };
        Ok(ob12e::RawEvent {
            id: Uuid::new_v4().to_string(),
            time,
            event,
        })
    }

    async fn handle_get_file(&self, action: ob12a::GetFile) -> Result<ActionConverted, AllErr> {
        use ob12a::*;

        let (name, kind) = match action.r#type {
            GetFileType::Url => match self.storage.get_url(&action.file_id).await? {
                Some(FileInfo {
                    name,
                    inner: UrlUpload { url, headers },
                }) => (
                    name,
                    ob12a::UploadKind::Url {
                        headers,
                        url,
                        extra: Default::default(),
                    },
                ),
                None => return mk_missing_file(),
            },
            GetFileType::Path => match self.storage.get_path(&action.file_id).await? {
                Some(FileInfo { name, inner }) => (
                    name,
                    ob12a::UploadKind::Path {
                        path: inner.to_string_lossy().into_owned(),
                        extra: Default::default(),
                    },
                ),
                None => return mk_missing_file(),
            },
            GetFileType::Data => match self.storage.get_data(&action.file_id).await? {
                Some(FileInfo { name, inner }) => (
                    name,
                    ob12a::UploadKind::Data {
                        data: UploadData(inner),
                        extra: Default::default(),
                    },
                ),
                None => return mk_missing_file(),
            },
            GetFileType::Other(typ) => {
                return Err(AllErr::other(format!("unknown `get_file` type: {}", typ)))
            }
        };

        let resp = GetFileResp {
            file: FileOpt {
                kind,
                name,
                sha256: None,
            },
            extra: Default::default(),
        };
        Ok(ActionConverted::Respond(RespArgs::success(resp)?))
    }

    async fn handle_upload(&self, action: ob12a::UploadFile) -> Result<ActionConverted, AllErr> {
        let ob12a::UploadFile(ob12a::FileOpt {
            kind,
            name,
            sha256: _,
        }) = action;
        let file_id = self
            .storage
            .upload(
                name,
                match kind {
                    ob12a::UploadKind::Url { headers, url, .. } => {
                        UploadKind::Url(UrlUpload { url, headers })
                    }
                    ob12a::UploadKind::Path { path, .. } => UploadKind::Path(path.into()),
                    ob12a::UploadKind::Data { data, .. } => UploadKind::Data(data.0.into()),
                    ob12a::UploadKind::Other { r#type, .. } => {
                        return Err(
                            UploadError::unsupported(format!("upload type `{}`", r#type)).into(),
                        )
                    }
                },
            )
            .await?;
        Ok(ActionConverted::Respond(RespArgs::success(
            ob12a::Uploaded {
                file_id,
                extra: Default::default(),
            },
        )?))
    }

    pub async fn convert_action<A: OBApp>(
        &self,
        action: ob12a::RawAction,
        app: &A,
    ) -> Result<ActionConverted, AllErr> {
        use convert::*;

        let ob12a::RawAction {
            action,
            echo,
            self_: _,
        } = action;
        let detail: ob12a::ActionType = action.try_into().map_err(OCErr::deserialize)?;
        let detail = match detail {
            ob12a::ActionType::SendMessage(action) => {
                convert_action_async(action, |msg| async move {
                    self.ob11_to_ob12_seg(msg, app).await
                })
                .await?
            }
            ob12a::ActionType::DeleteMessage(action) => convert_action_default(action)?,
            ob12a::ActionType::SetGroupName(action) => convert_action_default(action)?,
            ob12a::ActionType::GetLatestEvents(_) => {
                // please get events using ob11 http post
                return Err(OCErr::not_supported("please poll events with HTTP POST").into());
            }
            ob12a::ActionType::GetSupportedActions(_) => {
                return Ok(ActionConverted::Respond(RespArgs::success(
                    SUPPORTED_ACTIONS,
                )?))
            }
            ob12a::ActionType::GetStatus(action) => convert_action_default(action)?,
            ob12a::ActionType::GetVersion(action) => convert_action_default(action)?,
            ob12a::ActionType::GetSelfInfo(action) => convert_action_default(action)?,
            ob12a::ActionType::GetUserInfo(action) => convert_action_default(action)?,
            ob12a::ActionType::GetFriendList(action) => convert_action_default(action)?,
            ob12a::ActionType::GetGroupInfo(action) => convert_action_default(action)?,
            ob12a::ActionType::GetGroupList(action) => convert_action_default(action)?,
            ob12a::ActionType::GetGroupMemberInfo(action) => convert_action_default(action)?,
            ob12a::ActionType::GetGroupMemberList(action) => convert_action_default(action)?,
            ob12a::ActionType::LeaveGroup(action) => convert_action_default(action)?,
            ob12a::ActionType::GetFile(action) => return self.handle_get_file(action).await,
            ob12a::ActionType::GetFileFragmented(action) => {
                let resp = self.storage.get_fragmented(action).await?;
                return match resp {
                    Some(resp) => Ok(ActionConverted::Respond(RespArgs::success(resp)?)),
                    None => mk_missing_file(),
                };
            }
            ob12a::ActionType::Other(ob12a::ActionDetail { action, params }) => {
                let detail = CompatAction::from_data(action, params)
                    .map_err(OCErr::other)?
                    .try_into()?;
                return Ok(ActionConverted::Send(RawAction { detail, echo }));
            }
            ob12a::ActionType::UploadFile(action) => return self.handle_upload(action).await,
            ob12a::ActionType::UploadFileFragmented(ob12a::UploadFileFragmented(req)) => {
                use ob12a::*;
                return Ok(ActionConverted::Respond(RespArgs::success(
                    UploadFragmented {
                        file_id: self.storage.upload_fragmented(req).await?,
                        extra: Default::default(),
                    },
                )?));
            }
            action => {
                return Err(
                    OCErr::not_supported(format!("unsupported action: {:?}", action)).into(),
                )
            }
        };

        Ok(ActionConverted::Send(RawAction {
            detail: detail.try_into()?,
            echo,
        }))
    }
}
