use std::{fmt::Display, ops::Deref, str::FromStr, sync::Arc};

use dashmap::DashMap;
use fxhash::FxHashMap;
use parking_lot::RwLock;
use serde::{de::Error as DeErr, ser::Error as SerErr};
use serde_value::{DeserializerError, SerializerError};
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
    compat::{
        action::{bot::OB11File, IntoOB11Action, IntoOB11ActionAsync},
        event::IntoOB12EventAsync,
        message::{FileSeg, IntoOB11Seg, IntoOB11SegAsync, IntoOB12Seg, IntoOB12SegAsync},
    },
    ob11::{action as ob11a, event as ob11e, message as ob11m, MessageSeg},
    ob12::{self, action as ob12a, event as ob12e, message as ob12m},
};

use onebot_connect_interface::{app::OBApp, Error as OCErr};

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

#[derive(Default)]
struct AppDataInner {
    bot_self: RwLock<Option<ob12::BotSelf>>,
    self_id: RwLock<String>,
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

    async fn ob12_to_ob11_seg<A: OBApp>(
        &self,
        msg: ob12::MessageSeg,
        app: &A,
    ) -> Result<MessageSeg, DeserializerError> {
        use convert::{ob12_to_11_seg, ob12_to_11_seg_async};
        let find = |name: String| async move { self.find_file(&name).await.map_err(DeErr::custom) };
        match msg {
            ob12m::MessageSeg::Text(text) => ob12_to_11_seg(text),
            ob12m::MessageSeg::Mention(mention) => ob12_to_11_seg(mention),
            ob12m::MessageSeg::MentionAll(mention_all) => ob12_to_11_seg(mention_all),
            ob12m::MessageSeg::Location(location) => ob12_to_11_seg(location),
            ob12m::MessageSeg::Reply(reply) => ob12_to_11_seg(reply),
            ob12m::MessageSeg::Image(image) => ob12_to_11_seg_async(image, find).await,
            ob12m::MessageSeg::Voice(voice) => ob12_to_11_seg_async(voice, find).await,
            ob12m::MessageSeg::Audio(audio) => ob12_to_11_seg_async(audio, find).await,
            ob12m::MessageSeg::Video(video) => ob12_to_11_seg_async(video, find).await,
            ob12m::MessageSeg::File(_) => {
                ob12_to_11_seg(ob12m::MessageSeg::File(Default::default()))
            }
            ob12m::MessageSeg::Other { r#type, data } => {
                ob12_to_11_seg(ob12m::MessageSeg::Other { r#type, data })
            }
        }
    }

    async fn ob11_to_ob12_seg<A: OBApp>(
        &self,
        msg: MessageSeg,
        app: &A,
    ) -> Result<ob12m::MessageSeg, SerializerError> {
        match msg {
            MessageSeg::Text(text) => ob11_to_12_seg_default(text),
            MessageSeg::Face(face) => ob11_to_12_seg_default(face),
            MessageSeg::Image(image) => {
                ob11_to_12_seg_async(image, |name| async move {
                    Ok(self.trace_file(FileType::Image(name)).await)
                })
                .await
            }
            MessageSeg::Record(record) => ob11_to_12_seg_async(record, trace_fn).await,
            MessageSeg::Video(video) => ob11_to_12_seg_async(video, trace_fn).await,
            MessageSeg::At(at) => ob11_to_12_seg_default(at),
            MessageSeg::Rps(rps) => ob11_to_12_seg_default(rps),
            MessageSeg::Dice(dice) => ob11_to_12_seg_default(dice),
            MessageSeg::Shake(shake) => ob11_to_12_seg_default(shake),
            MessageSeg::Poke(poke) => ob11_to_12_seg_default(poke),
            MessageSeg::Anonymous(anonymous) => ob11_to_12_seg_default(anonymous),
            MessageSeg::Share(share) => ob11_to_12_seg_default(share),
            MessageSeg::Contact(contact) => ob11_to_12_seg_default(contact),
            MessageSeg::Location(location) => ob11_to_12_seg_default(location),
            MessageSeg::Music(music) => ob11_to_12_seg_default(music),
            MessageSeg::Reply(reply) => ob11_to_12_seg(reply.clone(), {
                use onebot_connect_interface::app::AppExt;
                let r = app
                    .call_action(
                        GetMsg {
                            message_id: reply.id,
                        },
                        None,
                    )
                    .await
                    .map_err(SerializerError::custom)?;
                r.sender.user_id().map(|id| id.to_string())
            }),
            MessageSeg::Forward(forward) => ob11_to_12_seg_default(forward),
            MessageSeg::Node(forward_node) => ob11_to_12_seg_default(forward_node),
            MessageSeg::Xml(xml) => ob11_to_12_seg_default(xml),
            MessageSeg::Json(json) => ob11_to_12_seg_default(json),
            seg => Err(SerializerError::custom(format!(
                "unknown ob11 message seg: {:?}",
                seg
            ))),
        }
    }

    async fn convert_msg_event<A: OBApp>(
        &self,
        msg: MessageEvent,
        app: &A,
    ) -> Result<ob12e::EventType, AllErr> {
        let msg_convert = |msg| async { self.ob11_to_ob12_seg(msg, app).await };
        Ok(msg
            .into_ob12((self.inner.read().self_id.clone(), msg_convert))
            .await
            .map_err(OCErr::serialize)?)
    }

    async fn convert_action<A: OBApp>(
        &self,
        action: ob12a::ActionType,
        app: &A,
    ) -> Result<ActionType, DeserializerError> {
        match action {
            ob12a::ActionType::GetLatestEvents(_) => {
                Err(DeErr::custom("please poll events with HTTP POST"))
            }
            ob12a::ActionType::GetSupportedActions(_) => {
                Err(DeErr::custom("ob11 side do not support"))
            }
            ob12a::ActionType::GetStatus(action) => convert_action_default(action),
            ob12a::ActionType::GetVersion(action) => convert_action_default(action),
            ob12a::ActionType::GetSelfInfo(action) => convert_action_default(action),
            ob12a::ActionType::GetUserInfo(action) => convert_action_default(action),
            ob12a::ActionType::GetFriendList(action) => convert_action_default(action),
            ob12a::ActionType::SendMessage(action) => {
                convert_action_async(action, |msg| async move {
                    self.ob11_to_ob12_seg(msg, app).await
                })
                .await
            }
            ob12a::ActionType::DeleteMessage(action) => Some(action.into_ob11(()).unwrap().into()),
            ob12a::ActionType::GetGroupInfo(action) => Some(action.into_ob11(()).unwrap().into()),
            ob12a::ActionType::GetGroupList(action) => Some(action.into_ob11(()).unwrap().into()),
            ob12a::ActionType::GetGroupMemberInfo(action) => {
                Some(action.into_ob11(()).unwrap().into())
            }
            ob12a::ActionType::GetGroupMemberList(action) => {
                Some(action.into_ob11(()).unwrap().into())
            }
            ob12a::ActionType::SetGroupName(action) => Some(action.into_ob11(()).unwrap().into()),
            ob12a::ActionType::LeaveGroup(action) => Some(action.into_ob11(()).unwrap().into()),
            ob12a::ActionType::GetFile(action) => Some(
                action
                    .into_ob11(|_| async { FileType::Record("sadwa".into()) })
                    .await
                    .unwrap()
                    .into(),
            ),
            ob12a::ActionType::Other(action) => {
                CompatAction::from_data(&action.action, action.params).unwrap();
                None
            }
            _ => None,
        }
    }
}
