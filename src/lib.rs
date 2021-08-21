#[cfg(test)]
#[allow(dead_code, unused)]
mod tests {
    use crate::Downloader;
    use reqwest::Url;
    use std::path::{Path, PathBuf};

    #[test]
    fn test() {
        println!("Starting test...");
        let mut dler = Downloader::new();
        dler.set_destination(PathBuf::from("."));
        dler.set_hash_check(true);
        dler.append_task(
            "https://avatars.githubusercontent.com/u/6939913?s=48&v=4".to_string(),
            PathBuf::from("."),
            None,
        );
        let result = dler.download(|_, _, _, _| {}).unwrap();
        for r in result {
            println!("Failed: {}, due to {:?}", r.url, r.err);
        }
    }
}

use blake3::Hasher;
// use content_inspector;
use reqwest::header::{HeaderMap, HeaderValue, IntoHeaderName};
use reqwest::{Client, Error as HttpError, Proxy, Url};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;

struct DownloadParams {
    url: String,
    path: PathBuf, // relative to folder of Downloader
    filename: Option<String>,
}

pub enum ProxyType {
    Http,
    Https,
    All,
}

pub struct Downloader {
    list: Vec<DownloadParams>,
    folder: PathBuf,
    timeout: Option<Duration>,
    headers: HeaderMap,
    hash_check: bool,
    only_binary: bool,
    auto_rename: bool,
    proxies: Vec<Proxy>,
    task_count: usize,
    disable_default_proxy: bool,
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
    DifferentFileExistedWhenRename,
    FileExistedAsFolder,
    FileExistedAsFolderWhenRename,
    NoPermissionToWrite,
    FailedToCreateFolder,
    FolderExistedAsFile,
    FileIsNotBinary,
    ResourceNotFound,
    HttpError(HttpError),
    UrlIllegal,
    UrlCannotDownload,
    RequestNotOK(u16),
    IoError(String),
    IoErrorWhenRename(String),
    HashingError,
    HashingErrorWhenRename,
    ProxyError(String),
}

impl Error {
    pub fn ignorable(&self) -> bool {
        match self {
            Self::DifferentFileExistedWhenRename => true,
            Self::FileExistedAsFolderWhenRename => true,
            Self::HashingErrorWhenRename => true,
            Self::IoErrorWhenRename(_) => true,
            Self::UrlCannotDownload => true,
            Self::FileIsNotBinary => true,
            _ => false,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::FileExisted => write!(f, "File Existed"),
            Error::DifferentFileExisted => write!(f, "Different File Existed"),
            Error::DifferentFileExistedWhenRename => {
                write!(f, "Different File Existed When Rename")
            }
            Error::FileExistedAsFolder => write!(f, "File Existed As Folder"),
            Error::FileExistedAsFolderWhenRename => write!(f, "File Existed As Folder When Rename"),
            Error::NoPermissionToWrite => write!(f, "No Permission To Write"),
            Error::FailedToCreateFolder => write!(f, "Failed To Create Folder"),
            Error::FolderExistedAsFile => write!(f, "Folder Existed As File"),
            Error::FileIsNotBinary => write!(f, "File Is Not Binary"),
            Error::ResourceNotFound => write!(f, "404 Resource Not Found"),
            Error::HttpError(http) => write!(f, "HTTP Error: {}", http.to_string()),
            Error::UrlIllegal => write!(f, "Url Illegal"),
            Error::UrlCannotDownload => write!(f, "Url Cannot Be Downloaded"),
            Error::RequestNotOK(status_code) => {
                write!(f, "Request Not OK with Code: {}", status_code)
            }
            Error::IoError(e) => write!(f, "Io Error: {}", e),
            Error::IoErrorWhenRename(e) => write!(f, "Io Error When Rename: {}", e),
            Error::HashingError => write!(f, "Hashing Error"),
            Error::HashingErrorWhenRename => write!(f, "Hashing Error When Rename"),
            Error::ProxyError(e) => write!(f, "Proxy Error: {}", e),
        }
    }
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
        only_binary: bool,
        auto_rename: bool,
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
        let path = path;
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
                                    .map_err(|_| Error::HashingError)?;
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
                fs::create_dir_all(&path).map_err(|e| {
                    println!("{}: {}", path.to_str().unwrap_or(""), e.to_string());
                    Error::FailedToCreateFolder
                })?;
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
            let content_header = content.headers().clone();
            let content = content.bytes().await?;
            // check type
            if only_binary {
                if let Some(content_type) = content_header.get("content-type") {
                    if content_type == "application/javascript"
                        || content_type.to_str().unwrap().contains("text/html")
                    {
                        return Err(Error::FileIsNotBinary);
                    }
                }
            }
            // check hash
            if let Some(existed_hash) = existed_hash {
                let mut hasher = Hasher::new();
                std::io::copy(&mut content.as_ref(), &mut hasher)
                    .map_err(|_| Error::HashingError)?;
                if hasher.finalize() == existed_hash {
                    Ok(())
                } else {
                    Err(Error::DifferentFileExisted)
                }
            } else {
                std::io::copy(
                    &mut content.as_ref(),
                    &mut fs::File::create(filepath).map_err(|e| Error::IoError(e.to_string()))?,
                )
                .map_err(|e| Error::IoError(e.to_string()))?;
                if auto_rename {
                    // rename file without extension via using mime types
                    if !filepath
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .contains('.')
                    {
                        let ext = content_header
                            .get("content-type")
                            .map_or("", |h| h.to_str().unwrap_or(""));
                        let ext = ext.split('/').last().unwrap().split(';').next().unwrap();
                        let mut new_file_path = filepath.clone();
                        new_file_path.set_extension(ext);
                        if new_file_path.exists() {
                            if new_file_path.is_dir() {
                                // give up
                                return Err(Error::FileExistedAsFolderWhenRename);
                            }
                            // using hash to check
                            let mut hasher = Hasher::new();
                            std::io::copy(&mut content.as_ref(), &mut hasher)
                                .map_err(|_| Error::HashingErrorWhenRename)?;
                            let old_hash = hasher.finalize();
                            hasher.reset();
                            std::io::copy(
                                &mut fs::File::open(new_file_path)
                                    .map_err(|e| Error::IoErrorWhenRename(e.to_string()))?,
                                &mut hasher,
                            )
                            .map_err(|_| Error::HashingErrorWhenRename)?;
                            let new_hash = hasher.finalize();
                            if old_hash != new_hash {
                                return Err(Error::DifferentFileExistedWhenRename);
                            }
                        } else {
                            fs::copy(filepath, new_file_path)
                                .map_err(|e| Error::IoErrorWhenRename(e.to_string()))?;
                        }
                        fs::remove_file(filepath)
                            .map_err(|e| Error::IoErrorWhenRename(e.to_string()))?;
                    }
                }
                Ok(())
            }
        }
    }

    pub fn new() -> Self {
        Self {
            list: vec![],
            folder: Default::default(),
            timeout: Some(Duration::from_secs(10)),
            headers: HeaderMap::new(),
            hash_check: false,
            only_binary: true,
            auto_rename: true,
            proxies: Vec::new(),
            disable_default_proxy: false,
            task_count: 8,
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

    pub fn set_binary_only(&mut self, only_binary: bool) {
        self.only_binary = only_binary;
    }

    pub fn set_auto_rename(&mut self, auto_rename: bool) {
        self.auto_rename = auto_rename;
    }

    pub fn add_proxy(&mut self, proxy_type: ProxyType, proxy: String) -> Result<(), Error> {
        let proxy = match proxy_type {
            ProxyType::Http => Proxy::http(proxy),
            ProxyType::Https => Proxy::https(proxy),
            ProxyType::All => Proxy::all(proxy),
        }
        .map_err(|e| Error::ProxyError(e.to_string()))?;
        self.proxies.push(proxy);
        Ok(())
    }

    pub fn disable_default_proxy(&mut self) {
        self.disable_default_proxy = true;
    }

    pub fn enable_default_proxy(&mut self) {
        self.disable_default_proxy = false;
    }

    pub fn set_task_count(&mut self, task_count: usize) {
        self.task_count = task_count;
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

    pub fn download<F: 'static>(self, callback: F) -> Result<Vec<DownloadFailed>, Error>
    where
        F: Fn(&str, &PathBuf, &Option<String>, Option<&Error>) + std::marker::Send,
    {
        let client = Client::builder().default_headers(self.headers);
        let client = if let Some(timeout) = self.timeout {
            client.timeout(timeout)
        } else {
            client
        };
        let client = self
            .proxies
            .into_iter()
            .fold(client, |client, proxy| client.proxy(proxy));
        let client = if self.disable_default_proxy {
            client.no_proxy()
        } else {
            client
        };
        let client = client.build().map_err(|e| Error::HttpError(e))?;
        let hash_check = self.hash_check;
        let only_binary = self.only_binary;
        let auto_rename = self.auto_rename;
        let workdir = self.folder;
        if !workdir.exists() {
            fs::create_dir_all(&workdir).map_err(|_| Error::FailedToCreateFolder)?;
        } else if workdir.is_file() {
            return Err(Error::FolderExistedAsFile);
        }
        let limits = Arc::new(Semaphore::new(self.task_count)); // limit the tasks
        let callback = Arc::new(Mutex::new(callback));

        let rt = tokio::runtime::Runtime::new().unwrap();
        let jobs: Vec<_> = self
            .list
            .into_iter()
            .map(|t| {
                let permit = Arc::clone(&limits).acquire_owned();
                let client = client.clone();
                let hash_check = hash_check.clone();
                let only_binary = only_binary.clone();
                let auto_rename = auto_rename.clone();
                let workdir = workdir.clone();
                let path = workdir.join(&t.path);
                let callback = Arc::clone(&callback);
                rt.spawn(async move {
                    let permit = permit.await.unwrap(); // for limiting tasks
                    let result = Self::dl_worker(
                        &client,
                        &t.url,
                        &path,
                        &t.filename,
                        hash_check,
                        only_binary,
                        auto_rename,
                    )
                    .await;
                    let callback = &*callback.lock().unwrap();
                    let r = if let Err(e) = result {
                        callback(&t.url, &path, &t.filename, Some(&e));
                        Some(DownloadFailed {
                            url: t.url,
                            path,
                            filename: t.filename,
                            err: e,
                        })
                    } else {
                        callback(&t.url, &path, &t.filename, None);
                        None
                    };
                    drop(permit);
                    r
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
        Ok(result)
    }
}
