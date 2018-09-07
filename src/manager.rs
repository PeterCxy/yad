use errors::*;
use futures::Future;
use hyper::{Body, Request, Uri};
use hyper::header;
use util::*;

#[derive(Clone, Debug)]
pub enum BlockState {
    Pending,
    Downloading,
    Finished
}

#[derive(Debug)]
pub struct DownloadManager {
    url: Uri,
    pub file_len: usize,
    pub block_count: usize,
    pub block_size: usize,
    pub file_name: String,
    blocks_state: Vec<BlockState>
}

impl DownloadManager {
    pub fn new(url: Uri, block_size: usize) -> impl Future<Item = DownloadManager, Error = Error> {
        // Use HEAD request to find the metadata of the file
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

    fn initialize(url: Uri, len: usize, block_size: usize) -> DownloadManager {
        // Divide the file into blocks of block_size, rounded up to an integer
        // Therefore, the last block might or might not be a full block. This
        // has to be dealt with by the download worker.
        let mut block_count = len / block_size;
        if block_count * block_size < len {
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
            blocks_state: vec![BlockState::Pending; block_count]
        }
    }
}