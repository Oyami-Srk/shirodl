# shirodl
[![Crates.io](https://img.shields.io/crates/v/shirodl.svg)](https://crates.io/crates/shirodl)

ShiroDL is an Async Download Library for Massive Batch of Urls Downloading

## Install
`cargo install --example shirodl --git https://github.com/Oyami-Srk/shirodl`

## Usage
`shirodl --help` for usage helps.

## Library

#### example
```rust
use shirodl::Downloader;
use std::path::PathBuf;

fn main() {
    let mut dler = Downloader::new();
    dler.set_destination(PathBuf::from("."));
    dler.set_hash_check(true);
    dler.append_task(
        "https://avatars.githubusercontent.com/u/6939913?s=48&v=4".to_string(),
        PathBuf::from("."),
        None,
    );
    let result = dler.download(|_, _, _, _| {});
    for r in result {
        println!("Failed: {}, due to {:?}", r.url, r.err);
    }
}
```

Downloader::download will create `Tokio` runtime, so you can call it directly in normal sync code.
Default Download parameters:
```rust
folder: Default::default(),
timeout: Some(Duration::from_secs(10)),
headers: HeaderMap::new(),
hash_check: false,
only_binary: true,
auto_rename: true,
```

Notice that currently auto-rename only determinates extensions simply by MIME via http header `content-type` and extracts `subtype` as extension.
Manually check is required but at least it could give you a tip.

`shirodl::Error::ignorable()` is not always ignorable, present errors to users always and leave the decisions to end user.

# Thanks
This Project is Developmented under wonderful JetBrains IDE.
[![CLion](./resource/icon_CLion.png)](https://www.jetbrains.com/?from=OmochaOS)
