use std::error::Error as ErrTrait;

use onebot_connect_interface::{
    app::{self, Connect, MessageSource as AMessageSource, OBApp, OBAppProvider},
    imp::{self, Action, Create, MessageSource as IMessageSource, OBImpl, OBImplProvider}, ActionResult,
};

pub struct OBridge<A, I>
where
    A: OBAppProvider,
    I: OBImplProvider,
{
    app_side: A,
    impl_side: I,
}

impl<A, I> OBridge<A, I>
where
    A: OBAppProvider,
    I: OBImplProvider,
{
    pub async fn create<AC, IC>(app_c: AC, impl_c: IC) -> Result<Self, Box<dyn ErrTrait>>
    where
        AC: Connect,
        IC: Create,
    {
        let (app_msg_src, app_prov, _) = app_c.connect().await?;
        let (impl_msg_src, impl_prov, _) = impl_c.create().await?;

        tokio::spawn(async {
            while let Some(msg) = app_msg_src.poll_message().await {
                let impl_ = impl_prov.provide().unwrap();
                match msg {
                    app::RecvMessage::Event(e) => impl_.send_event_impl(e).await.unwrap(),
                    app::RecvMessage::Close(close) => impl_.close().await.unwrap(),
                }
            }
        });

        tokio::spawn(async { while let Some(msg) = impl_msg_src.poll_message().await {
            match msg {
                imp::RecvMessage::Action(Action {
                    detail: action,
                    echo,
                    self_,
                }) => {
                    let app = app_prov.provide().unwrap();
                    app.send_action_impl(action, self_)
                },
                imp::RecvMessage::Close(_) => todo!(),
            }
        } });

        Ok(())
    }
}
