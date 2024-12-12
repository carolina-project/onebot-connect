use std::fs;
use std::ops::Deref;
use std::os::unix::fs::MetadataExt;
use std::str::FromStr;
use std::{io, path::Path};

use bytes::Bytes;
use dashmap::mapref::one::{Ref as MapRef, RefMut as MapRefMut};
use dashmap::DashMap;
use onebot_types::ob12::action::GetFileReq;
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

    fn read_to_end(
        &self,
        path: impl AsRef<Path>,
        offset: usize,
    ) -> impl Future<Output = Result<Bytes, FsError>>;

    fn rename(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> impl Future<Output = Result<(), FsError>>;

    fn delete(&self, path: impl AsRef<Path>) -> impl Future<Output = Result<(), FsError>>;

    fn metadata(
        &self,
        path: impl AsRef<Path>,
    ) -> impl Future<Output = Result<fs::Metadata, FsError>>;
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

    async fn read_to_end(&self, path: impl AsRef<Path>, offset: usize) -> Result<Bytes, FsError> {
        let mut file = tokio::fs::File::open(path).await?;
        file.seek(std::io::SeekFrom::Start(offset as u64)).await?;

        let mut buf = vec![];
        file.read_to_end(&mut buf).await?;
        Ok(Bytes::from(buf))
    }

    async fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<(), FsError> {
        Ok(tokio::fs::rename(from, to).await?)
    }

    async fn delete(&self, path: impl AsRef<Path>) -> Result<(), FsError> {
        Ok(tokio::fs::remove_file(path).await?)
    }

    async fn metadata(&self, path: impl AsRef<Path>) -> Result<fs::Metadata, FsError> {
        Ok(tokio::fs::metadata(path).await?)
    }
}

#[derive(Debug)]
pub enum UploadSource {
    Url {
        url: UrlUpload,
        path: Option<PathBuf>,
    },
    Path(PathBuf),
}

#[derive(Debug)]
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

    #[inline]
    fn mk_file_path(&self, uuid: &Uuid, file_name: &str) -> PathBuf {
        let file: PathBuf = file_name.into();
        let full_path: PathBuf = match file.extension() {
            Some(ext) => format!("{}/{uuid}.{}", self.upd_path_base, ext.to_string_lossy()).into(),
            None => format!("{}/{uuid}.upload", self.upd_path_base).into(),
        };

        full_path.into()
    }

    #[inline]
    fn get_file(&self, uuid: &Uuid) -> Result<MapRef<'_, Uuid, UploadedFile>, UploadError> {
        self.map
            .get(uuid)
            .ok_or_else(|| UploadError::not_exists(uuid))
    }

    #[inline]
    fn get_file_mut(&self, uuid: &Uuid) -> Result<MapRefMut<'_, Uuid, UploadedFile>, UploadError> {
        self.map
            .get_mut(uuid)
            .ok_or_else(|| UploadError::not_exists(uuid))
    }

    fn random_file(&self, file_name: &str) -> (Uuid, PathBuf) {
        let uuid = Uuid::new_v4();

        (uuid, self.mk_file_path(&uuid, file_name))
    }

    async fn load_file_path(
        &self,
        uuid: &Uuid,
        file: MapRef<'_, Uuid, UploadedFile>,
    ) -> Result<PathBuf, UploadError> {
        Ok(match &file.source {
            UploadSource::Url { url, path } => {
                if let Some(path) = path {
                    path.clone()
                } else {
                    // if file hasn't cached
                    let full_path = self.mk_file_path(uuid, &file.file_name);
                    let url = url.clone();

                    self.download_file(&full_path, url).await?;
                    // record file path
                    let mut item = self.get_file_mut(uuid)?;
                    match &mut item.source {
                        UploadSource::Url { url: _, path } => {
                            *path = Some(full_path.clone());
                            drop(item);
                        }
                        UploadSource::Path(_) => {
                            return Err(UploadError::other(format!(
                                "`{}`: expected `UploadSource::Url`",
                                uuid
                            )))
                        }
                    }

                    full_path
                }
            }
            UploadSource::Path(path) => path.clone(),
        })
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
                            path: Some(full_path),
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
        let file = self.get_file(uuid)?;
        match &file.source {
            UploadSource::Url { url, path: _ } => Ok(url.clone()),
            UploadSource::Path(_) => Err(UploadError::unsupported("cannot get url from path")),
        }
    }

    async fn get_path(&self, uuid: &Uuid) -> Result<PathBuf, UploadError> {
        let file = self
            .map
            .get(uuid)
            .ok_or_else(|| UploadError::not_exists(uuid))?;
        match &file.source {
            UploadSource::Url { url, path } => {
                if let Some(path) = path {
                    Ok(path.clone())
                } else {
                    let full_path = self.mk_file_path(uuid, &file.file_name);
                    let url = url.clone();
                    drop(file);
                    self.download_file(&full_path, url).await?;
                    Ok(full_path)
                }
            }
            UploadSource::Path(path) => Ok(path.clone()),
        }
    }

    async fn is_cached(&self, uuid: &Uuid) -> Result<bool, UploadError> {
        let file = self.get_file(uuid)?;

        match &file.source {
            UploadSource::Url { url: _, path } => Ok(path.is_some()),
            UploadSource::Path(_) => Ok(true),
        }
    }

    async fn get_data(&self, uuid: &Uuid) -> Result<Bytes, UploadError> {
        let file = self.get_file(uuid)?;
        let path = self.load_file_path(uuid, file).await?;

        Ok(self.fs.read_to_end(path, 0).await?)
    }

    async fn get_fragmented(&self, get: GetFileFragmented) -> Result<GetFileFrag, UploadError> {
        let uuid = Uuid::from_str(&get.file_id)?;
        let file = self.get_file(&uuid)?;
        match get.req {
            GetFileReq::Prepare => GetFileFrag::Prepare {
                name: file.file_name.clone(),
                total_size: { self.fs.metadata().await?.size() },
                sha256: (),
            },
            GetFileReq::Transfer { offset, size } => todo!(),
        }
    }
}
