use std::thread;

use crate::download_engine::http::fetch_file_info;
use crate::download_engine::http::segment::byte_range::ByteRange;
use crate::download_engine::utils::shared_data::SharedData;
use crate::download_engine::{
    DownloadInfo, Engine, download_worker::DownloadWorker,
    http::http_download_worker::HttpDownloadWorker,
};

pub struct HttpDownloadEngine {
    workers: Vec<String>,
}

impl Engine for HttpDownloadEngine {
    fn spawn_engine_thread(&self) -> thread::JoinHandle<()> {
        thread::spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let file_info = rt
                .block_on(fetch_file_info("https://dl2.soft98.ir/soft/w/WinRAR.7.12.x64.zip?1751290404"))
                .expect("Failed to fetch file info");

            drop(rt);

            let download_info = DownloadInfo::from(&file_info);
            let range = ByteRange::new(0, download_info.file_size);
            let download_info_shared = SharedData::new(download_info);

            let handle = thread::spawn(move || {
                let worker = HttpDownloadWorker::new(download_info_shared.read_handle, &range);
                worker.start();
            });

            handle.join().unwrap();
        })
    }
}
impl HttpDownloadEngine {
    pub fn new() -> Self {
        HttpDownloadEngine { workers: vec![] }
    }
}
