use base64;
use errors::*;
use futures::{Async, Future, Poll};
use futures::future::poll_fn;
use http::request::Builder as ReqBuilder;
use hyper::{Body, client, Client, header, Uri, Response};
use hyper::body::Payload;
use hyper_rustls::HttpsConnector;
use std::error;
use std::time::Duration;
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

// Extract the authentication part of the URL and generate
// a string that can be directly plugged into the Authorization header
pub fn parse_url_basic_auth(url: &Uri) -> Option<String> {
    url.authority_part()
        .map(|a| a.as_str())
        .and_then(|a| a.find("@").map(move |index| (a, index)))
        .map(|(a, index)| base64::encode(&a[0..index]))
        .map(|result| format!("Basic {}", result))
}

pub trait ReqBuilderExt {
    fn add_auth_header(&mut self, auth_header: &Option<String>) -> &mut Self;
}

impl ReqBuilderExt for ReqBuilder {
    fn add_auth_header(&mut self, auth_header: &Option<String>) -> &mut ReqBuilder {
        if let &Some(ref header) = auth_header {
            self.header(header::AUTHORIZATION, header.to_owned())
        } else {
            self
        }
    }
}

// Parse the Content-Disposition header for the file name
pub fn parse_content_disposition(resp: &Response<Body>) -> Option<String> {
    resp.headers().get(header::CONTENT_DISPOSITION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            let arr: Vec<_> = h.split(";").map(|s| s.trim()).collect();
            if arr.len() < 2 || arr[0] != "attachment" || !arr[1].starts_with("filename=\"") {
                None
            } else {
                Some(arr[1][10..arr[1].len() - 1].to_owned())
            }
        })
}

pub fn build_human_readable_speed(duration: Duration, delta: u64) -> (f64, String) {
    let duration = duration.as_secs() as f64 + duration.subsec_nanos() as f64 / 1000f64 / 1000f64 / 1000f64;
    let speed = delta as f64 / duration;

    let human_readable = if speed >= 1024f64 * 1024f64 {
        format!("{:.1} MiB/s", speed / 1024f64 / 1024f64)
    } else if speed >= 1024f64 {
        format!("{:.1} KiB/s", speed / 1024f64)
    } else {
        format!("{:.1} B/s", speed)
    };

    (speed, human_readable)
}

pub fn build_human_readable_eta(speed: f64, remaining: u64) -> String {
    let mut eta = (remaining as f64 / speed) as u64;
    let mut ret = String::new();

    if eta >= 3600 {
        let hr = eta / 3600;
        eta -= hr * 3600;
        ret += &format!("{:02}h", hr);
    }

    if eta >= 60 {
        let min = eta / 60;
        eta -= min * 60;
        ret += &format!("{:02}m", min);
    }

    ret += &format!("{:02}s", eta);

    return ret;
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

