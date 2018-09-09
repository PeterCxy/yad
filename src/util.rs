use errors::*;
use futures::{Future, Poll};
use hyper::{client, Client};
use hyper::body::Payload;
use hyper_rustls::HttpsConnector;
use std::error;

pub fn hyper_client<B: Payload>() -> Client<HttpsConnector<client::HttpConnector>, B> {
    client::Builder::default()
        .keep_alive(true)
        .build(HttpsConnector::new(4))
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

