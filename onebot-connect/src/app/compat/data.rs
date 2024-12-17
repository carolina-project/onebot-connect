use std::{fmt::Display, ops::Deref, str::FromStr, sync::Arc};

use dashmap::DashMap;
use fxhash::FxHashMap;
use parking_lot::RwLock;
use serde::{de::Error as DeErr, ser::Error as SerErr, Deserialize};
use serde_value::{DeserializerError, SerializerError, Value};
use url::Url;
use uuid::Uuid;

use crate::{
    common::{
        self,
        storage::{self, LocalFs, OBFileStorage, FS},
        UploadError, UploadKind, UploadStorage, UrlUpload,
    },
    Error as AllErr,
};

use onebot_types::{
    base::RawMessageSeg,
    compat::{
        action::{bot::OB11File, CompatAction, IntoOB11Action, IntoOB11ActionAsync},
        compat_self,
        event::IntoOB12EventAsync,
        message::{
            CompatSegment, FileSeg, IntoOB11Seg, IntoOB11SegAsync, IntoOB12Seg, IntoOB12SegAsync,
        },
    },
    ob11::{
        action::{self as ob11a, ActionType, RawAction},
        event::{self as ob11e, EventKind, MessageEvent},
        message as ob11m, MessageSeg, RawEvent,
    },
    ob12::{self, action as ob12a, event as ob12e, message as ob12m},
    ValueMap,
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

    pub async fn ob11_to_12_seg_async<P, M>(
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

    pub async fn ob12_to_11_seg_async<M, P>(
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

    pub async fn convert_action_async<A, P>(
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
    bot_self: RwLock<Option<ob12::BotSelf>>,
    storage: OBFileStorage,
}

/// Connection status of OneBot 11 connection
#[derive(Default, Clone)]
struct AppData(Arc<AppDataInner>);

impl Deref for AppData {
    type Target = AppDataInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AppData {
    async fn trace_file(&self, file: FileSeg) -> Result<Uuid, UploadError> {
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
        let uuid = Uuid::from_str(id)?;
        match self.storage.get_store_state(&uuid).await? {
            common::StoreState::NotCached(url) => Ok(url.url),
            common::StoreState::Cached(path) => Ok(path),
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

    async fn ob11_to_ob12_seg<A: OBApp>(
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
                        self.bot_self.read().clone(),
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

    async fn convert_msg_event<A: OBApp>(
        &self,
        msg: MessageEvent,
        app: &A,
    ) -> Result<ob12e::MessageEvent, OCErr> {
        let msg_convert = |msg| async {
            self.ob11_to_ob12_seg(msg, app)
                .await
                .map_err(SerErr::custom)
        };
        Ok(msg
            .into_ob12((
                self.bot_self
                    .read()
                    .as_ref()
                    .map(|r| r.user_id.clone())
                    .ok_or_else(|| OCErr::missing("bot_self not set"))?,
                msg_convert,
            ))
            .await
            .map_err(OCErr::serialize)?)
    }

    async fn convert_event<A: OBApp>(
        &self,
        event: RawEvent,
        app: &A,
    ) -> Result<ob12e::RawEvent, OCErr> {
        let event_k: EventKind = event.detail.try_into()?;

        let detail = match event_k {
            EventKind::Message(msg) => self.convert_msg_event(msg.try_into()?, app).await?,
            EventKind::Meta(_) => todo!(),
            EventKind::Request(_) => todo!(),
            EventKind::Notice(_) => todo!(),
        };
        Ok(ob12e::RawEvent {
            id: Uuid::new_v4().to_string(),
            time: event.time,
            event: detail.try_into()?,
        })
    }

    async fn convert_action<A: OBApp>(
        &self,
        action: ob12a::RawAction,
        app: &A,
    ) -> Result<ActionConverted, AllErr> {
        use convert::*;

        let detail: ob12a::ActionType = action.action.try_into().map_err(OCErr::deserialize)?;
        let detail = match detail {
            ob12a::ActionType::GetLatestEvents(_) => {
                // please get events using ob11 http post
                return Err(OCErr::not_supported("please poll events with HTTP POST").into());
            }
            ob12a::ActionType::GetSupportedActions(_) => {
                return Err(OCErr::not_supported("ob11 side do not support").into())
            }
            ob12a::ActionType::GetStatus(action) => convert_action_default(action)?,
            ob12a::ActionType::GetVersion(action) => convert_action_default(action)?,
            ob12a::ActionType::GetSelfInfo(action) => convert_action_default(action)?,
            ob12a::ActionType::GetUserInfo(action) => convert_action_default(action)?,
            ob12a::ActionType::GetFriendList(action) => convert_action_default(action)?,
            ob12a::ActionType::SendMessage(action) => {
                convert_action_async(action, |msg| async move {
                    self.ob11_to_ob12_seg(msg, app).await
                })
                .await?
            }
            ob12a::ActionType::DeleteMessage(action) => convert_action_default(action)?,
            ob12a::ActionType::GetGroupInfo(action) => convert_action_default(action)?,
            ob12a::ActionType::GetGroupList(action) => convert_action_default(action)?,
            ob12a::ActionType::GetGroupMemberInfo(action) => convert_action_default(action)?,
            ob12a::ActionType::GetGroupMemberList(action) => convert_action_default(action)?,
            ob12a::ActionType::SetGroupName(action) => convert_action_default(action)?,
            ob12a::ActionType::LeaveGroup(action) => convert_action_default(action)?,
            ob12a::ActionType::GetFile(action) => {
                use ob12a::*;

                #[inline]
                fn mk_response<R: serde::Serialize>(resp: R) -> Result<ActionConverted, AllErr> {
                    Ok(ActionConverted::Respond(RespArgs::success(
                        ValueMap::deserialize(serde_value::to_value(resp)?)?,
                    )))
                }
                #[inline]
                fn mk_missing_file() -> Result<ActionConverted, AllErr> {
                    Ok(ActionConverted::Respond(RespArgs::failed(
                        RetCode::FilesystemError(33404),
                        "cannot find specified file",
                    )))
                }

                let uuid = Uuid::from_str(&action.file_id).map_err(UploadError::Uuid)?;
                match action.r#type {
                    GetFileType::Url => match self.storage.get_url(&uuid).await? {
                        Some(url) => {
                            ob12a::GetFileResp {
                                file: FileOpt { kind: , name: (), sha256: () },
                                extra: todo!(),
                            }
                        }
                        None => return mk_missing_file(),
                    },
                    GetFileType::Path => todo!(),
                    GetFileType::Data => todo!(),
                    GetFileType::Other(_) => todo!(),
                }
            }
            ob12a::ActionType::Other(ob12a::ActionDetail { action, params }) => {
                CompatAction::from_data(action, params)
                    .map_err(OCErr::other)?
                    .into()
            }
        };

        Ok(ActionConverted::Send(RawAction {
            detail: detail.try_into()?,
            echo: action.echo,
        }))
    }
}
