use bytes::Bytes;
use errors::*;
use futures::{Future, Stream, Sink};
use futures::future::{self, Either};
use futures::sync::mpsc;
use hyper::{Body, Request, StatusCode, Uri};
use hyper::header;
use percent_encoding::percent_decode;
use speed::SpeedMeter;
use std::io::SeekFrom;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use terminal_size;
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
    Progress(usize, u64),
    Finished(usize, usize, Bytes),
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
    downloaded_len: u64,
    meter: SpeedMeter,
    auth_header: Option<String>,
    blocks_state: Vec<BlockState>,
    blocks_downloaded: Vec<u64>, // A cache mainly for recording how much is done for running jobs
    blocks_pending: Vec<usize>
}

impl DownloadManager {
    pub fn new(url: Uri, block_size: u64) -> impl Future<Item = DownloadManager, Error = Error> {
        let auth_header = parse_url_basic_auth(&url);
        // Use HEAD request to find the metadata of the file
        // TODO: Support header customization!
        hyper_client().request(Self::create_test_request(&url, &auth_header))
            .chain_err(|| "Failed to fetch for file information: Server error.")
            .and_then(|r| {
                if r.status() == StatusCode::OK {
                    // We sent a request to the server with the Range header
                    // but it only responded with OK, which means that it
                    // does not support the Range header
                    // in which case we just have nothing to do...
                    Err("Server does not support Range request. Please use single-threaded downloader instead".into())
                } else if r.status() != StatusCode::PARTIAL_CONTENT {
                    // Not a successful response either
                    Err(format!("Failed server response: {}", r.status()).into())
                } else {
                    let file_name = parse_content_disposition(&r);
                    r.headers().get(header::CONTENT_LENGTH)
                        .ok_or("Failed to request for content length".into())
                        .and_then(|l| l.to_str()
                            .chain_err(|| "Failed to get content length"))
                        .and_then(|l| l.parse()
                            .chain_err(|| "Failed to parse content length"))
                        .map(move |l| (l, file_name))
                }
            })
            .map(move |(len, file_name)| {
                Self::initialize(url, auth_header, file_name, len, block_size)
            })
    }

    // Create a dummy request to test if the server is available
    // and if it supports the Range header
    fn create_test_request(url: &Uri, auth_header: &Option<String>) -> Request<Body> {
        // Use GET because some server might not respond correctly to HEAD
        Request::get(url.clone())
            .add_auth_header(&auth_header)
            .header(header::RANGE, "bytes=0-")
            .body(Body::empty()).unwrap()
    }

    fn initialize(url: Uri, auth_header: Option<String>, file_name: Option<String>, len: u64, block_size: u64) -> DownloadManager {
        // Divide the file into blocks of block_size, rounded up to an integer
        // Therefore, the last block might or might not be a full block. This
        // has to be dealt with by the download worker.
        let mut block_count = (len / block_size) as usize;
        if block_count as u64 * block_size < len {
            block_count += 1;
        }

        // Find the file name from the url
        // TODO: Make this configurable
        let file_name = if let Some(file_name) = file_name {
            println!("=> Using server-provided file name: {}", file_name);
            file_name
        } else {
            percent_decode({
                let path = url.path();
                let index = path.rfind("/");
                match index {
                    Some(index) => (&path[index..]).replace("/", "").to_owned(),
                    None => path.to_owned()
                }
            }.as_bytes()).decode_utf8_lossy().into_owned()
        };

        DownloadManager {
            url,
            auth_header,
            file_len: len,
            file_name,
            block_count,
            block_size,
            meter: SpeedMeter::new(Duration::from_millis(100), 10),
            downloaded_len: 0,
            blocks_state: vec![BlockState::Pending; block_count],
            blocks_downloaded: vec![0; block_count],
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
                        id, self.url.clone(), self.auth_header.clone(),
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
            let mut _this = this.lock().unwrap();
            match message {
                WorkerMessage::Finished(worker, id, bytes) => {
                    Either::A(_this.write_to_file(file, id, bytes)
                        .map_err(|e| DownloadManagerError::Error(e))
                        .and_then(clone!(this, send_chan; |file| {
                            let mut _this = this.lock().unwrap();
                            // Mark the current one as completed
                            _this.blocks_state[id] = BlockState::Finished;

                            if _this.has_finished() {
                                // All download tasks finished. We use an "error"
                                // to terminate the stream, since this is
                                // the easiest way I could think of...
                                // Before that, we update the process to 100% at least
                                _this.downloaded_len = _this.file_len;
                                _this.print_progress();
                                Either::A(future::err(DownloadManagerError::Success))
                            } else {
                                // If there is still unsynchronized progress report
                                // report it now.
                                if _this.blocks_downloaded[id] < _this.block_size {
                                    let len = _this.block_size - _this.blocks_downloaded[id];
                                    _this.progress(id, len);
                                }
                                // We still haven't finished yet
                                // It might be caused by more pending blocks
                                // or that some worker is still running. Anyway,
                                // Try to assign a new task to the now idle worker
                                // It will silently skip if no more tasks available
                                Either::B(_this.assign_download_task(&send_chan, worker)
                                    .map_err(|_| DownloadManagerError::Success)
                                    .and_then(|_| future::ok(file)))
                            }
                        })))
                },
                WorkerMessage::Failed(worker, id, err) => Either::B(match err {
                    WorkerError::ConnectionError(e) => {
                        // TODO: Add a limit to connection errors
                        println!("\r=> Worker {} failed while downloading block {} with error {:?}, retrying later", worker, id, e);
                        _this.blocks_state[id] = BlockState::Pending;
                        _this.blocks_pending.push(id); // Add it back to pending list
                        _this.downloaded_len -= _this.blocks_downloaded[id];
                        _this.blocks_downloaded[id] = 0;
                        _this.print_progress();
                        // Re-assign a download task
                        Either::A(_this.assign_download_task(&send_chan, worker)
                            .map_err(|_| DownloadManagerError::Success)
                            .and_then(|_| future::ok(file)))
                    },
                    WorkerError::UnexpectedResponse(code) => {
                        // TODO: Maybe we can still retry for some error codes?
                        println!("\n=> Unexpected response from server: {}", code);
                        Either::B(future::err("Unexpected response".into()))
                    }
                }),
                WorkerMessage::Progress(block_id, len) => {
                    _this.progress(block_id, len);
                    Either::B(Either::B(future::ok(file)))
                },
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
    fn write_to_file(&self, file: fs::File, block_id: usize, bytes: Bytes) -> impl Future<Item = fs::File, Error = Error> {
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

    // Add some length to a block's known downloaded length
    // print progress report if needed
    fn progress(&mut self, block_id: usize, len: u64) {
        self.blocks_downloaded[block_id] += len;
        self.downloaded_len += len;
        if self.meter.add(len) {
            self.print_progress();
        }
    }

    fn print_progress(&mut self) {
        let percentage = self.downloaded_len as f64 / self.file_len as f64;
        let percentage_100 = percentage * 100f64;
        let speed_raw = self.meter.get_speed_per_sec();
        let speed = build_human_readable_speed(speed_raw);
        let eta = build_human_readable_eta(speed_raw, self.file_len - self.downloaded_len);

        if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
            // Only print the progress bar when we can get the size of the terminal
            // Prepare the text to print before and after the progress bar
            let precedent = format!("=> Downloading: [");
            let succedent = format!("] {:.1}% {} ETA {}", percentage_100, speed, eta);

            // If too small, just fallback
            if precedent.len() + succedent.len() + 4 >= w as usize {
                Self::print_progress_fallback(percentage_100);
                return;
            }

            // Calculate how many "-" and ">" do we need to print the progress bar
            let num_all = w - 4 - precedent.len() as u16 - succedent.len() as u16;
            let num_finished = (num_all as f64 * percentage) as u16;

            // Go back to the head of line
            print!("\r");

            // Print the precedent text
            print!("{}", precedent);

            // Print the finished part
            for _ in 0..num_finished {
                print!(">");
            }

            // Print the unfinished part
            for _ in num_finished..num_all {
                print!("-");
            }

            // Print the succedent
            print!("{}", succedent);
        } else {
            Self::print_progress_fallback(percentage_100);
        }
    }

    fn print_progress_fallback(percentage_100: f64) {
        print!("\r");
        print!("=> Progress: {:.2}%", percentage_100)
    }
}