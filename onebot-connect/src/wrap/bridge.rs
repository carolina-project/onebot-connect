use std::error::Error as ErrTrait;

use onebot_connect_interface::{
    app::{self, Connect, OBApp, OBAppProvider},
    imp::{self, Action, Create, OBImpl, OBImplProvider},
    AllResult, RespArgs,
};
use onebot_types::ob12::event::RawEvent;
use tokio::{
    sync::broadcast::{self, error::TryRecvError},
    task::{JoinError, JoinHandle},
};

use crate::common::util;

/// A simple connection bridge, use the OneBot Application as a source, OneBot Implementation as a sink, bridge between them.
pub struct OBridge<A>
where
    A: OBAppProvider,
{
    app_side: A,
    app_handle: JoinHandle<()>,
    impl_handle: JoinHandle<()>,
    close_tx: broadcast::Sender<()>,
}

impl<A> OBridge<A>
where
    A: OBAppProvider,
{
    pub async fn create<AC, IC, F>(
        app_conn: AC,
        impl_create_fn: F,
        event_buf_size: usize,
    ) -> Result<Self, Box<dyn ErrTrait>>
    where
        AC: Connect<Provider = A>,
        IC: Create,
        F: Fn() -> IC + Send + 'static,
    {
        log::debug!("waiting for OneBot app connection...");
        let (app_msg_src, mut app_prov, _) = app_conn
            .connect()
            .await
            .map_err(|e| format!("app connect err: {e}"))?;

        if app_prov.use_event_context() {
            log::warn!("Event context is being used by the app provider, actions will be ignored.");
        }

        type EventTx = util::LossySender<RawEvent>;
        type EventRx = util::LossyReceiver<RawEvent>;

        let (close_tx, mut close_rx) = broadcast::channel::<()>(1);
        let (event_tx, event_rx) = util::lossy_channel(event_buf_size);

        async fn app_task(
            mut app_src: impl app::MessageSource,
            event_tx: EventTx,
            close_tx: broadcast::Sender<()>,
        ) {
            log::debug!("app side started.");
            let mut close_rx = close_tx.subscribe();
            loop {
                tokio::select! {
                    _ = close_rx.recv() => {
                        break
                    },
                    Some(msg) = app_src.poll_message() => {
                        match msg {
                            app::RecvMessage::Event(e) => {
                                log::info!("received event: `{}.{}`", e.event.r#type, e.event.detail_type);
                                event_tx.send(e).unwrap();
                            },
                            app::RecvMessage::Close(close) => match close {
                                Ok(r) => {
                                    close_tx.send(()).unwrap();
                                    log::info!("app connection recv closed: {r:?}");
                                }
                                Err(e) => {
                                    log::error!("app connection close error: {e}")
                                }
                            },
                        }
                    }
                    else => break
                }
            }
        }
        let app_handle = tokio::spawn(app_task(app_msg_src, event_tx, close_tx.clone()));

        async fn impl_task<A: OBApp>(
            mut impl_src: impl imp::MessageSource,
            app: A,
            event_rx: EventRx,
            impl_: impl OBImpl,
            mut close_rx: broadcast::Receiver<()>,
            use_event: bool,
        ) -> (A, EventRx) {
            loop {
                tokio::select! {
                    _ = close_rx.recv() => {
                        break
                    }
                    Some(event) = event_rx.recv() => {
                        match impl_.send_event_impl(event).await {
                            Ok(_) => {},
                            Err(e) => {
                                log::error!("impl side send event err: {e}")
                            },
                        };
                    }
                    Some(msg) = impl_src.poll_message() => {
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
                            imp::RecvMessage::Close(close) => {
                                match close {
                                    Ok(r) => {
                                        log::info!("impl connection recv closed: {r:?}");
                                    }
                                    Err(e) => {
                                        log::error!("impl connection close error: {e}")
                                    }
                                }
                                break
                            },
                        }
                    }
                    else => break
                }
            }

            (app, event_rx)
        }

        let app = app_prov.provide()?;
        let use_event = app_prov.use_event_context();
        let close_tx_clone = close_tx.clone();
        let impl_handle = tokio::spawn(async move {
            let mut app = app;
            let mut event_rx = event_rx;
            while let Err(TryRecvError::Empty) = close_rx.try_recv() {
                log::info!("waiting for impl connection...");
                match impl_create_fn().create().await {
                    Ok((impl_src, mut impl_prov, msg)) => {
                        log::info!("accepted impl connection ({msg:?})");
                        let impl_ = match impl_prov.provide() {
                            Ok(impl_) => impl_,
                            Err(e) => {
                                log::error!("impl provider err: {e}");
                                continue;
                            }
                        };
                        (app, event_rx) = impl_task(
                            impl_src,
                            app,
                            event_rx,
                            impl_,
                            close_tx_clone.subscribe(),
                            use_event,
                        )
                        .await;
                    }
                    Err(e) => {
                        log::error!("implementation side connection err: {e}")
                    }
                }
            }
        });

        Ok(Self {
            app_side: app_prov,
            app_handle,
            impl_handle,
            close_tx,
        })
    }

    pub async fn close(&self) -> Result<(), broadcast::error::SendError<()>> {
        self.close_tx.send(()).map(|_| ())
    }

    pub async fn join(self) -> (Result<(), JoinError>, Result<(), JoinError>) {
        tokio::join!(self.app_handle, self.impl_handle)
    }

    pub fn provide_app(&mut self) -> AllResult<A::Output> {
        self.app_side.provide()
    }
}
