#[cfg(feature = "hyper")]
pub mod http_s;
#[cfg(feature = "ws")]
pub mod ws;

#[cfg(feature = "storage")]
pub mod storage;

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use onebot_connect_interface::ClosedReason;
use onebot_types::ob12::action::{GetFileFrag, GetFileFragmented, UploadFileReq};
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use uuid::Uuid;

#[derive(Debug)]
struct ConnStateInner {
    active: AtomicBool,
}

impl Default for ConnStateInner {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(true),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConnState(Arc<ConnStateInner>);

impl ConnState {
    pub fn is_active(&self) -> bool {
        self.0.active.load(Ordering::Acquire)
    }

    pub fn set_active(&self, active: bool) {
        self.0.active.store(active, Ordering::Release)
    }
}

pub(self) enum Signal {
    Close(oneshot::Sender<Result<(), String>>),
}

/// Command handler trait
/// Command is the data received from user code
pub trait CmdHandler<C, M> {
    fn handle_cmd(
        &mut self,
        cmd: C,
        state: ConnState,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_;
}

impl<F, R, C, M> CmdHandler<C, M> for F
where
    F: Fn(C, ConnState) -> R,
    R: Future<Output = Result<(), crate::Error>> + Send + 'static,
{
    fn handle_cmd(
        &mut self,
        cmd: C,
        state: ConnState,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_ {
        self(cmd, state)
    }
}

/// Receive handler trait, handles data received from the connection
/// It produces message for user code at most time
pub trait RecvHandler<D, M> {
    fn handle_recv(
        &mut self,
        recv: D,
        msg_tx: UnboundedSender<M>,
        state: ConnState,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_;
}

impl<F, D, R, M> RecvHandler<D, M> for F
where
    F: Fn(D, UnboundedSender<M>, ConnState) -> R,
    R: Future<Output = Result<(), crate::Error>> + Send + 'static,
{
    fn handle_recv(
        &mut self,
        recv: D,
        msg_tx: UnboundedSender<M>,
        state: ConnState,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_ {
        self(recv, msg_tx, state)
    }
}

pub trait CloseHandler<M> {
    fn handle_close(
        &mut self,
        result: Result<ClosedReason, String>,
        msg_tx: UnboundedSender<M>,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_;
}
