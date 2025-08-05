use crate::download_engine::EngineState;
use crate::download_engine::http::byte_range::ByteRange;
use crate::download_engine::http::byte_range::byte_range_tree::{ByteRangeTree, NodeRef};
use crate::download_engine::http::fetch_file_info;
use crate::download_engine::http::http_download_worker::Status;
use crate::download_engine::http::message::{
    DownloadCommand, EngineToMainMsg, ToEngineMessage, WorkerToEngineMsg,
};
use crate::download_engine::http::progress::{DownloadProgress, WorkerProgress};
use crate::download_engine::utils::file::{
    TempFileMetadata, list_temp_files_sorted, resolve_versioned_file_path,
};
use crate::download_engine::{
    DownloadInfo, DownloadItem, DownloadSetting, RunnableTask,
    http::http_download_worker::HttpDownloadWorker,
};
use anyhow::Ok;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, result, slice, thread};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task;
use tokio::time::interval;
use uuid::Uuid;

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
    state: EngineState,
    setting: DownloadSetting,
    from_main_rx: Receiver<DownloadCommand>,
    to_main_rx: Sender<EngineToMainMsg>,
    byte_range_tree: Option<ByteRangeTree>,
    workers: HashMap<u8, DownloadWorkerHandle>,
    progress: DownloadProgress,
    from_worker_tx: Sender<WorkerToEngineMsg>,
    from_worker_rx: Receiver<WorkerToEngineMsg>,
    spawned_workers: u8,
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
        info: DownloadInfo,
        setting: DownloadSetting,
        output_file_name_override: Option<String>,
    ) -> (HttpDownloadEngine, String) {
        let uid = if info.uid.is_some() {
            info.uid.unwrap()
        } else {
            Uuid::new_v4().to_string()
        };
        let item = DownloadItem {
            uid: uid.clone(),
            url: info.url,
            prefetched_info: info.file_size.is_some() && info.supports_range.is_some(),
            headers: HashMap::new(),
            supports_range: if info.supports_range.is_some() {
                info.supports_range.unwrap()
            } else {
                false
            },
            file_size: if info.file_size.is_some() {
                info.file_size.unwrap()
            } else {
                0
            },
            file_name: if output_file_name_override.is_some() {
                output_file_name_override.unwrap()
            } else if info.filename.is_some() {
                info.filename.unwrap()
            } else {
                "".to_string()
            },
        };
        let (worker_to_engine_tx, worker_to_engine_rx) =
            tokio::sync::mpsc::channel::<WorkerToEngineMsg>(100);
        (
            HttpDownloadEngine {
                setting,
                state: EngineState::Initial,
                download_item: item,
                from_main_rx,
                to_main_rx,
                byte_range_tree: None,
                workers: HashMap::new(),
                progress: DownloadProgress::new(),
                spawned_workers: 0,
                from_worker_rx: worker_to_engine_rx,
                from_worker_tx: worker_to_engine_tx,
            },
            uid,
        )
    }

    async fn run_async(&mut self) {
        if !self.download_item.prefetched_info {
            // TODO: error handling
            let info = fetch_file_info(self.download_item.url.clone())
                .await
                .unwrap();
            self.download_item.file_size = info.file_size;
            self.download_item.supports_range = info.supports_range;
            if self.download_item.file_name.is_empty() {
                self.download_item.file_name = info.file_name;
            }
        }
        println!("Total file size: {}", self.download_item.file_size);
        match self.run_event_loop().await {
            anyhow::Result::Ok(_) => {
                // TODO: terminate workers
            }
            Err(_) => {
                // TODO: restart engine
            }
        };
    }

    async fn run_event_loop(&mut self) -> anyhow::Result<()> {
        self.state = EngineState::Running;
        let mut worker_reuse_ticker = interval(Duration::from_secs(1));
        let mut worker_spawner_ticker = interval(Duration::from_secs(2));
        let mut connection_reset_ticker = interval(Duration::from_secs(4));

        loop {
            if let EngineState::Complete = self.state {
                return Ok(());
            }
            tokio::select! {
                biased;
                Some(cmd) = self.from_main_rx.recv() => match cmd {
                    DownloadCommand::Start => self.handle_start().await?,
                    DownloadCommand::Pause => self.pause_workers().await?,
                },
                Some(msg) = self.from_worker_rx.recv() => self.handle_worker_msg(msg),
                _ = worker_reuse_ticker.tick() => self.run_worker_reuse_ticker(),
                _ = worker_spawner_ticker.tick() => self.run_worker_spawner_ticker(),
                _ = connection_reset_ticker.tick() => self.run_connection_reset_ticker(),
            }
        }
    }

    fn run_connection_reset_ticker(&self) {}

    fn run_worker_reuse_ticker(&self) {}

    fn run_worker_spawner_ticker(&self) {}

    async fn pause_workers(&self) -> anyhow::Result<()> {
        for handle in &self.workers {
            let sender = &handle.1.to_worker_tx;
            sender.send(EngineToWorkerMsg::Stop).await?;
        }
        Ok(())
    }

    fn handle_worker_msg(&self, msg: WorkerToEngineMsg) {
        match msg.message {
            ToEngineMessage::Completed => todo!(),
            ToEngineMessage::ByteRangeRefreshSuccess {
                refreshed_start_byte,
                refreshed_end_byte,
                reuse,
            } => todo!(),
            ToEngineMessage::ByteRangeRefreshRefused {
                requested_range,
                reuse,
            } => todo!(),
            ToEngineMessage::ByteRangeRefreshOverlapped {
                new_valid_start_byte,
                new_valid_end_byte,
                refreshed_start_byte,
                refreshed_end_byte,
            } => todo!(),
            ToEngineMessage::Stopped => todo!(),
            ToEngineMessage::Failed => todo!(),
        }
    }

    async fn handle_start(&mut self) -> anyhow::Result<()> {
        if self.workers.is_empty() && self.byte_range_tree.is_none() {
            self.validate_temp_files_integrity(true, true, false)?;
            let missing_ranges = self.find_missing_byte_ranges()?;
            if missing_ranges.is_empty() && self.is_assemble_eligible() {
                self.assemble_file()?;
                return Ok(());
            }
            let tree = ByteRangeTree::new_from_missing_bytes(
                self.download_item.file_size,
                self.setting.total_connections - 1,
                missing_ranges,
            );
            println!("Tree result: {}", tree);
            if tree.lowest_level_nodes.len() != 1 {
                self.spawned_workers = self.setting.total_connections;
            }
            self.byte_range_tree = Some(tree);
            let node_ref = &self.byte_range_tree.as_ref().unwrap().root;
            self.spawn_worker(0, self.byte_range_tree.as_ref().unwrap().root.clone());
        } else {
            // TODO: handle resume not initial
        }
        Ok(())
    }

    async fn spawn_worker(&mut self, worker_num: u8, tree_node: NodeRef) {
        let (engine_to_worker_tx, engine_to_worker_rx) =
            tokio::sync::mpsc::channel::<EngineToWorkerMsg>(100);
        let status = Arc::new(Mutex::new(Status::Initial));
        let progress = Arc::new(Mutex::new(WorkerProgress::new()));
        let node = tree_node.borrow_mut();
        let worker_handle = DownloadWorkerHandle {
            range: node.range.clone(),
            to_worker_tx: engine_to_worker_tx.clone(),
            status_arc: status.clone(),
            progress_arc: progress.clone(),
        };
        self.workers.insert(worker_num, worker_handle);
        let mut worker = HttpDownloadWorker::new(
            worker_num,
            self.download_item.clone(),
            node.range.clone(),
            self.from_worker_tx.clone(),
            engine_to_worker_rx,
            status.clone(),
            progress,
        );
        thread::spawn(move || worker.run());
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
        let temp_files =
            list_temp_files_sorted(self.setting.base_temp_dir.join(&self.download_item.uid))?;
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
                if idx == 0 {
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

    fn find_missing_byte_ranges(&self) -> anyhow::Result<Vec<ByteRange>> {
        let temp_dir_path =
            PathBuf::from(&self.setting.base_temp_dir).join(&self.download_item.uid);
        let mut temp_files: Vec<TempFileMetadata> = vec![];
        if temp_dir_path.is_dir() {
            temp_files = list_temp_files_sorted(temp_dir_path)?;
        }
        if temp_files.is_empty() {
            return Ok(vec![ByteRange::new(0, self.download_item.file_size)]);
        }

        let mut missing_ranges: Vec<ByteRange> = vec![];
        for idx in 0..temp_files.len() {
            let curr_file = &temp_files[idx];
            if idx == 0 {
                if curr_file.start_byte != 0 {
                    missing_ranges.push(ByteRange::new(curr_file.start_byte, curr_file.end_byte));
                }
                continue;
            }
            let prev_file = &temp_files[idx - 1];
            if prev_file.end_byte + 1 != curr_file.start_byte {
                missing_ranges.push(ByteRange::new(
                    prev_file.end_byte + 1,
                    curr_file.start_byte - 1,
                ));
            }
            if idx == temp_files.len() - 1 && curr_file.end_byte != self.download_item.file_size - 1
            {
                missing_ranges.push(ByteRange::new(
                    curr_file.start_byte + 1,
                    self.download_item.file_size,
                ));
            }
        }

        missing_ranges.sort_by(|a, b| a.start.cmp(&b.start));
        Ok(missing_ranges)
    }

    fn is_assemble_eligible(&self) -> bool {
        todo!()
    }

    fn assemble_file(&self) -> anyhow::Result<bool> {
        let temp_files = list_temp_files_sorted(
            PathBuf::from(&self.setting.base_temp_dir).join(&self.download_item.uid),
        )?;
        let mut file_to_write =
            PathBuf::from(&self.setting.base_save_dir).join(&self.download_item.file_name);
        if file_to_write.exists() {
            file_to_write = resolve_versioned_file_path(
                self.download_item.file_name.clone(),
                &self.setting.base_save_dir,
            )?;
        }
        if File::create(&file_to_write).is_err() {
            file_to_write = resolve_versioned_file_path(
                self.download_item.uid.clone(),
                &self.setting.base_save_dir,
            )?;
            File::create(&file_to_write)?;
        }

        let mut output = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_to_write)?;

        for temp_file in temp_files {
            let data = fs::read(&temp_file.path)?;
            output.write_all(&data)?;
        }

        let success = output.metadata()?.len() == self.download_item.file_size;
        if success {
            // TODO kill workers
        } else {
            println!("Assemble failed");
        }
        // TODO: notify progress

        Ok(success)
    }
}
