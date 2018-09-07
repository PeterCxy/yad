use errors::*;
use futures::Future;
use hyper::{Body, Request, Uri};
use hyper::header;
use util::*;

#[derive(Debug)]
pub struct DownloadManager {
    url: Uri,
    pub file_len: usize,
}

impl DownloadManager {
    pub fn new(url: Uri) -> impl Future<Item = DownloadManager, Error = Error> {
        hyper_client().request(Request::head(url.clone()).body(Body::empty()).unwrap())
            .chain_err(|| "aaa")
            .and_then(|r| {
                r.headers().get(header::CONTENT_LENGTH)
                    .ok_or("Failed to request for content length".into())
                    .and_then(|l| l.to_str()
                        .chain_err(|| "Failed to get content length"))
                    .and_then(|l| l.parse()
                        .chain_err(|| "Failed to parse content length"))
            })
            .map(|len| {
                DownloadManager {
                    url: url,
                    file_len: len
                }
            })
    }
}