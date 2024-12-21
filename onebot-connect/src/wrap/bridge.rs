use std::error::Error as ErrTrait;

use onebot_connect_interface::{
    app::{self, Connect, OBApp, OBAppProvider},
    imp::{self, Action, Create, OBImpl, OBImplProvider},
    AllResult, RespArgs,
};
use tokio::task::{JoinError, JoinHandle};

/// A simple connection bridge, use the OneBot Application as a source, OneBot Implementation as a sink, bridge between them.
pub struct OBridge<A, I>
where
    A: OBAppProvider,
    I: OBImplProvider,
{
    app_side: A,
    impl_side: I,
    app_task: JoinHandle<AllResult<()>>,
    impl_task: JoinHandle<AllResult<()>>,
}

impl<A, I> OBridge<A, I>
where
    A: OBAppProvider,
    I: OBImplProvider,
{
    pub async fn create<AC, IC>(app_conn: AC, impl_create: IC) -> Result<Self, Box<dyn ErrTrait>>
    where
        AC: Connect<Provider = A>,
        IC: Create<Provider = I>,
    {
        let (app_msg_src, mut app_prov, _) = app_conn
            .connect()
            .await
            .map_err(|e| format!("app connect err: {e}"))?;
        let (impl_msg_src, mut impl_prov, _) = impl_create
            .create()
            .await
            .map_err(|e| format!("impl connect err: {e}"))?;

        if app_prov.use_event_context() {
            log::warn!("Event context is being used by the app provider, actions will be ignored.");
        }

        async fn app_task(
            mut app_src: impl app::MessageSource,
            impl_: impl OBImpl,
        ) -> AllResult<()> {
            while let Some(msg) = app_src.poll_message().await {
                match msg {
                    app::RecvMessage::Event(e) => impl_.send_event_impl(e).await?,
                    app::RecvMessage::Close(close) => match close {
                        Ok(r) => {
                            log::info!("app connection recv closed: {r:?}");
                        }
                        Err(e) => {
                            log::error!("app connection close error: {e}")
                        }
                    },
                }
            }

            Ok(())
        }
        let impl_ = impl_prov.provide()?;
        let app_handle = tokio::spawn(app_task(app_msg_src, impl_));

        async fn impl_task(
            mut impl_src: impl imp::MessageSource,
            mut app: impl OBApp,
            impl_: impl OBImpl,
            use_event: bool,
        ) -> AllResult<()> {
            while let Some(msg) = impl_src.poll_message().await {
                match msg {
                    imp::RecvMessage::Action(Action {
                        detail,
                        echo,
                        self_,
                    }) => {
                        if use_event {
                            continue;
                        }

                        match app.send_action_impl(detail, self_).await {
                            Ok(resp) => {
                                if impl_.respond_supported() {
                                    let resp_args: RespArgs = resp
                                        .map(Into::into)
                                        .unwrap_or_else(|| RespArgs::success(()).unwrap());
                                    if let Err(e) = impl_.respond_impl(echo, resp_args).await {
                                        log::error!("error while responding: {e}")
                                    }
                                }
                            }
                            Err(e) => {
                                log::error!("error while sending action: {e}")
                            }
                        }
                    }
                    imp::RecvMessage::Close(close) => match close {
                        Ok(r) => {
                            log::info!("impl connection recv closed: {r:?}");
                        }
                        Err(e) => {
                            log::info!("impl connection close error: {e}")
                        }
                    },
                }
            }

            app.release().await?;
            AllResult::Ok(())
        }

        let app = app_prov.provide()?;
        let impl_ = impl_prov.provide()?;
        let use_event = app_prov.use_event_context();
        let impl_handle = tokio::spawn(impl_task(impl_msg_src, app, impl_, use_event));

        Ok(Self {
            app_side: app_prov,
            impl_side: impl_prov,
            app_task: app_handle,
            impl_task: impl_handle,
        })
    }

    pub async fn close(mut self) -> AllResult<()> {
        self.app_side.provide()?.close().await?;
        self.impl_side.provide()?.close().await?;

        Ok(())
    }

    pub async fn join(self) -> Result<(AllResult<()>, AllResult<()>), JoinError> {
        let app_res = self.app_task.await?;
        let imp_res = self.impl_task.await?;

        Ok((app_res, imp_res))
    }
}
