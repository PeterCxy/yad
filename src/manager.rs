use errors::*;
use futures::{Future, Stream, Sink};
use futures::future::{self, Either};
use futures::sync::mpsc;
use hyper::{Body, Request, Uri};
use hyper::header;
use std::io::SeekFrom;
use std::sync::{Arc, Mutex};
use tokio::{self, fs, io};
use util::*;
use worker::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockState {
    Pending,
    Downloading,
    Finished
}

#[derive(Debug)]
pub enum WorkerMessage {
    Download(usize),
    Finished(usize, usize, Vec<u8>),
    Failed(usize, usize, WorkerError)
}

#[derive(Debug)]
pub enum DownloadManagerError {
    Success, // Only used to break the stream...
    Error(Error)
}

impl<T> From<T> for DownloadManagerError where T: Into<Error> {
    fn from(e: T) -> DownloadManagerError {
        DownloadManagerError::Error(e.into())
    }
}

#[derive(Debug)]
pub struct DownloadManager {
    url: Uri,
    pub file_len: u64,
    pub block_count: usize,
    pub block_size: u64,
    pub file_name: String,
    blocks_state: Vec<BlockState>,
    blocks_pending: Vec<usize>
}

impl DownloadManager {
    pub fn new(url: Uri, block_size: u64) -> impl Future<Item = DownloadManager, Error = Error> {
        // Use HEAD request to find the metadata of the file
        // TODO: Support header customization!
        hyper_client().request(Request::head(url.clone()).body(Body::empty()).unwrap())
            .chain_err(|| "Failed to fetch for file information: Server error.")
            .and_then(|r| {
                r.headers().get(header::CONTENT_LENGTH)
                    .ok_or("Failed to request for content length".into())
                    .and_then(|l| l.to_str()
                        .chain_err(|| "Failed to get content length"))
                    .and_then(|l| l.parse()
                        .chain_err(|| "Failed to parse content length"))
            })
            .map(move |len| {
                Self::initialize(url, len, block_size)
            })
    }

    fn initialize(url: Uri, len: u64, block_size: u64) -> DownloadManager {
        // Divide the file into blocks of block_size, rounded up to an integer
        // Therefore, the last block might or might not be a full block. This
        // has to be dealt with by the download worker.
        let mut block_count = (len / block_size) as usize;
        if block_count as u64 * block_size < len {
            block_count += 1;
        }
        

        // Find the file name from the url
        // TODO: Make this configurable
        let file_name = {
            let path = url.path();
            let index = path.rfind("/");
            match index {
                Some(index) => (&path[index..]).replace("/", "").to_owned(),
                None => path.to_owned()
            }
        };

        DownloadManager {
            url,
            file_len: len,
            file_name,
            block_count,
            block_size,
            blocks_state: vec![BlockState::Pending; block_count],
            blocks_pending: (0..block_count).collect()
        }
    }

    pub fn run(mut self, workers: usize) -> impl Future<Item = (), Error = Error> {
        // Create the file first
        // TODO: Automatically create all directories
        // TODO: Add an option whether to erase if the file exists
        create_file_with_len(&self.file_name, self.file_len)
            .and_then(move |file| {
                let mut send_chan = Vec::with_capacity(workers);
                let (worker_send, recv_chan) = mpsc::channel(1024);
                for id in 0..workers {
                    let (tx1, worker_recv) = mpsc::channel(1024);
                    DownloadWorker::new(
                        id, self.url.clone(),
                        self.file_len, self.block_size, worker_recv, worker_send.clone()
                    ).fork_run();
                    send_chan.push(tx1);
                }

                // Assign initial tasks to the workers
                for id in 0..workers {
                    tokio::spawn(self.assign_download_task(&send_chan, id));
                }

                // Run the message loop
                self.run_loop(file, recv_chan, send_chan)
            })
    }

    fn run_loop(
        self,
        file: fs::File,
        recv_chan: mpsc::Receiver<WorkerMessage>,
        send_chan: Vec<mpsc::Sender<WorkerMessage>>
    ) -> impl Future<Item = (), Error = Error> {
        // Event loop for the controller future
        let this = Arc::new(Mutex::new(self));
        let send_chan = Arc::new(send_chan);
        recv_chan.map_err(|_| "".into()).fold(file, clone!(this; |file, message| {
            match message {
                WorkerMessage::Finished(worker, id, bytes) => {
                    Either::A(this.lock().unwrap().write_to_file(file, id, bytes)
                        .map_err(|e| DownloadManagerError::Error(e))
                        .and_then(clone!(this, send_chan; |file| {
                            // TODO: A proper progress bar
                            println!("=> Worker {} finished downloading block {}", worker, id);
                            // Mark the current one as completed
                            this.lock().unwrap().blocks_state[id] = BlockState::Finished;
                            if this.lock().unwrap().has_finished() {
                                // All download tasks finished. We use an "error"
                                // to terminate the stream, since this is
                                // the easiest way I could think of...
                                Either::A(future::err(DownloadManagerError::Success))
                            } else {
                                // We still haven't finished yet
                                // It might be caused by more pending blocks
                                // or that some worker is still running. Anyway,
                                // Try to assign a new task to the now idle worker
                                // It will silently skip if no more tasks available
                                Either::B(this.lock().unwrap().assign_download_task(&send_chan, worker)
                                    .map_err(|_| DownloadManagerError::Success)
                                    .and_then(|_| future::ok(file)))
                            }
                        })))
                },
                WorkerMessage::Failed(worker, id, err) => Either::B(match err {
                    WorkerError::ConnectionError(e) => {
                        // TODO: Add a limit to connection errors
                        println!("=> Worker {} failed while downloading block {} with error {:?}, retrying later", worker, id, e);
                        this.lock().unwrap().blocks_state[id] = BlockState::Pending;
                        this.lock().unwrap().blocks_pending.push(id); // Add it back to pending list
                        // Re-assign a download task
                        Either::A(this.lock().unwrap().assign_download_task(&send_chan, worker)
                            .map_err(|_| DownloadManagerError::Success)
                            .and_then(|_| future::ok(file)))
                    },
                    WorkerError::NoPartialContentSupport => {
                        println!("=> No partial content support on the server. Exiting.");
                        Either::B(future::err("No partial content support".into()))
                    }
                }),
                _ => panic!("WTF")
            }
        }))
        .then(|r| {
            match r {
                Ok(_) => future::ok(()),
                Err(r) => match r {
                    DownloadManagerError::Success => future::ok(()),
                    DownloadManagerError::Error(e) => future::err(e)
                }
            }
        })
    }

    // Write a downloaded block to the file
    fn write_to_file(&self, file: fs::File, block_id: usize, bytes: Vec<u8>) -> impl Future<Item = fs::File, Error = Error> {
        file.seek(SeekFrom::Start(self.block_size * block_id as u64))
            .and_then(move |(file, _)| io::write_all(file, bytes))
            .map(|(file, _)| file)
            .chain_err(|| "Error writing to file")
    }

    // Assign the next pending download task to a worker
    fn assign_download_task(&mut self, send_chan: &[mpsc::Sender<WorkerMessage>], worker_id: usize) -> impl Future<Item = (), Error = ()> {
        if self.blocks_pending.len() == 0 {
            Either::A(future::ok(()))
        } else {
            let block_id = self.blocks_pending.remove(0);
            self.blocks_state[block_id] = BlockState::Downloading;
            Either::B(send_chan[worker_id].clone()
                .send(WorkerMessage::Download(block_id))
                .map(|_| ())
                .map_err(|_| panic!("WTF")))
        }
    }

    // If there is no remaining blocks to download, and
    // no block is currently being downloaded, then we are done.
    fn has_finished(&self) -> bool {
        self.blocks_pending.len() == 0 &&
            !self.blocks_state.contains(&BlockState::Downloading)
    }
}