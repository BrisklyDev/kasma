use std::sync::Arc;

use crate::download_engine::{
    DownloadInfo,
    download_worker::DownloadWorker,
    http::segment::byte_range::ByteRange,
    utils::shared_data::{self, CachedData, ReadHandle},
};

/// The worker responsible for downloading a file using HTTP
struct HttpDownloadWorker<'a> {
    download_info: ReadHandle<DownloadInfo>,
    worker_number: u8,
    byte_range: CachedData<'a, ByteRange>,
}

impl<'a> HttpDownloadWorker<'a> {
    pub fn new() -> Self {
        todo!()
    }
}

impl<'a> DownloadWorker for HttpDownloadWorker<'a> {
    fn spawn_worker_thread(&self) -> std::thread::JoinHandle<()> {
        todo!()
    }

    fn start(&self) {
        todo!()
    }

    fn pause(&self) {
        todo!()
    }

    fn cancel(&self) {
        todo!()
    }
}
