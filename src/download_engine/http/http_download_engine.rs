use crate::download_engine::EngineState;
use crate::download_engine::http::byte_range::ByteRange;
use crate::download_engine::http::byte_range::byte_range_tree::{
    ByteRangeStatus, ByteRangeTree, NodeRef,
};
use crate::download_engine::http::fetch_file_info;
use crate::download_engine::http::http_download_worker::Status;
use crate::download_engine::http::message::{
    DownloadCommand, EngineToMainMsg, EngineToWorkerMsg, ToEngineMessage, WorkerToEngineMsg,
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
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::{any, fs, thread, u64};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::id;
use tokio::time::interval;
use uuid::Uuid;

pub const MINIMUM_DOWNLOADABLE_BYTE_RANGE_LEN: u64 = 500000;

pub struct HttpDownloadEngine {
    download_item: DownloadItem,
    state: EngineState,
    setting: DownloadSetting,
    from_main_rx: Receiver<DownloadCommand>,
    to_main_rx: Sender<EngineToMainMsg>,
    pending_worker_handshakes: Vec<u8>,
    reuse_worker_queue: Vec<u8>,
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
                pending_worker_handshakes: Vec::new(),
                reuse_worker_queue: Vec::new(),
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
            Err(e) => {
                print!("Error: {}", e);
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
                Some(msg) = self.from_worker_rx.recv() => self.handle_worker_msg(msg).await?,
                _ = worker_reuse_ticker.tick() => self.run_worker_reuse_ticker(),
                _ = worker_spawner_ticker.tick() => self.run_worker_spawner_ticker().await?,
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

        Ok(())
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
        let estimated_remaining;

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
            estimated_remaining = format!(
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
            let progress = x.1.progress_arc.lock().unwrap();
            !matches!(
                &progress.status,
                Status::Stopped | Status::Starting | Status::Complete
            ) && progress.last_response_time + (self.setting.reset_timeout_millis as u128)
                < now_millis()
        });
        for worker in connections_to_reset {
            worker.1.to_worker_tx.send(EngineToWorkerMsg::Reset).await?;
        }
        Ok(())
    }

    fn run_worker_reuse_ticker(&self) {}

    async fn run_worker_spawner_ticker(&mut self) -> anyhow::Result<()> {
        if self.should_spawn_worker() {
            self.request_byte_range_refresh_new_worker().await?;
        }
        Ok(())
    }

    /// TODO: doc
    async fn request_byte_range_refresh_new_worker(&mut self) -> anyhow::Result<()> {
        if self.byte_range_tree.is_none() {
            return Ok(());
        }
        let byte_range_tree = self.byte_range_tree.as_mut().unwrap();
        println!("Pre-split byte range tree:\n{}", byte_range_tree);
        if let Err(e) = byte_range_tree.split() {
            println!("_refreshConnectionSegments:: Fatal! {}", e);
            return Ok(());
        }
        println!("Post-split byte range tree:\n{}", byte_range_tree);
        println!("Refreshing worker ranges...");
        for (worker_number, handle) in &self.workers {
            let related_node = byte_range_tree
                .lowest_level_nodes
                .iter()
                .find(|x| x.borrow().worker_number == *worker_number);

            if related_node.is_none() {
                println!("Fatal error occurred! relatedSegmentNode is null!");
                return Ok(());
            }
            let mut node = related_node.unwrap().borrow_mut();
            let message = EngineToWorkerMsg::RefreshByteRange(node.range.clone(), false);
            node.status = ByteRangeStatus::RefreshRequested;
            self.spawned_workers += 1;
            handle.to_worker_tx.send(message).await?;
        }
        Ok(())
    }

    fn should_spawn_worker(&self) -> bool {
        if self.byte_range_tree.is_none() {
            return false;
        }
        let pending_exists = self
            .byte_range_tree
            .as_ref()
            .unwrap()
            .lowest_level_nodes
            .iter()
            .any(|n| n.borrow().status == ByteRangeStatus::RefreshRequested);

        todo!();
    }

    async fn pause_workers(&self) -> anyhow::Result<()> {
        // TODO: status validation
        for handle in &self.workers {
            let sender = &handle.1.to_worker_tx;
            sender.send(EngineToWorkerMsg::Stop).await?;
        }
        Ok(())
    }

    async fn handle_worker_msg(&mut self, msg: WorkerToEngineMsg) -> anyhow::Result<()> {
        match msg.message {
            ToEngineMessage::Complete(range) => {
                self.handle_range_download_completion(msg.worker_number, range)?;
            }
            ToEngineMessage::ByteRangeRefreshSuccess {
                requested_range,
                refreshed_range,
                reuse,
            } => {
                self.handle_refresh_byte_range_success(requested_range, reuse)
                    .await?
            }
            ToEngineMessage::ByteRangeRefreshRefused {
                requested_range,
                reuse,
            } => {
                self.handle_refresh_byte_range_refused(requested_range, reuse, msg.worker_number)
                    .await?;
            }
            ToEngineMessage::ByteRangeRefreshOverlapped {
                requested_range,
                refreshed_range,
                new_valid_range,
                reuse,
            } => {
                self.handle_refresh_byte_range_overlap(
                    requested_range,
                    refreshed_range,
                    new_valid_range,
                    reuse,
                )
                .await?;
            }
            ToEngineMessage::Stopped => todo!(),
            ToEngineMessage::Failed => todo!(),
            ToEngineMessage::HandshakeResponse { reuse } => {}
        }
        Ok(())
    }

    fn handle_range_download_completion(
        &mut self,
        worker_num: u8,
        range: ByteRange,
    ) -> anyhow::Result<()> {
        if self.reuse_worker_queue.contains(&worker_num) {
            self.reuse_worker_queue.push(worker_num);
        }
        let tree_ref = self.byte_range_tree.as_ref().unwrap();
        let node = tree_ref.search_node(&range);
        if node.is_none() {
            return Err(anyhow::anyhow!(format!(
                "handle_range_download_completion:: Failed to find node {} in tree for worker number {}",
                range, worker_num
            )));
        }
        node.unwrap().borrow_mut().status = ByteRangeStatus::Complete;
        Ok(())
    }

    async fn handle_refresh_byte_range_overlap(
        &mut self,
        requested_range: ByteRange,
        refreshed_range: ByteRange,
        new_valid_range: ByteRange,
        reuse: bool,
    ) -> anyhow::Result<()> {
        if self.byte_range_tree.is_none() {
            return Ok(());
        }
        let tree_ref = self.byte_range_tree.as_ref().unwrap();
        let node = tree_ref.search_node(&requested_range);
        if node.is_none() {
            println!("Fatal:: _handleRefreshSegmentSuccess:: Failed to find segment node.");
            return Ok(());
        }
        let parent_weak = node.unwrap().borrow().parent.clone();
        let parent_rc = parent_weak.upgrade().unwrap();

        let mut parent = parent_rc.borrow_mut();
        let left_child_rc = parent.left_child.as_ref().unwrap().clone();
        let right_child_rc = parent.right_child.as_ref().unwrap().clone();
        let mut left_child = left_child_rc.borrow_mut();
        let mut right_child = right_child_rc.borrow_mut();
        left_child.range = requested_range.clone();
        left_child.status = ByteRangeStatus::InUse;
        right_child.status = ByteRangeStatus::ReuseRequested;
        parent_rc.borrow_mut().status = ByteRangeStatus::Outdated;

        let mut new_worker_node_ref = right_child;
        let new_worker_node = right_child_rc.clone();
        new_worker_node_ref.range = new_valid_range.clone();
        if reuse {
            self.send_start_command_reuse_worker(
                new_worker_node_ref.worker_number,
                new_worker_node_ref.range.clone(),
            )
            .await?;
        } else {
            self.spawn_worker(new_worker_node_ref.worker_number, new_worker_node)
                .await;
        }
        Ok(())
    }

    async fn handle_refresh_byte_range_refused(
        &mut self,
        requested_range: ByteRange,
        reuse: bool,
        worker_number: u8,
    ) -> anyhow::Result<()> {
        if self.byte_range_tree.is_none() {
            return Ok(());
        }
        let tree_ref = self.byte_range_tree.as_ref().unwrap();
        let node = tree_ref.search_node(requested_range);
        if node.is_none() {
            println!("Fatal:: _handleRefreshSegmentSuccess:: Failed to find segment node.");
            return Ok(());
        }
        let parent_weak = node.unwrap().borrow().parent.clone();
        let parent_rc = parent_weak.upgrade().unwrap();
        let mut parent = parent_rc.borrow_mut();
        if reuse {
            self.reuse_worker_queue.push(worker_number);
            println!("Added connection {} to connection queue", worker_number);
        } else {
            // TODO: can we handle this?
        }

        let left_child_range = parent.left_child.as_ref().unwrap().borrow().range.clone();

        let l_idx = {
            drop(parent);
            let idx = tree_ref
                .lowest_level_nodes
                .iter()
                .position(|node| node.borrow().range == left_child_range);
            parent = parent_rc.borrow_mut();
            idx
        };

        if let Some(idx) = l_idx {
            let mut tree = self.byte_range_tree.as_mut().unwrap();
            tree.lowest_level_nodes
                .insert(idx, parent_weak.upgrade().clone().unwrap());

            let left_child_range = parent.left_child.as_ref().unwrap().borrow().range.clone();
            let right_child_range = parent.right_child.as_ref().unwrap().borrow().range.clone();
            drop(parent);
            let r_idx = tree
                .lowest_level_nodes
                .iter()
                .position(|node| node.borrow().range == right_child_range);
            let l_idx = tree
                .lowest_level_nodes
                .iter()
                .position(|node| node.borrow().range == left_child_range);

            if let Some(l_idx) = l_idx {
                tree.lowest_level_nodes.remove(l_idx);
            } else {
                println!("Fatal:: failed to find left child node in lowest level nodes!");
            }
            if let Some(r_idx) = r_idx {
                tree.lowest_level_nodes.remove(r_idx);
            } else {
                println!("Fatal:: failed to find right child node in lowest level nodes!");
            }
        } else {
            println!(
                "RefreshSegmentRefused:: Fatal error occurred! Failed to find segment node to insert"
            );
        }

        Ok(())
    }

    async fn handle_refresh_byte_range_success(
        &mut self,
        requested_range: ByteRange,
        reuse: bool,
    ) -> anyhow::Result<()> {
        if self.byte_range_tree.is_none() {
            return Ok(());
        }
        let tree = self.byte_range_tree.as_ref().unwrap();
        let node = tree.search_node(requested_range);
        if node.is_none() {
            println!("Fatal:: _handleRefreshSegmentSuccess:: Failed to find segment node.");
            return Ok(());
        }
        let parent_weak = node.unwrap().borrow().parent.clone();
        let parent_rc = parent_weak.upgrade().unwrap();
        let mut parent = parent_rc.borrow_mut();
        parent.status = ByteRangeStatus::Outdated;

        let worker_node = parent.right_child.as_ref().unwrap().clone();
        let mut worker_node_ref = parent.right_child.as_ref().unwrap().borrow_mut();
        if reuse {
            self.send_start_command_reuse_worker(
                worker_node_ref.worker_number,
                worker_node_ref.range.clone(),
            )
            .await?;
        } else {
            self.spawn_worker(worker_node_ref.worker_number, worker_node)
                .await;
            self.pending_worker_handshakes
                .push(worker_node_ref.worker_number);
        }
        parent.left_child.as_ref().unwrap().borrow_mut().status = ByteRangeStatus::InUse;
        worker_node_ref.status = ByteRangeStatus::InUse;
        Ok(())
    }

    async fn send_start_command_reuse_worker(
        &self,
        worker_num: u8,
        range: ByteRange,
    ) -> anyhow::Result<()> {
        if let Some(worker) = self.workers.get(&worker_num) {
            worker
                .to_worker_tx
                .send(EngineToWorkerMsg::StartReuseConnection(range))
                .await?;

            Ok(())
        } else {
            Err(anyhow::anyhow!(format!(
                "Failed to find worker {} when sending start command reuse worker",
                worker_num
            )))
        }
    }

    fn handle_worker_handshake(&mut self, msg: WorkerToEngineMsg) {
        self.pending_worker_handshakes
            .retain(|x| x != &msg.worker_number);

        // TODO: reuse
    }

    async fn handle_start(&mut self) -> anyhow::Result<()> {
        if self.workers.is_empty() && self.byte_range_tree.is_none() {
            println!("Inside start");
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
        let mut node = tree_node.borrow_mut();
        let worker_handle = DownloadWorkerHandle {
            range: node.range.clone(),
            to_worker_tx: engine_to_worker_tx.clone(),
            progress_arc: progress.clone(),
        };
        self.workers.insert(worker_num, worker_handle);
        node.status = ByteRangeStatus::InUse;
        let mut worker = HttpDownloadWorker::new(
            worker_num,
            self.setting.clone(),
            self.download_item.clone(),
            node.range.clone(),
            self.from_worker_tx.clone(),
            engine_to_worker_rx,
            progress,
        );
        self.pending_worker_handshakes.push(worker_num);
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
        let dir = self.setting.base_temp_dir.join(&self.download_item.uid);
        if (!dir.exists()) {
            return Ok(());
        }
        let temp_files = list_temp_files_sorted(dir)?;
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
