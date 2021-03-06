use bytes::{Bytes, BytesMut};
use errors::*;
use futures::{Future, Stream, Sink};
use futures::future::{self, Either};
use futures::sync::mpsc;
use hyper::{self, Body, client, Client, header, Request, StatusCode, Uri};
use hyper_rustls::HttpsConnector;
use manager::WorkerMessage;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio;
use util::*;

#[derive(Debug)]
pub enum WorkerError {
    UnexpectedResponse(StatusCode),
    ConnectionError(hyper::Error)
}

pub struct DownloadWorker {
    id: usize,
    url: Uri,
    client: Client<HttpsConnector<client::HttpConnector>, Body>,
    block_size: u64,
    file_len: u64,
    delta: Arc<AtomicU64>,
    cur_block_delta: Arc<AtomicU64>,
    auth_header: Option<String>,
    recv_chan: Option<mpsc::Receiver<WorkerMessage>>,
    send_chan: mpsc::Sender<WorkerMessage>
}

impl DownloadWorker {
    pub fn new(
        id: usize, url: Uri, auth_header: Option<String>,
        file_len: u64, block_size: u64, delta: Arc<AtomicU64>,
        recv_chan: mpsc::Receiver<WorkerMessage>,
        send_chan: mpsc::Sender<WorkerMessage>
    ) -> DownloadWorker {
        let client = hyper_client();

        DownloadWorker {
            id,
            url,
            auth_header,
            client,
            block_size,
            file_len,
            delta,
            cur_block_delta: Arc::new(AtomicU64::new(0)),
            recv_chan: Some(recv_chan),
            send_chan
        }
    }

    // Fork and start running this worker inside a new separate future ("thread")
    // In this project, the "thread" will actually mean futures because they
    // are basically coroutines (light-weight threads)
    pub fn fork_run(self) {
        tokio::spawn(self.run());
    }

    // Run an infinite loop to receive download
    // request from the main thread and download per request
    fn run(mut self) -> impl Future<Item = (), Error = ()> {
        use std::mem::replace;
        let cur_block_delta = self.cur_block_delta.clone();
        replace(&mut self.recv_chan, None).unwrap().for_each(move |message| {
            if let WorkerMessage::Download(block_id) = message {
                // We have been requested to download a block
                let send_chan = self.send_chan.clone();
                let worker_id = self.id;
                self.download_block(block_id)
                    .then(clone!(cur_block_delta; |result| {
                        let delta = cur_block_delta.swap(0, Ordering::Relaxed);
                        // Pass the result back to the master thread
                        match result {
                            Ok(bytes) => send_chan.send(WorkerMessage::Finished(worker_id, block_id, bytes)),
                            Err(e) => send_chan.send(WorkerMessage::Failed(worker_id, block_id, delta, e))
                        }
                    }))
                    .map_err(|_| ())
                    // The result is the channel itself, it's just a clone
                    .map(|_| ())
            } else {
                // We simply can't receive anything other than the Download command yet
                panic!("WTF???");
            }
        })
    }

    // Download a specified block with Range requests
    // and return a futrue that resolves when completes
    fn download_block(&self, block_id: usize) -> impl Future<Item = Bytes, Error = WorkerError> {
        // Calculate the start & end position of the block
        let block_start = self.block_size * block_id as u64;
        let mut block_end = block_start + self.block_size - 1;

        // If this is the last block, it might not be full-sized.
        if block_end >= self.file_len {
            block_end = self.file_len - 1;
        }

        // Construct the range request
        let req = Request::get(self.url.clone())
            .header(header::RANGE, format!("bytes={}-{}", block_start, block_end))
            .add_auth_header(&self.auth_header)
            .body(Body::empty()).unwrap();

        let v = BytesMut::with_capacity(self.block_size as usize);
        let delta = self.delta.clone();
        let cur_block_delta = self.cur_block_delta.clone();

        self.client.request(req)
            .map_err(|e| WorkerError::ConnectionError(e))
            .and_then(move |response| {
                if response.status() != StatusCode::PARTIAL_CONTENT {
                    // Well, PARTIAL_CONTENT is unsupported... (or failed)
                    // There is nothing we can do here.
                    Either::A(future::err(WorkerError::UnexpectedResponse(response.status())))
                } else {
                    // Collect the body for this block
                    Either::B(response.into_body()
                        .map_err(|e| WorkerError::ConnectionError(e))
                        .fold(v, move |mut v, chunk| {
                            // Record the newly downloaded chunk length
                            let len = chunk.len() as u64;
                            delta.fetch_add(len, Ordering::Relaxed);
                            cur_block_delta.fetch_add(len, Ordering::Relaxed);

                            // Accumulate into the buffer
                            v.extend_from_slice(&chunk);
                            future::ok(v)
                        })
                        .map(|v| v.freeze()))
                }
            })
    }
}