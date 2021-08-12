#![allow(dead_code, unused)]

#[cfg(test)]
#[allow(dead_code, unused)]
mod tests {
    use crate::shirodl::*;
    use reqwest::Url;
    use std::path::{Path, PathBuf};

    #[test]
    fn test() {
        println!("Starting test...");
        let mut dler = Downloader::new();
        dler.set_destination(PathBuf::from("."));
        dler.set_hash_check(true);
        dler.append_task(
            "https://pbs.twimg.com/media/E8SpfWfUUAMrV3r.jpg?name=orig".to_string(),
            PathBuf::from("."),
            None,
        );
        let result = dler.download();
        for r in result {
            println!("Failed: {}, due to {:?}", r.url, r.err);
        }
    }
}

mod shirodl {
    use crate::shirodl::Error::HashingError;
    use blake3::{hash, Hash, Hasher};
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue, IntoHeaderName};
    use reqwest::{Client, Error as HttpError, Url};
    use std::borrow::Borrow;
    use std::convert::TryFrom;
    use std::future::Future;
    use std::io::Error as IoError;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;
    use std::{fs, path};
    use tokio::sync::Semaphore;
    use tokio::task::JoinHandle;
    use tokio::{spawn, task};

    struct DownloadParams {
        url: String,
        path: PathBuf, // relative to folder of Downloader
        filename: Option<String>,
    }

    pub struct Downloader {
        list: Vec<DownloadParams>,
        folder: PathBuf,
        timeout: Option<Duration>,
        headers: HeaderMap,
        hash_check: bool,
        progressbar: bool,
    }

    pub struct DownloadFailed {
        pub url: String,
        pub path: PathBuf,
        pub filename: Option<String>,
        pub err: Error,
    }

    #[derive(Debug)]
    pub enum Error {
        FileExisted,
        DifferentFileExisted,
        FileExistedAsFolder,
        NoPermissionToWrite,
        FailedToCreateFolder,
        FolderExistedAsFile,
        ResourceNotFound,
        HttpError(HttpError),
        UrlIllegal,
        UrlCannotDownload,
        RequestNotOK(u16),
        IoError(String),
        HashingError,
    }

    impl From<HttpError> for Error {
        fn from(err: HttpError) -> Self {
            Self::HttpError(err)
        }
    }

    impl Downloader {
        async fn dl_worker(
            client: &reqwest::Client,
            url: &str,
            path: &PathBuf,
            filename: &Option<String>,
            hash_check: bool,
        ) -> Result<(), Error> {
            let url = Url::parse(&url).map_err(|_| Error::UrlIllegal)?;

            let filename = if let Some(filename) = filename {
                filename.to_string()
            } else {
                match url.path_segments() {
                    Some(l) => l.last().unwrap_or(""),
                    None => "",
                }
                .to_string()
            };
            if filename == "" {
                return Err(Error::UrlCannotDownload);
            }
            let filepath = &path.join(&filename);
            let path_metadata = fs::metadata(&path);
            let existed_hash = match path_metadata {
                Ok(metadata) => {
                    if metadata.is_dir() {
                        let file_metadata = fs::metadata(filepath);
                        match file_metadata {
                            Ok(file_metadata) => {
                                if file_metadata.is_dir() {
                                    return Err(Error::FileExistedAsFolder);
                                } else {
                                    if hash_check {
                                        let mut hasher = Hasher::new();
                                        std::io::copy(
                                            &mut fs::File::open(filepath)
                                                .map_err(|e| Error::IoError(e.to_string()))?,
                                            &mut hasher,
                                        )
                                        .map_err(|_| Error::HashingError);
                                        Some(hasher.finalize())
                                    } else {
                                        return Err(Error::FileExisted);
                                    }
                                }
                            }
                            Err(_) => None,
                        }
                    } else {
                        return Err(Error::FolderExistedAsFile);
                    }
                }
                Err(_) => {
                    fs::create_dir_all(&path).map_err(|_| Error::FailedToCreateFolder)?;
                    None
                }
            };
            let req = client.get(url).build()?;
            let content = client.execute(req).await?;
            if content.status() != 200 {
                if content.status() == 404 {
                    Err(Error::ResourceNotFound)
                } else {
                    Err(Error::RequestNotOK(content.status().as_u16()))
                }
            } else {
                let content = content.bytes().await?;
                if let Some(existed_hash) = existed_hash {
                    let mut hasher = Hasher::new();
                    std::io::copy(&mut content.as_ref(), &mut hasher)
                        .map_err(|_| Error::HashingError);
                    if hasher.finalize() == existed_hash {
                        Ok(())
                    } else {
                        Err(Error::DifferentFileExisted)
                    }
                } else {
                    std::io::copy(
                        &mut content.as_ref(),
                        &mut fs::File::create(filepath)
                            .map_err(|e| Error::IoError(e.to_string()))?,
                    )
                    .map_err(|e| Error::IoError(e.to_string()))?;
                    Ok(())
                }
            }
        }

        pub fn new() -> Self {
            Self {
                list: vec![],
                folder: Default::default(),
                timeout: None,
                headers: HeaderMap::new(),
                hash_check: false,
                progressbar: true,
            }
        }

        pub fn set_destination(&mut self, download_folder: PathBuf) {
            self.folder = download_folder;
        }

        pub fn set_timeout(&mut self, timeout: Duration) {
            self.timeout = Some(timeout);
        }

        pub fn set_hash_check(&mut self, hash_check: bool) {
            self.hash_check = hash_check;
        }

        // path is relative to Downloader global folder
        pub fn append_task(&mut self, url: String, path: PathBuf, filename: Option<String>) {
            if self
                .list
                .iter()
                .any(|x| x.url == url && x.path == path && x.filename == filename)
            {
                return;
            }
            self.list.push(DownloadParams {
                url,
                path,
                filename,
            });
        }

        pub fn add_header<K, V>(&mut self, key: K, value: V)
        where
            K: IntoHeaderName,
            HeaderValue: From<V>,
        {
            self.headers.append(key, value.into());
        }

        pub fn download(self) -> Vec<DownloadFailed> {
            let client = Client::builder().default_headers(self.headers);
            let client = if let Some(timeout) = self.timeout {
                client.timeout(timeout)
            } else {
                client
            }
            .build();
            let client = if let Err(e) = client {
                panic!("Cannot build HTTP Client, {}", e.to_string());
            } else {
                client.unwrap()
            };
            let hash_check = self.hash_check;
            let limits = Arc::new(Semaphore::new(10)); // limit the tasks

            let mut rt = tokio::runtime::Runtime::new().unwrap();
            let jobs: Vec<_> = self
                .list
                .into_iter()
                .map(|t| {
                    let client = client.clone();
                    let hash_check = hash_check.clone();
                    let permit = Arc::clone(&limits).acquire_owned();
                    rt.spawn(async move {
                        permit.await;
                        let result =
                            Self::dl_worker(&client, &t.url, &t.path, &t.filename, hash_check)
                                .await;
                        if let Err(e) = result {
                            Some(DownloadFailed {
                                url: t.url,
                                path: t.path,
                                filename: t.filename,
                                err: e,
                            })
                        } else {
                            None
                        }
                    })
                })
                .collect();
            let mut result = vec![];
            let downloader = async {
                for job in jobs {
                    let res = job.await.unwrap();
                    if let Some(res) = res {
                        result.push(res)
                    }
                }
            };
            rt.block_on(downloader);
            result
        }
    }
}
