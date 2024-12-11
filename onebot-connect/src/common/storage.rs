use std::{io, path::Path, str::FromStr};

use bytes::Bytes;
use dashmap::DashMap;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::*;

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("url error: {0}")]
    ParseUrl(url::ParseError),
    #[error("{0}")]
    Other(String),
}

impl FsError {
    pub fn other<E: Display>(e: E) -> Self {
        Self::Other(e.to_string())
    }
}

pub trait FS: Send + Sync {
    fn try_exists(&self, path: impl AsRef<Path>) -> impl Future<Output = Result<bool, FsError>>;

    fn write(
        &self,
        path: impl AsRef<Path>,
        offset: usize,
        data: &[u8],
    ) -> impl Future<Output = Result<(), FsError>>;

    fn read(
        &self,
        path: impl AsRef<Path>,
        offset: usize,
        size: usize,
    ) -> impl Future<Output = Result<Bytes, FsError>>;

    fn rename(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> impl Future<Output = Result<(), FsError>>;

    fn delete(&self, path: impl AsRef<Path>) -> impl Future<Output = Result<(), FsError>>;
}

pub struct LocalFs;

impl FS for LocalFs {
    async fn try_exists(&self, path: impl AsRef<Path>) -> Result<bool, FsError> {
        Ok(tokio::fs::try_exists(path).await?)
    }

    async fn write(
        &self,
        path: impl AsRef<Path>,
        offset: usize,
        data: &[u8],
    ) -> Result<(), FsError> {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(path)
            .await?;

        file.seek(std::io::SeekFrom::Start(offset as u64)).await?;
        file.write_all(data).await?;
        Ok(())
    }

    async fn read(
        &self,
        path: impl AsRef<Path>,
        offset: usize,
        size: usize,
    ) -> Result<Bytes, FsError> {
        let mut file = tokio::fs::File::open(path).await?;
        file.seek(std::io::SeekFrom::Start(offset as u64)).await?;

        let mut buf = vec![0; size];
        let read_n = file.read_exact(&mut buf).await?;
        buf.truncate(read_n);
        Ok(Bytes::from(buf))
    }

    async fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<(), FsError> {
        Ok(tokio::fs::rename(from, to).await?)
    }

    async fn delete(&self, path: impl AsRef<Path>) -> Result<(), FsError> {
        Ok(tokio::fs::remove_file(path).await?)
    }
}

pub enum UploadSource {
    Url {
        url: UrlUpload,
        path: Option<PathBuf>,
    },
    Path(PathBuf),
}

pub struct UploadedFile {
    source: UploadSource,
    file_name: String,
}

pub struct OBFileStorage<F: FS> {
    fs: F,
    lazy: bool,
    upd_path_base: String,
    map: DashMap<Uuid, UploadedFile>,
    frag_map: DashMap<Uuid, (String, PathBuf)>,
    http: reqwest::Client,
}

impl<F: FS> OBFileStorage<F> {
    async fn download_file(
        &self,
        path: impl AsRef<Path>,
        url: UrlUpload,
    ) -> Result<(), UploadError> {
        let mut req = self.http.get(url.url);
        if let Some(headers) = url.headers {
            req = req.headers(headers);
        }
        Ok(self
            .fs
            .write(path, 0, &req.send().await?.bytes().await?)
            .await?)
    }

    fn random_file(&self, file_name: &str) -> (Uuid, PathBuf) {
        let file = PathBuf::from_str(file_name.as_ref()).unwrap();
        let ext = file.extension();
        let uuid = Uuid::new_v4();
        let full_path: PathBuf = match ext {
            Some(ext) => format!("{}/{uuid}.{}", self.upd_path_base, ext.to_string_lossy()).into(),
            None => format!("{}/{uuid}.upload", self.upd_path_base).into(),
        };

        (uuid, full_path)
    }
}

impl<F: FS> UploadStorage for OBFileStorage<F> {
    async fn upload(
        &self,
        file_name: impl AsRef<str>,
        upload: UploadKind,
    ) -> Result<Uuid, UploadError> {
        let (uuid, full_path) = self.random_file(file_name.as_ref());
        match upload {
            UploadKind::Data(data) => {
                self.fs.write(full_path, 0, &data).await?;
            }
            UploadKind::Url(url) => {
                if !self.lazy {
                    self.download_file(&full_path, url.clone()).await?;
                }
                self.map.insert(
                    uuid.clone(),
                    UploadedFile {
                        source: UploadSource::Url {
                            url,
                            path: Some(full_path.into()),
                        },
                        file_name: file_name.as_ref().to_owned(),
                    },
                );
            }
            UploadKind::Path(path) => {
                self.map.insert(
                    uuid.clone(),
                    UploadedFile {
                        source: UploadSource::Path(path),
                        file_name: file_name.as_ref().to_owned(),
                    },
                );
            }
        };

        Ok(uuid)
    }

    async fn upload_fragmented(&self, upload: UploadFileReq) -> Result<Option<Uuid>, UploadError> {
        match upload {
            UploadFileReq::Prepare {
                name,
                total_size: _,
            } => {
                let (uuid, full_path) = self.random_file(&name);
                self.frag_map.insert(uuid, (name, full_path));
                Ok(Some(uuid))
            }
            UploadFileReq::Transfer {
                file_id,
                offset,
                data,
            } => {
                let uuid = Uuid::parse_str(&file_id)?;
                let Some(res) = self.frag_map.get(&uuid) else {
                    return Err(UploadError::NotExists(uuid));
                };

                self.fs.write(&res.1, offset as usize, &data).await?;
                Ok(None)
            }
            UploadFileReq::Finish { file_id, sha256: _ } => {
                let uuid = Uuid::parse_str(&file_id)?;
                let Some((_, (file_name, path))) = self.frag_map.remove(&uuid) else {
                    return Err(UploadError::NotExists(uuid));
                };
                let new_uuid = Uuid::new_v4();
                self.map.insert(
                    new_uuid.clone(),
                    UploadedFile {
                        source: UploadSource::Path(path),
                        file_name,
                    },
                );
                log::warn!("sha256 not implemented");
                Ok(Some(new_uuid))
            }
        }
    }

    async fn get_url(&self, uuid: &Uuid) -> Result<UrlUpload, UploadError> {
        let Some(file) = self.map.get(uuid) else {
            return Err(UploadError::NotExists(uuid.clone()));
        };
        match &file.source {
            UploadSource::Url { url, path: _ } => Ok(url.clone()),
            UploadSource::Path(_) => Err(UploadError::unsupported("cannot get url from path")),
        }
    }

    async fn get_path(&self, uuid: &Uuid) -> Result<PathBuf, UploadError> {
        let Some(file) = self.map.get(uuid) else {
            return Err(UploadError::NotExists(uuid.clone()));
        };
        match &file.source {
            UploadSource::Url { url, path: _ } => {
            },
            UploadSource::Path(_) => Err(UploadError::unsupported("cannot get url from path")),
        }
    }
}
