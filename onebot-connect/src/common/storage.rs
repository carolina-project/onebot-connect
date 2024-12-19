use std::collections::HashMap;
use std::fmt::Display;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::str::FromStr;
use std::{io, path::Path};

use dashmap::mapref::one::{Ref as MapRef, RefMut as MapRefMut};
use dashmap::DashMap;
use http::{HeaderMap, HeaderName, HeaderValue};
use onebot_connect_interface::upload::*;
use onebot_types::ob12::action::{GetFileFrag, GetFileFragmented, GetFileReq, UploadData, UploadFileReq};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use super::*;

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{0}")]
    Other(String),
}

impl FsError {
    pub fn other<E: Display>(e: E) -> Self {
        Self::Other(e.to_string())
    }
}

#[derive(Debug, Clone)]
pub enum FileSource {
    Url {
        url: UrlUpload,
        path: Option<PathBuf>,
    },
    Path(PathBuf),
}

#[derive(Debug, Clone)]
pub struct FileStored {
    source: FileSource,
    file_name: String,
}

pub trait FS: Send + Sync {
    fn try_exists(&self, path: impl AsRef<Path>) -> impl Future<Output = Result<bool, FsError>>;

    fn write(
        &self,
        path: impl AsRef<Path>,
        offset: u64,
        data: &[u8],
    ) -> impl Future<Output = Result<(), FsError>>;

    fn read(
        &self,
        path: impl AsRef<Path>,
        offset: u64,
        size: usize,
    ) -> impl Future<Output = Result<Vec<u8>, FsError>>;

    fn read_to_end(
        &self,
        path: impl AsRef<Path>,
        offset: u64,
    ) -> impl Future<Output = Result<Vec<u8>, FsError>>;

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

#[derive(Debug, Default)]
pub struct LocalFs;

impl FS for LocalFs {
    async fn try_exists(&self, path: impl AsRef<Path>) -> Result<bool, FsError> {
        Ok(tokio::fs::try_exists(path).await?)
    }

    async fn write(&self, path: impl AsRef<Path>, offset: u64, data: &[u8]) -> Result<(), FsError> {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(path)
            .await?;

        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.write_all(data).await?;
        Ok(())
    }

    async fn read(
        &self,
        path: impl AsRef<Path>,
        offset: u64,
        size: usize,
    ) -> Result<Vec<u8>, FsError> {
        let mut file = tokio::fs::File::open(path).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;

        let mut buf = vec![0; size];
        let read_n = file.read_exact(&mut buf).await?;
        buf.truncate(read_n);
        Ok(buf)
    }

    async fn read_to_end(&self, path: impl AsRef<Path>, offset: u64) -> Result<Vec<u8>, FsError> {
        let mut file = tokio::fs::File::open(path).await?;
        file.seek(std::io::SeekFrom::Start(offset as u64)).await?;

        let mut buf = vec![];
        file.read_to_end(&mut buf).await?;
        Ok(buf)
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

pub struct OBFileStorage<F: FS = LocalFs> {
    fs: F,
    lazy: bool,
    upd_path_base: String,
    map: DashMap<String, FileStored>,
    /// Map for storing uploading files
    frag_map: DashMap<String, (String, PathBuf)>,
    http: reqwest::Client,
}

impl<F: FS + Default> Default for OBFileStorage<F> {
    fn default() -> Self {
        Self {
            fs: F::default(),
            lazy: true,
            upd_path_base: Default::default(),
            map: Default::default(),
            frag_map: Default::default(),
            http: Default::default(),
        }
    }
}

fn hashmap_to_headers(map: &HashMap<String, String>) -> Result<HeaderMap, http::Error> {
    let mut header_map = HeaderMap::new();
    for (name, value) in map {
        header_map.insert(HeaderName::from_str(&name)?, HeaderValue::from_str(&value)?);
    }

    Ok(header_map)
}

impl<F: FS> OBFileStorage<F> {
    async fn download_file(
        &self,
        path: impl AsRef<Path>,
        url: &UrlUpload,
    ) -> Result<(), UploadError> {
        let mut req = self.http.get(&url.url);
        if let Some(headers) = &url.headers {
            req = req.headers(hashmap_to_headers(&headers).map_err(UploadError::other)?);
        }
        Ok(self
            .fs
            .write(
                path,
                0,
                &req.send()
                    .await
                    .map_err(UploadError::other)?
                    .bytes()
                    .await
                    .map_err(UploadError::other)?,
            )
            .await
            .map_err(UploadError::other)?)
    }

    #[inline]
    fn mk_file_path(&self, id: &str, file_name: &str) -> PathBuf {
        let file: PathBuf = file_name.into();
        let full_path: PathBuf = match file.extension() {
            Some(ext) => format!("{}/{id}.{}", self.upd_path_base, ext.to_string_lossy()).into(),
            None => format!("{}/{id}.upload", self.upd_path_base).into(),
        };

        full_path.into()
    }

    #[inline]
    fn get_file(&self, id: &str) -> Result<MapRef<'_, String, FileStored>, UploadError> {
        self.map
            .get(id)
            .ok_or_else(|| UploadError::not_exists(id.to_owned()))
    }

    #[inline]
    fn get_file_mut(&self, id: &str) -> Result<MapRefMut<'_, String, FileStored>, UploadError> {
        self.map
            .get_mut(id)
            .ok_or_else(|| UploadError::not_exists(id.to_owned()))
    }

    fn random_file(&self, file_name: &str) -> (String, PathBuf) {
        let uuid = Uuid::new_v4().to_string();

        let path = self.mk_file_path(&uuid, file_name);
        (uuid, path)
    }

    async fn load_file_path(
        &self,
        id: &str,
        file: MapRef<'_, String, FileStored>,
    ) -> Result<PathBuf, UploadError> {
        Ok(match &file.source {
            FileSource::Url { url, path } => {
                if let Some(path) = path {
                    path.clone()
                } else {
                    // if file hasn't cached
                    let full_path = self.mk_file_path(id, &file.file_name);
                    let url = url.clone();
                    drop(file);

                    self.download_file(&full_path, &url).await?;
                    // record file path
                    let mut item = self.get_file_mut(id)?;
                    match &mut item.source {
                        FileSource::Url { url: _, path } => {
                            *path = Some(full_path.clone());
                            drop(item);
                        }
                        FileSource::Path(_) => {
                            return Err(UploadError::other(format!(
                                "`{}`: expected `UploadSource::Url`",
                                id
                            )))
                        }
                    }

                    full_path
                }
            }
            FileSource::Path(path) => path.clone(),
        })
    }
}

impl<F: FS> UploadStorage for OBFileStorage<F> {
    async fn upload(
        &self,
        file_name: impl AsRef<str>,
        upload: UploadKind,
    ) -> Result<String, UploadError> {
        let (uuid, full_path) = self.random_file(file_name.as_ref());
        match upload {
            UploadKind::Data(data) => {
                self.fs
                    .write(full_path, 0, &data)
                    .await
                    .map_err(UploadError::other)?;
            }
            UploadKind::Url(url) => {
                if !self.lazy {
                    self.download_file(&full_path, &url).await?;
                }
                self.map.insert(
                    uuid.to_string(),
                    FileStored {
                        source: FileSource::Url {
                            url,
                            path: Some(full_path),
                        },
                        file_name: file_name.as_ref().to_owned(),
                    },
                );
            }
            UploadKind::Path(path) => {
                self.map.insert(
                    uuid.to_string(),
                    FileStored {
                        source: FileSource::Path(path),
                        file_name: file_name.as_ref().to_owned(),
                    },
                );
            }
        };

        Ok(uuid.to_string())
    }

    async fn upload_fragmented(
        &self,
        upload: UploadFileReq,
    ) -> Result<Option<String>, UploadError> {
        match upload {
            UploadFileReq::Prepare {
                name,
                total_size: _,
                ..
            } => {
                let (uuid, full_path) = self.random_file(&name);
                self.frag_map.insert(uuid.to_string(), (name, full_path));
                Ok(Some(uuid.to_string()))
            }
            UploadFileReq::Transfer {
                file_id,
                offset,
                data,
                ..
            } => {
                let Some(res) = self.frag_map.get(&file_id) else {
                    return Err(UploadError::NotExists(file_id));
                };

                self.fs
                    .write(&res.1, offset as u64, &data)
                    .await
                    .map_err(UploadError::other)?;
                Ok(None)
            }
            UploadFileReq::Finish {
                file_id, sha256: _, ..
            } => {
                let Some((_, (file_name, path))) = self.frag_map.remove(&file_id) else {
                    return Err(UploadError::NotExists(file_id));
                };
                let new_uuid = Uuid::new_v4().to_string();
                self.map.insert(
                    new_uuid.clone(),
                    FileStored {
                        source: FileSource::Path(path),
                        file_name,
                    },
                );
                log::warn!("sha256 not implemented");
                Ok(Some(new_uuid))
            }
        }
    }

    async fn get_url(
        &self,
        id: impl AsRef<str>,
    ) -> Result<Option<FileInfo<UrlUpload>>, UploadError> {
        let Some(file) = self.map.get(id.as_ref()) else {
            return Ok(None);
        };
        match &file.source {
            FileSource::Url { url, path: _ } => Ok(Some(FileInfo {
                name: file.file_name.clone(),
                inner: url.clone(),
            })),
            FileSource::Path(_) => Err(UploadError::unsupported("cannot get url from path")),
        }
    }

    async fn get_path(
        &self,
        id: impl AsRef<str>,
    ) -> Result<Option<FileInfo<PathBuf>>, UploadError> {
        let Some(file) = self.map.get(id.as_ref()) else {
            return Ok(None);
        };
        let name = file.file_name.clone();
        let inner = match &file.source {
            FileSource::Url { url, path } => {
                if let Some(path) = path {
                    path.clone()
                } else {
                    let full_path = self.mk_file_path(id.as_ref(), &file.file_name);
                    let url = url.clone();
                    drop(file);
                    self.download_file(&full_path, &url).await?;
                    full_path
                }
            }
            FileSource::Path(path) => path.clone(),
        };
        Ok(Some(FileInfo { name, inner }))
    }

    async fn get_data(
        &self,
        id: impl AsRef<str>,
    ) -> Result<Option<FileInfo<Vec<u8>>>, UploadError> {
        let Some(file) = self.map.get(id.as_ref()) else {
            return Ok(None);
        };
        let name = file.file_name.clone();
        let path = self.load_file_path(id.as_ref(), file).await?;

        Ok(Some(FileInfo {
            name,
            inner: self
                .fs
                .read_to_end(path, 0)
                .await
                .map_err(UploadError::other)?,
        }))
    }

    async fn get_fragmented(
        &self,
        get: GetFileFragmented,
    ) -> Result<Option<GetFileFrag>, UploadError> {
        let Some(file) = self.map.get(&get.file_id) else {
            return Ok(None);
        };
        Ok(Some(match get.req {
            GetFileReq::Prepare { .. } => GetFileFrag::Prepare {
                name: file.file_name.clone(),
                total_size: self
                    .fs
                    .metadata(self.load_file_path(&get.file_id, file).await?)
                    .await
                    .map_err(UploadError::other)?
                    .size() as i64,
                sha256: None,
            },
            GetFileReq::Transfer { offset, size, .. } => GetFileFrag::Transfer {
                data: UploadData(
                    self.fs
                        .read(
                            self.load_file_path(&get.file_id, file).await?,
                            offset as _,
                            size as _,
                        )
                        .await
                        .map_err(UploadError::other)?,
                ),
            },
        }))
    }

    async fn is_cached(&self, id: impl AsRef<str>) -> Result<bool, UploadError> {
        let file = self.get_file(id.as_ref())?;

        match &file.source {
            FileSource::Url { url: _, path } => Ok(path.is_some()),
            FileSource::Path(_) => Ok(true),
        }
    }

    async fn get_store_state(&self, id: impl AsRef<str>) -> Result<StoreState, UploadError> {
        let file = self.get_file(id.as_ref())?;

        match &file.source {
            FileSource::Url { url, path } => {
                if let Some(path) = path {
                    Ok(StoreState::Cached(path.to_string_lossy().into_owned()))
                } else {
                    Ok(StoreState::NotCached(url.clone()))
                }
            }
            FileSource::Path(path) => Ok(StoreState::Cached(path.to_string_lossy().into_owned())),
        }
    }
}
