#[cfg(feature = "hyper")]
pub mod http_s;
#[cfg(feature = "ws")]
pub mod ws;

#[cfg(feature = "storage")]
pub mod storage;

use std::{fmt::Display, future::Future, path::PathBuf, sync::Arc};

use http::HeaderMap;
use onebot_connect_interface::ClosedReason;
use onebot_types::ob12::action::{GetFileFrag, GetFileFragmented, UploadFileReq};
use parking_lot::RwLock;
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct UrlUpload {
    pub url: String,
    pub headers: Option<HeaderMap>,
}

#[derive(Clone)]
pub enum UploadKind {
    Url(UrlUpload),
    Path(PathBuf),
    Data(bytes::Bytes),
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[cfg(feature = "storage")]
    #[error(transparent)]
    Fs(#[from] storage::FsError),
    #[cfg(feature = "http")]
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("file not exists: {0}")]
    NotExists(Uuid),
    #[error(transparent)]
    Uuid(#[from] uuid::Error),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Other(String),
}

impl UploadError {
    pub fn other<E: Display>(e: E) -> Self {
        Self::Other(e.to_string())
    }

    pub fn not_exists(e: impl AsRef<Uuid>) -> Self {
        Self::NotExists(e.as_ref().clone())
    }

    pub fn unsupported<E: Display>(e: E) -> Self {
        Self::Unsupported(e.to_string())
    }
}

#[derive(Debug, Clone)]
pub enum StoreState {
    NotCached(UrlUpload),
    Cached(String),
}

pub trait UploadStorage: Send + Sync {
    fn upload(
        &self,
        file_name: impl AsRef<str>,
        upload: UploadKind,
    ) -> impl Future<Output = Result<Uuid, UploadError>>;

    fn upload_fragmented(
        &self,
        upload: UploadFileReq,
    ) -> impl Future<Output = Result<Option<Uuid>, UploadError>>;

    fn get_url(&self, uuid: &Uuid) -> impl Future<Output = Result<Option<UrlUpload>, UploadError>>;

    fn get_store_state(&self, uuid: &Uuid) -> impl Future<Output = Result<StoreState, UploadError>>;

    fn is_cached(&self, uuid: &Uuid) -> impl Future<Output = Result<bool, UploadError>>;

    fn get_path(&self, uuid: &Uuid) -> impl Future<Output = Result<Option<PathBuf>, UploadError>>;

    fn get_data(&self, uuid: &Uuid) -> impl Future<Output = Result<Option<Vec<u8>>, UploadError>>;

    fn get_fragmented(
        &self,
        get: GetFileFragmented,
    ) -> impl Future<Output = Result<Option<GetFileFrag>, UploadError>>;
}

#[derive(Debug)]
struct ConnStateInner {
    active: bool,
}

impl Default for ConnStateInner {
    fn default() -> Self {
        Self { active: true }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConnState(Arc<RwLock<ConnStateInner>>);

impl ConnState {
    pub fn is_active(&self) -> bool {
        self.0.read().active
    }

    pub fn set_active(&self, active: bool) {
        self.0.write().active = active;
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
