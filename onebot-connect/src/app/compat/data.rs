use std::{
    fmt::Display,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use onebot_connect_interface::{app::RecvMessage as AppMsg, upload::*};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_value::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    common::{storage::OBFileStorage, ConnState},
    ob11, Error as AllErr,
};

use onebot_types::{
    base::RawMessageSeg,
    compat::{
        action::{
            CompatAction, FromOB11Resp, IntoOB11Action, IntoOB11ActionAsync, UserInfoResp,
            SUPPORTED_ACTIONS,
        },
        compat_self,
        event::{IntoOB12Event, IntoOB12EventAsync},
        message::{
            CompatSegment, FileSeg, IntoOB11Seg, IntoOB11SegAsync, IntoOB12Seg, IntoOB12SegAsync,
        },
        CompatError,
    },
    ob11::{
        action::{self as ob11a},
        event::{
            EventKind, MessageEvent, MetaDetail, MetaEvent, NoticeDetail, NoticeEvent,
            RequestDetail, RequestEvent,
        },
        message as ob11m, MessageSeg, RawEvent,
    },
    ob12::{self, action as ob12a, event as ob12e, message as ob12m},
    OBAction,
};

use onebot_connect_interface::{app::OBApp, Error as OCErr, RespArgs};

mod convert {
    use onebot_types::compat::{CompatError, CompatResult};

    use super::*;

    pub async fn ob11_to_12_seg_async<P: Send, M>(
        msg: M,
        param: P,
    ) -> CompatResult<ob12m::MessageSeg>
    where
        M: IntoOB12SegAsync<P>,
        <M::Output as TryInto<ob12m::MessageSeg>>::Error: Display,
    {
        msg.into_ob12(param)
            .await?
            .try_into()
            .map_err(CompatError::other)
    }

    pub fn ob11_to_12_seg_default<M>(msg: M) -> Result<ob12m::MessageSeg, CompatError>
    where
        M: IntoOB12Seg<()>,
        <M::Output as TryInto<ob12m::MessageSeg>>::Error: Display,
    {
        msg.into_ob12(())?.try_into().map_err(CompatError::other)
    }

    pub async fn ob12_to_11_seg_async<M, P: Send>(
        msg: M,
        param: P,
    ) -> Result<ob11m::MessageSeg, CompatError>
    where
        M: IntoOB11SegAsync<P>,
        <M::Output as TryInto<ob11m::MessageSeg>>::Error: Display,
    {
        msg.into_ob11(param)
            .await?
            .try_into()
            .map_err(CompatError::other)
    }

    pub fn ob12_to_11_seg<M>(msg: M) -> CompatResult<ob11m::MessageSeg>
    where
        M: IntoOB11Seg,
        <M::Output as TryInto<ob11m::MessageSeg>>::Error: Display,
    {
        msg.into_ob11()?.try_into().map_err(CompatError::other)
    }

    pub fn convert_action_default<A>(action: A) -> Result<ob11a::ActionType, CompatError>
    where
        A: IntoOB11Action<()>,
        <A::Output as TryInto<ob11a::ActionType>>::Error: Display,
    {
        action.into_ob11(())?.try_into().map_err(CompatError::other)
    }

    pub async fn convert_action_async<A, P: Send>(
        action: A,
        param: P,
    ) -> Result<ob11a::ActionType, CompatError>
    where
        A: IntoOB11ActionAsync<P>,
        <A::Output as TryInto<ob11a::ActionType>>::Error: Display,
    {
        action
            .into_ob11(param)
            .await?
            .try_into()
            .map_err(CompatError::other)
    }
}

#[derive(Debug, Clone)]
pub enum ActionConverted {
    Send(ob11a::ActionDetail),
    Respond(RespArgs),
}

#[derive(Debug)]
struct BotStatus {
    online: bool,
    good: bool,
}

impl Default for BotStatus {
    fn default() -> Self {
        Self {
            online: true,
            good: true,
        }
    }
}

#[derive(Default)]
struct AppDataInner {
    #[allow(unused)]
    conn_state: ConnState,
    self_id: AtomicI64,
    version_info: RwLock<ob12::VersionInfo>,
    status: RwLock<BotStatus>,
    storage: OBFileStorage,
}

/// Connection status of OneBot 11 connection
#[derive(Default, Clone)]
pub struct AppData(Arc<AppDataInner>);

#[inline]
fn mk_missing_file() -> Result<ActionConverted, AllErr> {
    Ok(ActionConverted::Respond(RespArgs::failed(
        ob12a::RetCode::FilesystemError(33404),
        "cannot find specified file",
    )))
}

impl AppData {
    #[inline]
    pub fn update_self_id(&self, id: i64) {
        self.0.self_id.store(id, Ordering::Release)
    }

    #[inline]
    pub fn get_self_id(&self) -> i64 {
        self.0.self_id.load(Ordering::Acquire)
    }

    async fn trace_file(&self, file: FileSeg) -> Result<String, UploadError> {
        match file.url {
            Some(url) => {
                self.0
                    .storage
                    .upload(file.file, UploadKind::Url(UrlUpload { url, headers: None }))
                    .await
            }
            None => Err(UploadError::unsupported("send file")),
        }
    }

    async fn find_file(&self, id: &str) -> Result<String, UploadError> {
        match self.0.storage.get_store_state(&id).await? {
            StoreState::NotCached(url) => Ok(url.url),
            StoreState::Cached(path) => Ok(path),
        }
    }

    async fn ob12_to_ob11_seg(&self, msg: RawMessageSeg) -> Result<RawMessageSeg, OCErr> {
        use convert::{ob12_to_11_seg, ob12_to_11_seg_async};
        let msg: ob12m::MessageSeg = msg.try_into()?;
        let find =
            |name: String| async move { self.find_file(&name).await.map_err(CompatError::other) };

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

        ob12seg.try_into().map_err(OCErr::other)
    }

    async fn ob11_to_ob12_seg<A: OBApp + 'static>(
        &self,
        msg: RawMessageSeg,
        app: &A,
    ) -> Result<RawMessageSeg, OCErr> {
        use convert::*;
        let msg: ob11m::MessageSeg = msg.try_into()?;

        let trace_fn = |file: FileSeg| async move {
            self.trace_file(file)
                .await
                .map(|r| r.to_string())
                .map_err(CompatError::other)
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
                        Some(compat_self(self.get_self_id().to_string())),
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

    async fn convert_msg_event<'a, A: OBApp + Sync + 'static>(
        &'a self,
        msg: MessageEvent,
        app: &'a A,
    ) -> Result<ob12e::MessageEvent, OCErr> {
        let msg_convert = |msg| async move {
            self.ob11_to_ob12_seg(msg, app).await.map_err(|e| {
                CompatError::other(format!("error while converting msg from ob11 to 12: {e}"))
            })
        };
        let id = self.get_self_id().to_string();
        Ok(msg.into_ob12((id, msg_convert)).await?)
    }

    async fn convert_meta_event<A: OBApp>(
        &self,
        time: f64,
        meta: MetaDetail,
        cmd_tx: &mpsc::UnboundedSender<AppMsg>,
        _app: &A,
    ) -> Result<ob12e::MetaEvent, OCErr> {
        let inner = &self.0;
        let meta: MetaEvent = meta.try_into()?;

        let (event, update) = meta.into_ob12(&inner.version_info.read())?;

        if let Some(status) = update {
            let ob11::Status { online, good, .. } = status;
            let curr = self.0.status.read();
            if curr.good != status.good || curr.online != online {
                drop(curr);
                let curr_status = BotStatus { online, good };
                *self.0.status.write() = curr_status;

                let state = ob12::BotState {
                    self_: compat_self(self.get_self_id().to_string()),
                    online,
                    extra: Default::default(),
                };
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
        Ok(req.into_ob12(self.get_self_id().to_string())?)
    }

    async fn convert_notice_event<A: OBApp + 'static>(
        &self,
        notice: NoticeDetail,
        _app: &A,
    ) -> Result<ob12e::Event, OCErr> {
        let notice: NoticeEvent = notice.try_into()?;

        Ok(notice
            .into_ob12((self.get_self_id().to_string(), |_| async move {
                Uuid::new_v4().to_string()
            }))
            .await?)
    }

    pub async fn convert_event<A: OBApp + Sync + 'static>(
        &self,
        raw_event: RawEvent,
        cmd_tx: &mpsc::UnboundedSender<AppMsg>,
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

        let inner = &self.0;
        let (name, kind) = match action.r#type {
            GetFileType::Url => match inner.storage.get_url(&action.file_id).await? {
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
            GetFileType::Path => match inner.storage.get_path(&action.file_id).await? {
                Some(FileInfo { name, inner }) => (
                    name,
                    ob12a::UploadKind::Path {
                        path: inner.to_string_lossy().into_owned(),
                        extra: Default::default(),
                    },
                ),
                None => return mk_missing_file(),
            },
            GetFileType::Data => match inner.storage.get_data(&action.file_id).await? {
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
            .0
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

    pub async fn convert_action<A: OBApp + 'static>(
        &self,
        action: ob12a::ActionDetail,
        _app: &A,
    ) -> Result<ActionConverted, AllErr> {
        use convert::*;

        #[inline]
        fn mk_response<R: serde::Serialize>(resp: R) -> Result<ActionConverted, AllErr> {
            Ok(ActionConverted::Respond(RespArgs::success(resp)?))
        }

        let detail: ob12a::ActionType = action.try_into().map_err(OCErr::deserialize)?;
        let inner = &self.0;
        let detail = match detail {
            ob12a::ActionType::SendMessage(action) => {
                convert_action_async(action, |msg| async move {
                    self.ob12_to_ob11_seg(msg).await.map_err(|e| {
                        OCErr::other(format!("err while converting msg from ob12 to ob11: {e}"))
                    })
                })
                .await?
            }
            ob12a::ActionType::GetStatus(_) => {
                let status = self.0.status.read();
                return mk_response(ob12::Status {
                    good: status.good,
                    bots: vec![ob12::BotState {
                        self_: compat_self(self.get_self_id().to_string()),
                        online: status.online,
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                });
            }
            ob12a::ActionType::DeleteMessage(action) => convert_action_default(action)?,
            ob12a::ActionType::SetGroupName(action) => convert_action_default(action)?,
            ob12a::ActionType::GetLatestEvents(_) => {
                // please get events using ob11 http post
                return Err(OCErr::not_supported("please poll events with HTTP POST").into());
            }
            ob12a::ActionType::GetSupportedActions(_) => return mk_response(SUPPORTED_ACTIONS),
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
                let resp = inner.storage.get_fragmented(action).await?;
                return match resp {
                    Some(resp) => mk_response(resp),
                    None => mk_missing_file(),
                };
            }
            ob12a::ActionType::Other(ob12a::ActionDetail { action, params }) => {
                let detail = CompatAction::from_data(action, params)
                    .map_err(OCErr::other)?
                    .try_into()?;
                return Ok(ActionConverted::Send(detail));
            }
            ob12a::ActionType::UploadFile(action) => return self.handle_upload(action).await,
            ob12a::ActionType::UploadFileFragmented(ob12a::UploadFileFragmented(req)) => {
                use ob12a::*;
                return mk_response(UploadFragmented {
                    file_id: inner.storage.upload_fragmented(req).await?,
                    extra: Default::default(),
                });
            }
            action => {
                return Err(
                    OCErr::not_supported(format!("unsupported action: {:?}", action)).into(),
                )
            }
        };

        Ok(ActionConverted::Send(detail.try_into()?))
    }

    pub async fn convert_resp_data(&self, action_name: &str, data: Value) -> Result<Value, OCErr> {
        use ob11a::*;
        fn convert_user_resp(resp: impl Into<UserInfoResp>) -> Result<Value, OCErr> {
            Ok(serde_value::to_value(ob12::UserInfo::from_ob11(
                resp.into(),
                (),
            )?)?)
        }

        fn to_value(data: impl Serialize) -> Result<Value, OCErr> {
            Ok(serde_value::to_value(data)?)
        }

        match Some(action_name) {
            SendGroupMsg::ACTION | SendPrivateMsg::ACTION | SendMsg::ACTION => to_value(
                ob12a::SendMessageResp::from_ob11(MessageResp::deserialize(data)?, {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs_f64()
                })?,
            ),
            GetLoginInfo::ACTION => convert_user_resp(LoginInfo::deserialize(data)?),
            GetStrangerInfo::ACTION => convert_user_resp(StrangerInfo::deserialize(data)?),
            GetFriendList::ACTION => {
                let list: Vec<FriendInfo> = Deserialize::deserialize(data)?;

                to_value(
                    list.into_iter()
                        .map(convert_user_resp)
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            GetGroupInfo::ACTION => to_value(ob12::GroupInfo::from_ob11(
                GroupInfo::deserialize(data)?,
                (),
            )?),
            GetGroupList::ACTION => {
                let list: Vec<GroupInfo> = Deserialize::deserialize(data)?;
                to_value(
                    list.into_iter()
                        .map(|r| ob12::GroupInfo::from_ob11(r, ()))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            GetGroupMemberInfo::ACTION => convert_user_resp(GroupMemberInfo::deserialize(data)?),
            GetGroupMemberList::ACTION => {
                let list: Vec<GroupMemberInfo> = Deserialize::deserialize(data)?;

                to_value(
                    list.into_iter()
                        .map(convert_user_resp)
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            _ => to_value(data),
        }
    }
}
