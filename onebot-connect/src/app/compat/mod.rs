use std::{fmt::Display, sync::Arc};

use fxhash::FxHashMap;
use onebot_types::{compat::message::IntoOB12SegAsync, ob11::Event};
use parking_lot::RwLock;
use serde::ser::Error;
use serde_value::SerializerError;
use uuid::Uuid;

use crate::Error as AllErr;

use onebot_types::{
    compat::{event::IntoOB12EventAsync, message::IntoOB12Seg},
    ob11::{event::MessageEvent, MessageSeg},
    ob12::{event as ob12e, message as ob12m},
};

use onebot_connect_interface::Error as OCErr;

pub mod http;
pub mod ws;
pub mod ws_re;

enum OB11Recv {
    Event(Event),
}

fn convert_seg<P, M>(msg: M, param: P) -> Result<ob12m::MessageSeg, SerializerError>
where
    M: IntoOB12Seg<P>,
    <M::Output as TryInto<ob12m::MessageSeg>>::Error: Display,
{
    msg.into_ob12(param)?
        .try_into()
        .map_err(SerializerError::custom)
}

async fn convert_seg_async<P, M>(msg: M, param: P) -> Result<ob12m::MessageSeg, SerializerError>
where
    M: IntoOB12SegAsync<P>,
    <M::Output as TryInto<ob12m::MessageSeg>>::Error: Display,
{
    msg.into_ob12(param)
        .await?
        .try_into()
        .map_err(SerializerError::custom)
}

fn convert_seg_default<M>(msg: M) -> Result<ob12m::MessageSeg, SerializerError>
where
    M: IntoOB12Seg<()>,
    <M::Output as TryInto<ob12m::MessageSeg>>::Error: Display,
{
    msg.into_ob12(())?
        .try_into()
        .map_err(SerializerError::custom)
}

#[derive(Default, Clone)]
struct AppDataInner {
    file_map: FxHashMap<Uuid, String>,
    self_id: String,
}

/// Connection status of OneBot 11 connection
#[derive(Default, Clone)]
struct AppData(Arc<RwLock<AppDataInner>>);

impl AppData {
    async fn trace_file(&self, name: String) -> String {
        let file_map = &mut self.0.write().file_map;
        let mut uuid;
        loop {
            uuid = Uuid::new_v4();
            if !file_map.contains_key(&uuid) {
                break;
            }
        }

        file_map.insert(uuid, name);
        uuid.into()
    }

    async fn ob11_to_ob12_seg(
        &self,
        msg: MessageSeg,
    ) -> Result<ob12m::MessageSeg, SerializerError> {
        let trace_fn = |name: String| async move { Ok(self.trace_file(name).await) };
        match msg {
            MessageSeg::Text(text) => convert_seg_default(text),
            MessageSeg::Face(face) => convert_seg_default(face),
            MessageSeg::Image(image) => convert_seg_async(image, trace_fn).await,
            MessageSeg::Record(record) => convert_seg_async(record, trace_fn).await,
            MessageSeg::Video(video) => convert_seg_async(video, trace_fn).await,
            MessageSeg::At(at) => convert_seg_default(at),
            MessageSeg::Rps(rps) => convert_seg_default(rps),
            MessageSeg::Dice(dice) => convert_seg_default(dice),
            MessageSeg::Shake(shake) => convert_seg_default(shake),
            MessageSeg::Poke(poke) => convert_seg_default(poke),
            MessageSeg::Anonymous(anonymous) => convert_seg_default(anonymous),
            MessageSeg::Share(share) => convert_seg_default(share),
            MessageSeg::Contact(contact) => convert_seg_default(contact),
            MessageSeg::Location(location) => convert_seg_default(location),
            MessageSeg::Music(music) => convert_seg_default(music),
            MessageSeg::Reply(reply) => convert_seg(reply, reply_user_id.map(|s| s.to_owned())),
            MessageSeg::Forward(forward) => convert_seg_default(forward),
            MessageSeg::Node(forward_node) => convert_seg_default(forward_node),
            MessageSeg::Xml(xml) => convert_seg_default(xml),
            MessageSeg::Json(json) => convert_seg_default(json),
            seg => Err(SerializerError::custom(format!(
                "unknown ob11 message seg: {:?}",
                seg
            ))),
        }
    }

    async fn convert_msg_event(
        &self,
        msg: MessageEvent,
    ) -> Result<ob12e::EventType, AllErr> {
        let msg_convert = |msg| async { self.ob11_to_ob12_seg(msg, reply_user_id.clone()).await };
        Ok(msg
            .into_ob12((self.0.read().self_id.clone(), msg_convert))
            .await
            .map_err(OCErr::serialize)?)
    }
}
