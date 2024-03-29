#![allow(unused)]
use clap::{Parser, ValueHint};
use console::{style, Emoji, Style, Term};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use shirodl::{DownloadFailed, Downloader, ProxyType};
use std::fmt::Write;
use std::io;
use std::io::Read;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

#[derive(Parser)]
#[clap(name = "ShiroDL", version = env!("CARGO_PKG_VERSION"), author = "Shiroko <hhx.xxm@gmail.com>")]
struct Opts {
    #[clap(short, long, value_hint=ValueHint::FilePath, validator(is_existed_as_file), help="List of urls.")]
    input: Option<PathBuf>,
    #[clap(short, long, help = "Set headers, usage: --header a=b,c=d")]
    header: Option<String>,
    #[clap(short, long, value_hint=ValueHint::DirPath, help="Download destination folder.")]
    destination: Option<PathBuf>,
    #[clap(
        long,
        help = "No hash check when file already existed. Not affect hashing when auto rename."
    )]
    no_hash: bool,
    #[clap(short, long, help = "Timeout in microsecond.")]
    timeout: Option<u64>,
    #[clap(
        short,
        long,
        help = "Set Proxy, `no` means not set proxy from environment variables."
    )]
    proxy: Option<String>,
    #[clap(short, long, help = "Async task count.", default_value = "8")]
    jobs: usize,
    #[clap(
        short,
        long,
        help = "Use json format as input. field: `url`, `filename`, `folder`."
    )]
    json: bool,
    #[clap(short, long, help = "Maxium Retry times", default_value = "3")]
    retry: usize,
    #[clap(
        short,
        long,
        help = "Save Unignorable failed tasks to json format file."
    )]
    save_failed: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
struct DownloadTask {
    pub url: String,
    pub filename: Option<String>,
    pub folder: Option<PathBuf>,
}

#[derive(Serialize)]
struct DownloadFailedSerializable {
    pub url: String,
    pub filename: Option<String>,
    pub folder: Option<PathBuf>,
    pub error: String,
}

fn main() {
    let opts = Opts::parse();
    println!(
        "ShiroDL Program Version {} By Shiroko <hhx.xxm@gmail.com>",
        env!("CARGO_PKG_VERSION")
    );
    let mut downloader = Downloader::new();
    // parse header
    if let Some(header) = opts.header {
        header.split(',').for_each(|s| {
            if let Some((k, v)) = s.split_once('=') {
                let header_key: HeaderName = HeaderName::from_bytes(k.as_bytes()).unwrap();
                let header_value: HeaderValue = HeaderValue::from_bytes(v.as_bytes()).unwrap();
                println!(
                    "Inserted Header: {} = {}",
                    header_key,
                    header_value.to_str().unwrap_or("#CannotBeFormatted#")
                );
                downloader.add_header(header_key, header_value);
            }
        });
    }
    // get inputs
    let inputs = if let Some(input) = opts.input {
        // read from file
        let mut buffer = String::new();
        std::fs::File::open(input)
            .unwrap()
            .read_to_string(&mut buffer)
            .unwrap();
        buffer
    } else {
        // read from stdin
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer).unwrap();
        buffer
    };
    let tasks = if opts.json {
        serde_json::from_str(inputs.as_str()).unwrap()
    } else {
        let inputs = inputs.lines().collect::<Vec<&str>>();
        // filtering url
        inputs
            .iter()
            .filter(|l| {
                let url = if let Ok(url) = reqwest::Url::parse(l) {
                    url
                } else {
                    return false;
                };
                if url.scheme() == "http" || url.scheme() == "https" {
                    true
                } else {
                    false
                }
            })
            .map(|v| DownloadTask {
                url: v.to_string(),
                folder: None,
                filename: None,
            })
            .collect::<Vec<_>>()
    };

    if let Some(dest) = opts.destination {
        let dest = std::env::current_dir().unwrap().join(dest);
        downloader.set_destination(dest);
    } else {
        downloader.set_destination(std::env::current_dir().unwrap());
    }
    if let Some(timeout) = opts.timeout {
        downloader.set_timeout(Duration::from_micros(timeout));
    }
    downloader.set_hash_check(!opts.no_hash);
    downloader.set_task_count(opts.jobs);
    downloader.set_retries_count(opts.retry);

    if let Some(proxy) = opts.proxy {
        if proxy.to_lowercase() != "no" {
            println!("Set Proxy {} for all.", proxy);
            downloader.add_proxy(ProxyType::All, proxy).unwrap();
        } else {
            downloader.disable_default_proxy();
        }
    }

    tasks.iter().for_each(|v| {
        // todo: Running path generator
        downloader.append_task((
            v.url.clone(),
            v.folder.clone().unwrap_or(".".to_string().into()),
            v.filename.clone(),
        ))
    });

    let bar = ProgressBar::new(tasks.len() as u64);
    bar.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner}[{elapsed_precise}][{eta}] {wide_bar:.cyan/blue} [{pos}/{len} - {percent}%] {msg}",
            ).unwrap()
            .progress_chars("##-"),
    );
    let (sender, receiver) = mpsc::channel();
    let retain_sender = sender.clone();
    bar.enable_steady_tick(Duration::from_millis(200));
    let display_thread = thread::spawn(move || loop {
        let msg = receiver.recv().unwrap();
        if let Some(s) = msg {
            bar.println(s);
            bar.inc(1);
        } else {
            bar.finish();
            break;
        }
    });
    let failed = downloader
        .download(move |url, path, filename, err| {
            let msg_style = if let Some(e) = err {
                if e.ignorable() {
                    Style::new().black().bright()
                } else {
                    Style::new().red().bright().bold()
                }
            } else {
                Style::new().green()
            };
            let msg = format!(
                "{} {}",
                if err.is_none() {
                    Emoji::new("✔️", "[ Done ]")
                } else {
                    Emoji::new("❌️", "[Failed]")
                },
                if err.is_none() {
                    url.to_string()
                } else {
                    format!("{} [{}]", url, err.unwrap())
                }
            );
            sender.send(Some(msg_style.apply_to(msg).to_string()));
        })
        .unwrap();
    retain_sender.send(None);
    display_thread.join().unwrap();
    let failed_unignorable: Vec<_> = failed.iter().filter(|v| !v.err.ignorable()).collect();
    println!("Download Complete!");
    if failed.len() != 0 {
        println!(
            "{} Failed, {} Ignorable, {} Unignorable.",
            failed.len(),
            failed.len() - failed_unignorable.len(),
            failed_unignorable.len()
        );
    }
    if let Some(save_failed) = opts.save_failed {
        let failed = failed_unignorable
            .iter()
            .map(|v| DownloadFailedSerializable {
                url: v.url.clone(),
                filename: v.filename.clone(),
                folder: Some(v.path.clone()),
                error: v.err.to_string(),
            })
            .collect::<Vec<DownloadFailedSerializable>>();
        std::fs::write(save_failed, serde_json::to_string_pretty(&failed).unwrap()).unwrap();
    }
}

fn is_existed_as_file(v: &str) -> Result<(), String> {
    let p = Path::new(v);
    if p.exists() && p.is_file() {
        Ok(())
    } else {
        if p.exists() {
            Err("Not a file.".to_string())
        } else {
            Err("Not Exists".to_string())
        }
    }
}
