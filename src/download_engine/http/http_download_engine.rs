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
                .block_on(fetch_file_info(
                    "https://github.com/BrisklyDev/brisk/releases/download/v2.3.2/Brisk-v2.3.2-macos.dmg",
                ))
                .expect("Failed to fetch file info");

            drop(rt);

            let download_info = DownloadInfo::from(&file_info);
            let range = ByteRange::new(0, download_info.file_size);
            let download_info_shared = SharedData::new(download_info);
            let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let cancel_handle = {
                thread::spawn(move || {
                    println!("Cancelation spawned!!");
                    std::thread::sleep(std::time::Duration::from_secs(6));
                    println!("⛔ Sending cancel signal...");
                    let _ = cancel_tx.send(true);
                })
            };

            let handle = thread::spawn(move || {
                let mut worker =
                    HttpDownloadWorker::new(download_info_shared.read_handle, &range, cancel_rx);
                worker.start();
            });
            cancel_handle.join().unwrap();
            handle.join().unwrap();
        })
    }
}
impl HttpDownloadEngine {
    pub fn new() -> Self {
        HttpDownloadEngine { workers: vec![] }
    }
}
