#![allow(unused)]
use clap::{Clap, ValueHint};
use console::{style, Emoji, Style, Term};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{HeaderName, HeaderValue};
use shirodl::{DownloadFailed, Downloader};
use std::io;
use std::io::Read;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

#[derive(Clap)]
#[clap(
    name = "ShiroDL",
    version = "0.1.0",
    author = "Shiroko <hhx.xxm@gmail.com>"
)]
struct Opts {
    #[clap(short, long, value_hint=ValueHint::FilePath, validator(is_existed_as_file))]
    input: Option<PathBuf>,
    #[clap(short, long)]
    header: Option<String>,
    #[clap(short, long, value_hint=ValueHint::DirPath)]
    destination: Option<PathBuf>,
    #[clap(long)]
    no_hash: bool,
    #[clap(short, long)]
    timeout: Option<u64>,
}

fn main() {
    let opts = Opts::parse();
    println!("ShiroDL Program Version 0.1.0 <hhx.xxm@gmail.com>");
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
    let inputs = inputs.lines().collect::<Vec<&str>>();
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
    // filtering url
    let inputs = inputs
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
        .collect::<Vec<_>>();
    inputs.iter().for_each(|s| {
        // todo: Running path generator
        downloader.append_task(s.to_string(), PathBuf::from("."), None);
    });
    let bar = ProgressBar::new(inputs.len() as u64);
    bar.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner}[{elapsed_precise}][{eta}] {wide_bar:.cyan/blue} [{pos}/{len} - {percent}%] {msg}",
            )
            .progress_chars("##-"),
    );
    let (sender, receiver) = mpsc::channel();
    let retain_sender = sender.clone();
    bar.enable_steady_tick(200);
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
    let failed = downloader.download(move |url, path, filename, err| {
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
                Emoji::new("✔️", "Done")
            } else {
                Emoji::new("❌️", "Failed")
            },
            if err.is_none() {
                url.to_string()
            } else {
                format!("{} [{}]", url, err.unwrap())
            }
        );
        sender.send(Some(msg_style.apply_to(msg).to_string()));
    });
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
    /*
    for f in failed_unignorable {
        println!(
            "[FAILED][{:?}] {} -> {}",
            f.err,
            f.url,
            f.path.to_str().unwrap_or("#CannotBeFormatted#")
        );
    }*/
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
