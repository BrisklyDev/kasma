use crate::download_engine::http::byte_range::ByteRange;
use crate::download_engine::http::byte_range::byte_range_tree::ByteRangeTree;
use crate::download_engine::http::http_download_worker::{Status, WorkerProgress};
use crate::download_engine::http::message::{DownloadCommand, EngineToMainMsg, WorkerToEngineMsg};
use crate::download_engine::http::{FileInfo, fetch_file_info};
use crate::download_engine::utils::{TempFileMetadata, list_temp_files_sorted};
use crate::download_engine::{
    DownloadItem, RunnableTask, http::http_download_worker::HttpDownloadWorker,
};
use std::collections::HashMap;
use std::iter::Map;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{fs, thread};
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Debug)]
pub enum EngineToWorkerMsg {
    Start,
    Stop,
    Reset,
    RefreshSegment(ByteRange, bool),
}

pub const MINIMUM_DOWNLOADABLE_BYTE_RANGE_LEN: u64 = 500000;

pub struct HttpDownloadEngine {
    download_item: DownloadItem,
    from_main_rx: Receiver<DownloadCommand>,
    to_main_rx: Sender<EngineToMainMsg>,
    byte_range_tree: Option<ByteRangeTree>,
    workers: HashMap<u8, DownloadWorkerHandle>,
}

pub struct DownloadWorkerHandle {
    range: ByteRange,
    to_worker_tx: Sender<EngineToWorkerMsg>,
    status_arc: Arc<Mutex<Status>>,
    progress_arc: Arc<Mutex<WorkerProgress>>,
}

impl RunnableTask for HttpDownloadEngine {
    /// Spawns a tokio thread and runs the task. This is merely an entry point to the engine that
    /// spawns the engine server thread which listens to commands from the caller. To actually start
    /// a download, the start command has to be sent after running the engine.
    fn run(&mut self) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(self.run_async());
    }
}
impl HttpDownloadEngine {
    pub fn new(
        from_main_rx: Receiver<DownloadCommand>,
        to_main_rx: Sender<EngineToMainMsg>,
    ) -> Self {
        HttpDownloadEngine {
            from_main_rx,
            to_main_rx,
            byte_range_tree: None,
            workers: HashMap::new(),
        }
    }

    async fn run_async(&mut self) {
        let file_info = self.fetch_file_info();
        let download_item = DownloadItem::from(&file_info);
        let range = ByteRange::new(0, download_item.file_size);
        let (worker_to_engine_tx, worker_to_engine_rx) =
            tokio::sync::mpsc::channel::<WorkerToEngineMsg>(100);
        let (engine_to_worker_tx, engine_to_worker_rx) =
            tokio::sync::mpsc::channel::<EngineToWorkerMsg>(100);
        println!("Total file size: {}", download_item.file_size);

        match self.from_main_rx.recv().await.unwrap() {
            DownloadCommand::Start => self.handle_start().await,
            DownloadCommand::Pause => {}
        }

        let status_arc = Arc::new(Mutex::new(Status::Initial));
        let progress_arc = Arc::new(Mutex::new(WorkerProgress::new()));
        let worker_handle = DownloadWorkerHandle {
            range: range.clone(),
            to_worker_tx: engine_to_worker_tx.clone(),
            status_arc: status_arc.clone(),
            progress_arc: progress_arc.clone(),
        };
        self.workers.insert(0, worker_handle);
        let handle = {
            let mut worker = HttpDownloadWorker::new(
                0,
                download_item.clone(),
                range,
                worker_to_engine_tx,
                engine_to_worker_rx,
                status_arc.clone(),
                progress_arc.clone(),
            );
            thread::spawn(move || {
                worker.run();
            })
        };
        handle.join().unwrap();
    }

    async fn handle_start(&mut self) {
        if self.workers.is_empty() {
            self.validate_temp_files_integrity();
            self.find_missing_byte_ranges();
        }
    }

    /// Checks the temp files' integrity and optionally deletes corrupted files by checking for missing
    /// ranges (optional), clashing ranges between different workers, inconsistent temp file range
    /// with the temp file's actual size.
    fn validate_temp_files_integrity(
        &self,
        check_missing_range: bool,
        delete_corrupted: bool,
        restart_engine_on_corrupted: bool,
    ) -> anyhow::Result<()> {
        let temp_files = list_temp_files_sorted(PathBuf::from("/tmp/brisk/"))?;
        if temp_files.is_empty() {
            return Ok(());
        }
        let mut files_to_delete: Vec<&TempFileMetadata> = vec![];
        for idx in 0..temp_files.len() {
            let curr_file = &temp_files[idx];
            if curr_file.end_byte - curr_file.start_byte + 1 != curr_file.size {
                print!("found bad length...");
                files_to_delete.push(curr_file);
            }
            if curr_file.start_byte > self.download_item.file_size
                || curr_file.end_byte > self.download_item.file_size
            {
                println!("Byte range exceeding total length");
                files_to_delete.push(curr_file);
            }
            if idx == temp_files.len() - 1 {
                continue;
            }
            let next_file = &temp_files[idx + 1];
            if next_file.start_byte - curr_file.end_byte == 2 {
                files_to_delete.push(curr_file);
                if idx - 1 < 0 {
                    files_to_delete.push(next_file);
                } else {
                    files_to_delete.push(&temp_files[idx - 1]);
                }
            }
            if check_missing_range && next_file.start_byte - 1 != curr_file.end_byte {
                println!("Found missing range");
                files_to_delete.push(curr_file);
                files_to_delete.push(next_file);
            }
            for idx_other in (idx + 1)..temp_files.len() {
                let other_file = &temp_files[idx_other];
                let curr_range = ByteRange::new(curr_file.start_byte, curr_file.end_byte);
                let other_range = ByteRange::new(other_file.start_byte, other_file.end_byte);
                if other_file.start_byte > curr_file.end_byte {
                    break;
                }
                if curr_range.overlaps_with(&other_range) || other_range.overlaps_with(&curr_range)
                {
                    println!("Found overlap");
                    files_to_delete.push(curr_file);
                    files_to_delete.push(other_file);
                }
            }
        }
        // TODO: add logging
        let mut bad_file_existed = false;
        if delete_corrupted {
            for file in files_to_delete {
                bad_file_existed = true;
                print!("Deleting file...");
                if fs::remove_file(&file.path).is_err() {
                    println!("Failed");
                    // TODO restart engine
                }
                println!("Done");
            }
        }
        if restart_engine_on_corrupted && bad_file_existed {
            print!("Restarting engine...");
            // TODO: restart engine
        }
        Ok(())
    }

    fn find_missing_byte_ranges(&self) {
        todo!()
    }

    fn fetch_file_info(&self) -> FileInfo {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fetch_file_info(
                "https://github.com/BrisklyDev/brisk/releases/download/v2.3.2/Brisk-v2.3.2-macos.dmg",
            ))
            .expect("Failed to fetch file info")
    }
}
