use std::{
    collections::HashMap,
    fmt::{Debug, Display},
    future::Future,
    ops::{Deref, DerefMut},
    path::PathBuf,
};

use onebot_types::ob12::action::{GetFileFrag, GetFileFragmented, UploadFileReq};

#[derive(Debug, Clone)]
pub struct FileInfo<T: Debug + Clone> {
    pub name: String,
    pub inner: T,
}

impl<T: Debug + Clone> Deref for FileInfo<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl<T: Debug + Clone> DerefMut for FileInfo<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[derive(Clone, Debug)]
pub struct UrlUpload {
    pub url: String,
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Clone)]
pub enum UploadKind {
    Url(UrlUpload),
    Path(PathBuf),
    Data(bytes::Bytes),
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("file not exists: `{0}`")]
    NotExists(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Other(String),
}

impl UploadError {
    pub fn other<E: Display>(e: E) -> Self {
        Self::Other(e.to_string())
    }

    pub fn not_exists(e: impl Into<String>) -> Self {
        Self::NotExists(e.into())
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
    ) -> impl Future<Output = Result<String, UploadError>>;

    fn upload_fragmented(
        &self,
        upload: UploadFileReq,
    ) -> impl Future<Output = Result<Option<String>, UploadError>>;

    fn get_url(
        &self,
        id: impl AsRef<str>,
    ) -> impl Future<Output = Result<Option<FileInfo<UrlUpload>>, UploadError>>;

    fn get_store_state(
        &self,
        id: impl AsRef<str>,
    ) -> impl Future<Output = Result<StoreState, UploadError>>;

    fn is_cached(&self, id: impl AsRef<str>) -> impl Future<Output = Result<bool, UploadError>>;

    fn get_path(
        &self,
        id: impl AsRef<str>,
    ) -> impl Future<Output = Result<Option<FileInfo<PathBuf>>, UploadError>>;

    fn get_data(
        &self,
        id: impl AsRef<str>,
    ) -> impl Future<Output = Result<Option<FileInfo<Vec<u8>>>, UploadError>>;

    fn get_fragmented(
        &self,
        get: GetFileFragmented,
    ) -> impl Future<Output = Result<Option<GetFileFrag>, UploadError>>;
}
