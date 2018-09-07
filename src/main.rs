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
    tokio::run(manager::DownloadManager::new(
        std::env::args().skip(1).take(1).last().unwrap().parse().unwrap())
            .and_then(|m| {
                println!("{}", m.file_len);
                Ok(())
            })
            .map_err(|_| ()))
}
