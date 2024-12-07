#[cfg(feature = "http")]
pub mod http_s;

use std::future::Future;

use tokio::sync::mpsc::UnboundedSender;

pub trait CmdHandler<C, M> {
    fn handle_cmd(
        &mut self,
        cmd: C,
        msg_tx: &UnboundedSender<M>,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_;
}

impl<F, R, C, M> CmdHandler<C, M> for F
where
    F: Fn(C, &UnboundedSender<M>) -> R,
    R: Future<Output = Result<(), crate::Error>> + Send + 'static,
{
    fn handle_cmd(
        &mut self,
        cmd: C,
        msg_tx: &UnboundedSender<M>,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_ {
        self(cmd, msg_tx)
    }
}

pub trait RecvHandler<D, M> {
    fn handle_recv(
        &mut self,
        recv: D,
        msg_tx: &UnboundedSender<M>,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_;
}

impl<F, D, R, M> RecvHandler<D, M> for F
where
    F: Fn(D, &UnboundedSender<M>) -> R,
    R: Future<Output = Result<(), crate::Error>> + Send + 'static,
{
    fn handle_recv(
        &mut self,
        recv: D,
        msg_tx: &UnboundedSender<M>,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send + '_ {
        self(recv, msg_tx)
    }
}
