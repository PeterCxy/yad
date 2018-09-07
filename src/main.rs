#[macro_use]
extern crate error_chain;
extern crate futures;
extern crate hyper;
extern crate hyper_rustls;
extern crate tokio;

mod errors {
    error_chain! {
        foreign_links {
            HyperError(::hyper::Error);
            ToStrError(::hyper::header::ToStrError);
        }
    }
}

mod util;
mod manager;

use futures::Future;

fn main() {
    let block_size = 512; // TODO: Make this configurable
    let url = std::env::args().skip(1).take(1).last().unwrap();
    println!("=> Retrieving information about url...");
    tokio::run(manager::DownloadManager::new(url.parse().unwrap(), block_size)
        .and_then(move |m| {
            println!("=> File name: {}", m.file_name);
            println!("=> File size: {} bytes", m.file_len);
            println!("=> {} bytes block count: {}", block_size, m.block_count);
            Ok(())
        })
        .map_err(|e| println!("=> fatal error: {:?}", e)));
}
