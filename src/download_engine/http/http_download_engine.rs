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
use crate::download_engine::utils::now_millis;
use crate::download_engine::utils::sync_ext::MutexAnyhowExt;
use crate::download_engine::{
    DownloadInfo, DownloadItem, DownloadSetting, RunnableTask,
    http::http_download_worker::HttpDownloadWorker,
};
use anyhow::Ok;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::ops::Deref;
use std::os::linux::raw::stat;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::{any, fs, thread, u64};
use tokio::sync::mpsc::{Receiver, Sender};
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
    last_estimation_calc_time: u128,
    from_worker_tx: Sender<WorkerToEngineMsg>,
    from_worker_rx: Receiver<WorkerToEngineMsg>,
    spawned_workers: u8,
}

pub struct DownloadWorkerHandle {
    range: ByteRange,
    to_worker_tx: Sender<EngineToWorkerMsg>,
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
                last_estimation_calc_time: 0,
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
            Result::Ok(_) => {
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
        let mut worker_reset_ticker = interval(Duration::from_secs(4));
        let mut download_progress_ticker = interval(Duration::from_millis(200));
        self.handle_start().await?;

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
                _ = worker_reset_ticker.tick() => self.run_worker_reset_ticker().await?,
                _ = download_progress_ticker.tick() => self.handle_progress_updates()?,
            }
        }
    }

    fn handle_progress_updates(&mut self) -> anyhow::Result<()> {
        let total_bytes_speed = self.calculate_total_speed()?;
        let is_temp_write_complete = self.check_temp_write_completion()?;
        self.progress.total_download_progress = self.calculate_total_progress()?;
        self.calculate_estimated_remaining(total_bytes_speed)?;

        todo!()
    }

    fn calculate_estimated_remaining(&mut self, bytes_speed: u64) -> anyhow::Result<()> {
        let progresses = self.worker_progresses()?;
        if progresses.is_empty()
            || self.last_estimation_calc_time + 1000 > now_millis()
            || bytes_speed == 0
        {
            return Ok(());
        }

        let total_bytes: u64 = progresses.iter().map(|x| x.total_bytes_received).sum();
        let remaining_sec = (self.download_item.file_size - total_bytes) / bytes_speed;
        let mut estimated_remaining = "".to_string();

        let days = (remaining_sec % 31536000) / 86400;
        let hours = ((remaining_sec % 31536000) % 86400) / 3600;
        let minutes = (((remaining_sec % 31536000) % 86400) % 3600) / 60;
        let seconds = (((remaining_sec % 31536000) % 86400) % 3600) % 60;

        fn format_unit(value: u64, unit: &str) -> String {
            format!("{} {}{}", value, unit, if value == 1 { "" } else { "s" })
        }

        if days >= 1 {
            estimated_remaining = format_unit(hours, "Hour");
        } else if hours >= 1 {
            estimated_remaining = format!(
                "{}, {}",
                format_unit(hours, "Hour"),
                format_unit(minutes, "Minute")
            );
        } else if minutes >= 1 {
            let estimated_remaining = format!(
                "{}, {}",
                format_unit(minutes, "Minute"),
                format_unit(seconds, "Second")
            );
        } else if remaining_sec == 0 {
            estimated_remaining = "".to_string();
        } else {
            estimated_remaining = format_unit(remaining_sec, "Seconds");
        }

        drop(progresses);

        self.last_estimation_calc_time = now_millis();
        self.progress.estimated_remaining = estimated_remaining;
        self.progress.estimated_remaining_sec = remaining_sec;

        Ok(())
    }

    fn calculate_total_progress(&self) -> anyhow::Result<f64> {
        let total = self
            .worker_progresses()?
            .iter()
            .map(|p| p.total_download_progress)
            .sum();
        Ok(total)
    }

    fn worker_progresses(&self) -> anyhow::Result<Vec<MutexGuard<'_, WorkerProgress>>> {
        let progress_vec = self
            .workers
            .iter()
            .map(|w| w.1.progress_arc.lock_anyhow())
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(progress_vec)
    }

    fn check_temp_write_completion(&self) -> anyhow::Result<bool> {
        let all_complete = self
            .worker_progresses()?
            .iter()
            .all(|x| x.status == Status::RangeComplete);
        if !all_complete {
            return Ok(false);
        }
        self.validate_temp_files_integrity(true, true, true)?;
        let missing_ranges = self.find_missing_byte_ranges()?;
        for range in &missing_ranges {
            println!("Missing range:: {}", range);
        }
        Ok(missing_ranges.is_empty())
    }

    fn calculate_total_speed(&self) -> anyhow::Result<u64> {
        let speed_bytes = self
            .worker_progresses()?
            .iter()
            .map(|x| x.speed_bytes_per_sec)
            .sum();
        Ok(speed_bytes)
    }

    async fn run_worker_reset_ticker(&self) -> anyhow::Result<()> {
        let connections_to_reset = self.workers.iter().filter(|x| {
            let status = &x.1.progress_arc.lock().unwrap().status;
            !matches!(
                status,
                Status::Stopped | Status::Starting | Status::Complete
            )
        });
        for worker in connections_to_reset {
            worker.1.to_worker_tx.send(EngineToWorkerMsg::Reset).await?;
        }
        Ok(())
    }

    fn run_worker_reuse_ticker(&self) {}

    fn run_worker_spawner_ticker(&self) {}

    async fn pause_workers(&self) -> anyhow::Result<()> {
        // TODO: status validation
        for handle in &self.workers {
            let sender = &handle.1.to_worker_tx;
            sender.send(EngineToWorkerMsg::Stop).await?;
        }
        Ok(())
    }

    fn handle_worker_msg(&self, msg: WorkerToEngineMsg) {
        match msg.message {
            ToEngineMessage::Complete => todo!(),
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
            if missing_ranges.len() != 1
                && *missing_ranges.first().unwrap()
                    != ByteRange::new(0, self.download_item.file_size)
            {
                self.spawned_workers = self.setting.total_connections;
            }
            let tree = ByteRangeTree::from_missing_bytes(
                self.download_item.file_size,
                self.setting.total_connections - 1,
                missing_ranges,
            );
            println!("Tree result: {}", tree);
            self.byte_range_tree = Some(tree);
            let node_ref = self.byte_range_tree.as_ref().unwrap().root.clone();
            self.spawn_worker(0, node_ref).await;
        } else {
            // TODO: handle resume not initial
        }
        Ok(())
    }

    async fn spawn_worker(&mut self, worker_num: u8, tree_node: NodeRef) {
        let (engine_to_worker_tx, engine_to_worker_rx) =
            tokio::sync::mpsc::channel::<EngineToWorkerMsg>(100);
        let progress = Arc::new(Mutex::new(WorkerProgress::new()));
        let node = tree_node.borrow_mut();
        let worker_handle = DownloadWorkerHandle {
            range: node.range.clone(),
            to_worker_tx: engine_to_worker_tx.clone(),
            progress_arc: progress.clone(),
        };
        self.workers.insert(worker_num, worker_handle);
        let mut worker = HttpDownloadWorker::new(
            worker_num,
            self.download_item.clone(),
            node.range.clone(),
            self.from_worker_tx.clone(),
            engine_to_worker_rx,
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
