extern crate base64;
#[macro_use]
extern crate error_chain;
extern crate futures;
extern crate http;
extern crate hyper;
extern crate hyper_rustls;
extern crate percent_encoding;
extern crate terminal_size;
extern crate tokio;

mod errors {
    error_chain! {
        foreign_links {
            HyperError(::hyper::Error);
            ToStrError(::hyper::header::ToStrError);
            TokioIoError(::tokio::io::Error);
        }
    }
}

mod speed;
#[macro_use]
mod util;
mod manager;
mod worker;

use futures::Future;

fn main() {
    let thread_num = 4;
    let block_size = 2048000; // TODO: Make this configurable
    let url = std::env::args().skip(1).take(1).last().unwrap();
    let mut threadpool_builder = tokio::executor::thread_pool::Builder::new();
    threadpool_builder.pool_size(thread_num);
    let mut runtime = tokio::runtime::Builder::new()
        .threadpool_builder(threadpool_builder)
        .build().unwrap();

    println!("=> Retrieving information about url...");
    runtime.block_on(manager::DownloadManager::new(url.parse().unwrap(), block_size)
        .and_then(move |m| {
            println!("=> File name: {}", m.file_name);
            println!("=> File size: {} bytes", m.file_len);
            println!("=> {} bytes block count: {}", block_size, m.block_count);
            m.run(thread_num)
        })
        .map_err(|e| println!("=> fatal error: {:?}", e))).unwrap();
}
