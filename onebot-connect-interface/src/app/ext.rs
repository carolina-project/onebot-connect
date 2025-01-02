use std::{fmt::Display, future::Future};

use crate::Error;
use onebot_types::{
    base::{IntoMessageChain, OBAction},
    ob12::{
        action::{GetLatestEvents, SendMessage, SendMessageResp},
        event::{message::MessageExtacted, RawEvent},
        message::Reply,
        BotSelf, ChatTarget, MessageEvent,
    },
};

pub trait MsgTarget {
    fn extract(self) -> Result<(ChatTarget, Option<BotSelf>), Error>;
}
impl MsgTarget for ChatTarget {
    fn extract(self) -> Result<(ChatTarget, Option<BotSelf>), Error> {
        Ok((self, None))
    }
}
impl MsgTarget for (ChatTarget, BotSelf) {
    fn extract(self) -> Result<(ChatTarget, Option<BotSelf>), Error> {
        Ok((self.0, Some(self.1)))
    }
}
impl MsgTarget for &MessageEvent {
    fn extract(self) -> Result<(ChatTarget, Option<BotSelf>), Error> {
        let chat = self
            .get_chat_target()
            .ok_or_else(|| Error::other("unknown message event type"))?;
        let self_ = self.get_self().cloned();
        Ok((chat, self_))
    }
}
impl MsgTarget for MessageEvent {
    fn extract(self) -> Result<(ChatTarget, Option<BotSelf>), Error> {
        let extracted = self
            .into_extracted()
            .map_err(|_| Error::other("unknown message event type"))?;
        Ok((extracted.chat_target, Some(extracted.self_)))
    }
}
impl MsgTarget for MessageExtacted {
    fn extract(self) -> Result<(ChatTarget, Option<BotSelf>), Error> {
        Ok((self.chat_target, Some(self.self_)))
    }
}

pub trait ReplyTarget {
    fn extract(self) -> Result<(String, ChatTarget, Option<BotSelf>), Error>;
}
impl ReplyTarget for &MessageEvent {
    fn extract(self) -> Result<(String, ChatTarget, Option<BotSelf>), Error> {
        let (Some(msg_id), Some(target), Some(self_)) = (
            self.message_id().map(|r| r.to_owned()),
            self.get_chat_target(),
            self.get_self().cloned(),
        ) else {
            return Err(Error::other("unknown message event type"));
        };

        Ok((msg_id, target, Some(self_)))
    }
}
impl ReplyTarget for MessageEvent {
    fn extract(self) -> Result<(String, ChatTarget, Option<BotSelf>), Error> {
        let extracted = self
            .into_extracted()
            .map_err(|_| Error::other("unknown message event type"))?;
        Ok((
            extracted.message_id,
            extracted.chat_target,
            Some(extracted.self_),
        ))
    }
}
impl ReplyTarget for MessageExtacted {
    fn extract(self) -> Result<(String, ChatTarget, Option<BotSelf>), Error> {
        Ok((self.message_id, self.chat_target, Some(self.self_)))
    }
}
impl<S: Into<String>> ReplyTarget for (S, ChatTarget) {
    fn extract(self) -> Result<(String, ChatTarget, Option<BotSelf>), Error> {
        Ok((self.0.into(), self.1, None))
    }
}
impl<S: Into<String>> ReplyTarget for (S, ChatTarget, BotSelf) {
    fn extract(self) -> Result<(String, ChatTarget, Option<BotSelf>), Error> {
        Ok((self.0.into(), self.1, Some(self.2)))
    }
}

#[inline]
fn process_resp<R>(resp: Option<R>) -> Result<R, Error> {
    match resp {
        Some(res) => Ok(res),
        None => Err(Error::NotSupported("action response not supported".into())),
    }
}

/// Extension trait for `App` to provide additional functionalities
pub trait AppExt {
    /// Send an action, return `Ok(Some(A::Resp))` if `App` is responsive, `Err()` if error
    /// occurred while processing action.
    fn send_action<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<Option<A::Resp>, Error>> + Send + '_
    where
        A: OBAction + Send + 'static;

    /// Only send action, ignore its response excluding error.
    fn send_action_only<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_
    where
        A: OBAction + Send + 'static,
    {
        let fut = self.send_action(action, self_);
        async move { fut.await.map(|_| ()) }
    }

    /// Send an action, treating unresponsive as an error.
    fn call_action<A>(
        &self,
        action: A,
        self_: Option<BotSelf>,
    ) -> impl Future<Output = Result<A::Resp, Error>> + Send + '_
    where
        A: OBAction + Send + 'static,
    {
        let fut = self.send_action(action, self_);
        async move { fut.await?.ok_or_else(|| Error::missing("response")) }
    }

    fn get_latest_events(
        &self,
        limit: i64,
        timeout: i64,
        self_: Option<BotSelf>,
    ) -> impl std::future::Future<Output = Result<Vec<RawEvent>, Error>> + Send + '_ {
        let action = GetLatestEvents {
            limit,
            timeout,
            extra: Default::default(),
        };
        let fut = self.send_action(action, self_);
        async move { process_resp(fut.await?) }
    }

    fn send_message(
        &self,
        target: impl MsgTarget,
        message: impl IntoMessageChain<Error: Display>,
    ) -> impl Future<Output = Result<Option<SendMessageResp>, Error>> + Send + '_ {
        let fut = target.extract().and_then(|(target, self_)| {
            Ok(self.send_action(
                SendMessage {
                    target,
                    message: message.into_msg_chain().map_err(Error::other)?,
                    extra: Default::default(),
                },
                self_,
            ))
        });

        async move {
            match fut {
                Ok(fut) => fut.await,
                Err(e) => Err(e),
            }
        }
    }

    fn reply(
        &self,
        target: impl ReplyTarget,
        message: impl IntoMessageChain<Error: Display>,
    ) -> impl Future<Output = Result<Option<SendMessageResp>, Error>> + Send + '_ {
        let fut = target.extract().and_then(move |(id, target, self_)| {
            let mut msg = message.into_msg_chain().map_err(Error::other)?;
            msg.append_front(Reply {
                message_id: id,
                user_id: None,
                extra: Default::default(),
            })
            .map_err(Error::other)?;
            Ok(self.send_action(
                SendMessage {
                    target,
                    message: msg,
                    extra: Default::default(),
                },
                self_,
            ))
        });

        async move {
            match fut {
                Ok(fut) => fut.await,
                Err(e) => Err(e),
            }
        }
    }
}
