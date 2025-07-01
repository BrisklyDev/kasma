use crate::download_engine::http::fetch_file_info;
use crate::download_engine::http::http_download_worker::{Status, WorkerProgress};
use crate::download_engine::http::segment::byte_range::ByteRange;
use crate::download_engine::{
    DownloadInfo, Runnable, http::http_download_worker::HttpDownloadWorker,
};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
pub enum WorkerToEngineMsg {
    Progress(u8, u64),
    Completed(u8),
    Error(u8, String),
}

#[derive(Debug)]
pub enum EngineToWorkerMsg {
    Start,
    Stop,
}

pub struct HttpDownloadEngine {
    workers: Vec<Arc<Mutex<HttpDownloadWorker>>>,
}

impl Runnable for HttpDownloadEngine {
    fn run(&mut self) {
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
        let (worker_to_engine_tx, worker_to_engine_rx) =
            tokio::sync::mpsc::channel::<WorkerToEngineMsg>(100);
        let (engine_to_worker_tx, engine_to_worker_rx) =
            tokio::sync::mpsc::channel::<EngineToWorkerMsg>(100);

        let status_arc = Arc::new(Mutex::new(Status::Initial));
        let progress_arc = Arc::new(Mutex::new(WorkerProgress::new()));
        let cancel_handle = {
            thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
        
                rt.block_on(async move {
                    println!("Cancelation spawned!!");
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    println!("⛔ Sending cancel signal...");
                    engine_to_worker_tx.send(EngineToWorkerMsg::Stop).await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    engine_to_worker_tx.send(EngineToWorkerMsg::Start).await;
                });
            })
        };

        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<EngineToWorkerMsg>(100);
        let handle = {
            let mut worker = HttpDownloadWorker::new(
                download_info.clone(),
                range,
                worker_to_engine_tx,
                engine_to_worker_rx,
                status_arc.clone(),
                progress_arc.clone(),
            );
            thread::spawn(move || {
                worker.run(); // or spawn_worker_thread()
            })
        };
        cancel_handle.join().unwrap();
        handle.join().unwrap();
    }
}
impl HttpDownloadEngine {
    pub fn new() -> Self {
        HttpDownloadEngine { workers: vec![] }
    }
}
