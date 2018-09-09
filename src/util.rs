use errors::*;
use futures::{Async, Future, Poll};
use futures::future::poll_fn;
use hyper::{client, Client};
use hyper::body::Payload;
use hyper_rustls::HttpsConnector;
use std::error;
use tokio::fs;

pub fn hyper_client<B: Payload>() -> Client<HttpsConnector<client::HttpConnector>, B> {
    client::Builder::default()
        .keep_alive(true)
        .build(HttpsConnector::new(4))
}

// Create a file with length
// TODO: create all the paths leading to the file
pub fn create_file_with_len(name: &str, len: u64) -> impl Future<Item = fs::File, Error = Error> {
    use std::mem::replace;
    fs::File::create(name.to_owned())
        .chain_err(|| "Failed to create file")
        .and_then(move |file| {
            // Set the file length by using poll_fn
            // since tokio_fs didn't implement this for us
            // Wrap the file in an Option so that we can
            // move out of it when we finish polling
            let mut file = Some(file);
            poll_fn(move || {
                let result = file.as_mut().unwrap().poll_set_len(len);
                match result {
                    Err(e) => Err(e),
                    Ok(result) => match result {
                        Async::Ready(_) => Ok(Async::Ready(replace(&mut file, None).unwrap())),
                        Async::NotReady => Ok(Async::NotReady)
                    }
                }
            })
            .chain_err(|| "Failed to allocate file")
        })
}

macro_rules! clone {
    /*
     * Simulate a closure that clones
     * some environment variables and
     * take ownership of them by default.
     */
    ($($n:ident),+; || $body:block) => (
        {
            $( let $n = $n.clone(); )+
            move || { $body }
        }
    );
    ($($n:ident),+; |$($p:pat),+| $body:block) => (
        {
            $( let $n = $n.clone(); )+
            move |$($p),+| { $body }
        }
    );

}

// Glue code to make error-chain work with futures
// Source: <https://github.com/alexcrichton/sccache/blob/master/src/errors.rs>
// Modified to avoid static lifetimes and heap allocation
pub trait FutureChainErr<'a, F: Future, T>
    where F: Future + 'a,
          F::Error: error::Error + Send + 'static {
    fn chain_err<CB, E>(self, callback: CB) -> ChainErr<F, CB, E>
        where CB: FnOnce() -> E + Clone + 'a,
              E: Into<ErrorKind>;
}

impl<'a, F> FutureChainErr<'a, F, F::Item> for F
    where F: Future + 'a,
          F::Error: error::Error + Send + 'static,
{
    fn chain_err<C, E>(self, callback: C) -> ChainErr<F, C, E>
        where C: FnOnce() -> E + Clone + 'a,
              E: Into<ErrorKind>,
    {
        ChainErr::new(self, callback)
    }
}

pub struct ChainErr<F: Future, CB, EK: Into<ErrorKind>>
    where F::Error: error::Error + Send + 'static,
          CB: FnOnce() -> EK + Clone {
    future: F,
    callback: CB
}

impl<F: Future, CB, EK: Into<ErrorKind>> ChainErr<F, CB, EK>
    where F::Error: error::Error + Send + 'static,
          CB: FnOnce() -> EK + Clone {

    fn new(future: F, callback: CB) -> ChainErr<F, CB, EK> {
        ChainErr {
            future,
            callback: callback
        }
    }

}

impl<F: Future, CB, EK: Into<ErrorKind>> Future for ChainErr<F, CB, EK>
    where F::Error: error::Error + Send + 'static,
          CB: FnOnce() -> EK + Clone {
    type Error = Error;
    type Item = F::Item;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let p = self.future.poll();
        p.chain_err(self.callback.clone())
    }
}

