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
use crate::download_engine::setting::DownloadSetting;
use crate::download_engine::utils::file::{
    TempFileMetadata, list_temp_files_sorted, resolve_versioned_file_path,
};
use crate::download_engine::utils::now_millis;
use crate::download_engine::utils::sync_ext::MutexAnyhowExt;
use crate::download_engine::{
    DownloadInfo, DownloadItem, RunnableTask, http::http_download_worker::HttpDownloadWorker,
};
use crate::{engine_warn, unwrap_or_bail};
use anyhow::{Ok, anyhow};
use std::cmp::PartialEq;
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use std::{fs, thread, u64};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::interval;
use uuid::Uuid;

/// Defines the minimum allowed byte range length.
///
/// When a byte range is smaller than this, it will no longer be split and assigned to another worker,
/// preventing unreasonably small download requests.
pub const MINIMUM_DOWNLOADABLE_BYTE_RANGE_LEN: u64 = 500_000;

/// The interval duration at which the `worker_reuse_ticker` fires.
pub const WORKER_REUSE_TICKER_SECS: u64 = 1;

/// The interval duration at which the `worker_spawner_ticker` fires.
pub const WORKER_SPAWNER_TICKER_SECS: u64 = 2;

/// The interval duration at which the `worker_reset_ticker` fires.
pub const WORKER_RESET_TICKER_SECS: u64 = 1;

/// The download engine is responsible for spawning download workers, coordinating byte range
/// assignments between workers, restarting hanging connections, validating the integrity of
/// temporary files, assembling the final file, and more.
///
/// Essentially, it runs as an async Tokio task that manages, coordinates, and closely monitors
/// the entire download process.
pub struct HttpDownloadEngine {
    download_item: DownloadItem,
    state: EngineState,
    setting: DownloadSetting,
    from_main_rx: Receiver<DownloadCommand>,
    to_main_rx: Sender<EngineToMainMsg>,
    pending_worker_handshakes: Vec<u8>,
    reuse_worker_queue: VecDeque<u8>,
    byte_range_tree: Option<ByteRangeTree>,
    workers: HashMap<u8, DownloadWorkerHandle>,
    assemble_requested: bool,
    progress: DownloadProgress,
    last_estimation_calc_time: u128,
    from_worker_tx: Sender<WorkerToEngineMsg>,
    from_worker_rx: Receiver<WorkerToEngineMsg>,
    spawned_workers: u8,
}

pub struct DownloadWorkerHandle {
    assigned_to_worker: bool,
    range: ByteRange,
    to_worker_tx: Sender<EngineToWorkerMsg>,
    engine_to_worker_rx: Option<Receiver<EngineToWorkerMsg>>,
    progress_arc: Arc<Mutex<WorkerProgress>>,
    awaiting_reset_response: bool,
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
                spawned_workers: 1,
                assemble_requested: false,
                from_worker_rx: worker_to_engine_rx,
                from_worker_tx: worker_to_engine_tx,
                pending_worker_handshakes: Vec::new(),
                reuse_worker_queue: VecDeque::new(),
            },
            uid,
        )
    }

    /// Runs the main async task of running the engine.
    ///
    /// If the download item lacks prefetched info, this method fetches the file metadata
    /// such as file size, support for range requests, and filename.
    ///
    /// It then starts the main event loop by calling `run_event_loop`.
    /// On success, it is expected to terminate workers and the engine itself.
    /// On failure, it tries to recover by performing an engine hard reset.
    ///
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
                println!("{}", e);
                // TODO: Restart engine
            }
        };
    }

    /// Runs the engine's event loop.
    ///
    /// The event loop is an infinite loop over a `tokio::select!` with the following branches:
    /// - `self.from_main_rx.recv()`: listens to commands like pause/resume from the main thread.
    /// - `self.from_worker_rx.recv()`: listens to messages from download workers, typically responses to engine requests.
    /// - `worker_reuse_ticker.tick()`: ticker that adds new connections dynamically during the download.
    /// - `worker_reset_ticker.tick()`: ticker that resets hanging connections.
    /// - `download_progress_ticker.tick()`: ticker that polls download progress and performs calculations
    ///   such as speed and time remaining.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop encounters a failure during execution.
    /// Such failures are handled in `run_async` by performing an engine hard reset.
    async fn run_event_loop(&mut self) -> anyhow::Result<()> {
        self.state = EngineState::Running;

        let mut worker_reuse_ticker = interval(Duration::from_secs(WORKER_REUSE_TICKER_SECS));
        let mut worker_spawner_ticker = interval(Duration::from_secs(WORKER_SPAWNER_TICKER_SECS));
        let mut worker_reset_ticker = interval(Duration::from_secs(WORKER_RESET_TICKER_SECS));
        let mut download_progress_ticker = interval(Duration::from_millis(
            self.setting.progress_polling_frequency_millis,
        ));

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
                // _ = worker_reuse_ticker.tick() => self.run_worker_reuse_ticker()?,
                _ = worker_spawner_ticker.tick() => self.run_worker_spawner_ticker().await?,
                _ = worker_reset_ticker.tick() => self.run_worker_reset_ticker().await?,
                _ = download_progress_ticker.tick() => self.handle_progress_updates()?,
            }
        }
    }

    /// Polls the overall download progress and calculates overall download speed, estimated time
    /// remaining. At the end, it also calls `assemble_file` if assemble eligible
    /// (all workers finished their downloads)
    ///
    fn handle_progress_updates(&mut self) -> anyhow::Result<()> {
        let total_bytes_speed = self.calculate_total_speed()?;
        let readable_speed = self.speed_in_bytes_to_readable_string(total_bytes_speed);
        let is_temp_write_complete = self.check_temp_write_completion()?;
        self.progress.total_download_progress = self.calculate_total_progress()?;
        self.calculate_estimated_remaining(total_bytes_speed)?;
        println!(
            "Total download progress: {}",
            self.progress.total_download_progress
        );
        println!("Total Speed: {}", readable_speed);
        self.set_overall_download_status()?;
        if self.progress.total_download_progress > 1.0 {
            anyhow::bail!("Fatal! Overall progress exceeded 1")
        }
        if is_temp_write_complete && self.is_assemble_eligible() {
            self.assemble_file()?;
        }
        Ok(())
    }

    fn set_overall_download_status(&mut self) -> anyhow::Result<()> {
        let worker_progresses = self
            .workers
            .iter()
            .map(|w| w.1.progress_arc.lock_anyhow())
            .collect::<anyhow::Result<Vec<MutexGuard<WorkerProgress>>>>()?;

        let all_connecting = worker_progresses
            .iter()
            .all(|p| p.status == Status::Connecting);

        if all_connecting {
            self.progress.status = Status::Connecting;
            return Ok(());
        }

        let any_downloading = worker_progresses
            .iter()
            .any(|p| p.status == Status::Downloading);

        if any_downloading {
            self.progress.status = Status::Downloading;
        }

        Ok(())
    }

    fn speed_in_bytes_to_readable_string(&self, speed_bytes: u64) -> String {
        let speed_in_mega_bytes = speed_bytes as f64 / 1048576.0;
        let speed_in_kilo_bytes = speed_bytes as f64 / 1024.0;

        if speed_in_mega_bytes > 1.0 {
            format!("{:.2} MB/s", speed_in_mega_bytes)
        } else if speed_in_kilo_bytes > 1.0 {
            format!("{:.2} KB/s", speed_in_kilo_bytes)
        } else {
            format!("{:.2} B/s", speed_bytes)
        }
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
        if total_bytes > self.download_item.file_size {
            return Ok(());
        }
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
            .filter(|w| w.1.assigned_to_worker)
            .map(|w| w.1.progress_arc.lock_anyhow())
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(progress_vec)
    }

    fn check_temp_write_completion(&self) -> anyhow::Result<bool> {
        let all_complete = self.worker_progresses()?.iter().all(|x| {
            println!("Status for checktemp {:?}", x.status);
            x.status == Status::RangeComplete
        });
        if !all_complete {
            return Ok(false);
        }
        self.validate_temp_files_integrity(true, true, true)?;
        let missing_ranges = self.find_missing_byte_ranges()?;
        for range in &missing_ranges {
            println!("Missing range:: {}", range);
        }
        if missing_ranges.is_empty() && self.state == EngineState::WorkersComplete {
            return Ok(true);
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

    /// Sends a reset command to the workers that have not responded to the reset command in the
    /// last (self.setting.reset_timeout_millis) milliseconds.
    async fn run_worker_reset_ticker(&self) -> anyhow::Result<()> {
        let connections_to_reset = self.workers.iter().filter(|w| {
            let progress = w.1.progress_arc.lock().unwrap();
            w.1.assigned_to_worker
                && !matches!(
                    &progress.status,
                    Status::Stopped | Status::Starting | Status::Complete | Status::RangeComplete
                )
                && progress.last_response_time + (self.setting.reset_timeout_millis as u128)
                    < now_millis()
        });
        for worker in connections_to_reset {
            println!("Sent reset to worker {}", worker.0);
            worker.1.to_worker_tx.send(EngineToWorkerMsg::Reset).await?;
        }
        Ok(())
    }

    fn run_worker_reuse_ticker(&mut self) -> anyhow::Result<()> {
        if self.reuse_worker_queue.is_empty()
            || self.should_spawn_worker()
            || self.workers.iter().any(|w| w.1.awaiting_reset_response)
            || self.progress.total_download_progress >= 1f64
        {
            return Ok(());
        }

        let worker_num = self.reuse_worker_queue.pop_front().unwrap();
        self.request_byte_range_refresh_reuse_worker(worker_num)
    }

    fn request_byte_range_refresh_reuse_worker(&mut self, worker_num: u8) -> anyhow::Result<()> {
        let byte_range_tree = self.byte_range_tree.as_mut().unwrap();
        let worker_handle = unwrap_or_bail!(
            self.workers.get(&worker_num),
            "request_byte_range_refresh_reuse_worker:: Fatal! Failed to find worker_num {} in list of workers",
            worker_num
        );

        let in_queue_nodes =
            byte_range_tree.lowest_level_nodes_by_status(ByteRangeStatus::ToDownloadInQueue);
        let in_use_nodes =
            byte_range_tree.lowest_level_nodes_by_status(ByteRangeStatus::Downloading);

        let mut nodes = if !in_queue_nodes.is_empty() {
            in_queue_nodes
        } else {
            in_use_nodes
        };

        if nodes.is_empty() {
            self.reuse_worker_queue.push_back(worker_num);
            engine_warn!("request_byte_range_refresh_reuse_worker:: Failed to find segment node!")
        }

        nodes.sort_by(|a, b| a.borrow().range.cmp(&b.borrow().range));
        let target_node = nodes
            .iter()
            .find(|x| x.borrow().range != worker_handle.range);

        if target_node.is_none() {
            self.reuse_worker_queue.push_back(worker_num);
            engine_warn!("request_byte_range_refresh_reuse_worker:: Target node is none!")
        }
        let mut target_node_borrow = target_node.unwrap().borrow_mut();

        println!(
            "Splitting node worker_num::{} with range {}",
            worker_num, target_node_borrow.range
        );
        println!("Pre-split byte range tree:\n{}", byte_range_tree);
        println!("Splitting byte range node from engine");
        let split_result = byte_range_tree.split_byte_range_node(target_node.unwrap(), false);
        println!("Post-split byte range tree:\n{}", byte_range_tree);
        if split_result.is_err() {
            engine_warn!(
                "request_byte_range_refresh_reuse_worker:: Failed to split node worker_num {} with range {} \n Tree:\n{}",
                worker_num,
                target_node_borrow.range,
                byte_range_tree
            )
        }

        if let Some(right_child) = target_node_borrow.right_child.as_ref() {
            let mut borrow = right_child.borrow_mut();
            borrow.worker_number = worker_num;
            borrow.status = ByteRangeStatus::ToDownload;
        }

        target_node_borrow.status = ByteRangeStatus::RefreshRequested;

        if let Some(left_child) = target_node_borrow.left_child.as_ref() {
            left_child.borrow_mut().status = ByteRangeStatus::RefreshRequested;
        }

        self.workers
            .iter()
            .find(|w| w.1.range == target_node_borrow.range && !w.1.awaiting_reset_response);

        Ok(())
    }

    async fn run_worker_spawner_ticker(&mut self) -> anyhow::Result<()> {
        if self.should_spawn_worker() {
            self.request_byte_range_refresh_new_worker().await?
        }
        Ok(())
    }

    /// TODO: doc
    /// TODO: create custom error wrapper with fatal bool value and return that
    async fn request_byte_range_refresh_new_worker(&mut self) -> anyhow::Result<()> {
        if self.byte_range_tree.is_none() {
            return Ok(());
        }
        let byte_range_tree = self.byte_range_tree.as_mut().unwrap();
        println!("Pre-split byte range tree:\n{}", byte_range_tree);
        if let Err(e) = byte_range_tree.split() {
            engine_warn!(
                "request_byte_range_refresh_new_worker:: Failed to split tree {}",
                e
            )
        }
        println!("Post-split byte range tree:\n{}", byte_range_tree);
        println!("Refreshing worker ranges...");
        for (worker_number, handle) in &self.workers {
            if !handle.assigned_to_worker {
                continue;
            }
            println!("processing worker {}", worker_number);
            let related_node = byte_range_tree
                .lowest_level_nodes
                .iter()
                .find(|x| x.borrow().worker_number == *worker_number);

            if related_node.is_none() {
                engine_warn!("Fatal error occurred! relatedSegmentNode is null!")
            }
            let mut node = related_node.unwrap().borrow_mut();
            println!(
                "sending refresh segment {} to worker {}",
                worker_number,
                node.range.clone()
            );
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
        let pending_range_refresh_exists = self
            .byte_range_tree
            .as_ref()
            .unwrap()
            .lowest_level_nodes
            .iter()
            .any(|n| n.borrow().status == ByteRangeStatus::RefreshRequested);

        !pending_range_refresh_exists
            && self.progress.estimated_remaining_sec > 3
            && self.pending_worker_handshakes.is_empty()
            && self.progress.workers_progress.len() < (self.setting.total_connections as usize)
            && self.spawned_workers < self.setting.total_connections
            && self.progress.status != Status::Stopped
    }

    async fn pause_workers(&mut self) -> anyhow::Result<()> {
        // TODO: status validation
        for handle in &self.workers {
            let sender = &handle.1.to_worker_tx;
            sender.send(EngineToWorkerMsg::Stop).await?;
        }
        self.progress.status = Status::Stopped;
        Ok(())
    }

    async fn handle_worker_msg(&mut self, msg: WorkerToEngineMsg) -> anyhow::Result<()> {
        match msg.message {
            ToEngineMessage::Complete(range) => {
                self.handle_range_download_completion(msg.worker_number, range)?;
            }
            ToEngineMessage::ByteRangeRefreshSuccess {
                requested_range,
                reuse,
            } => {
                self.handle_refresh_byte_range_success(msg.worker_number, requested_range, reuse)
                    .await?
            }
            ToEngineMessage::ByteRangeRefreshRefused {
                requested_range,
                reuse,
            } => {
                self.handle_refresh_byte_range_refused(msg.worker_number, requested_range, reuse)
                    .await?;
            }
            ToEngineMessage::ByteRangeRefreshOverlapped {
                requested_range,
                refreshed_range,
                new_valid_range,
                reuse,
            } => {
                self.handle_refresh_byte_range_overlap(
                    msg.worker_number,
                    requested_range,
                    refreshed_range,
                    new_valid_range,
                    reuse,
                )
                .await?;
            }
            ToEngineMessage::Stopped => todo!(),
            ToEngineMessage::Failed => {}
            ToEngineMessage::HandshakeResponse { reuse } => {
                self.handle_worker_handshake(msg.worker_number, reuse);
            }
            ToEngineMessage::ConnectionSuccess => {
                self.handle_worker_connection_success(msg.worker_number)?;
            }
        }
        Ok(())
    }

    fn handle_worker_connection_success(&mut self, worker_number: u8) -> anyhow::Result<()> {
        if let Some(worker) = self.workers.get_mut(&worker_number) {
            worker.awaiting_reset_response = false;
            return Ok(());
        }

        anyhow::bail!(
            "handle_worker_connection_success:: Failed to find worker_num {} in list of workers",
            worker_number
        )
    }

    fn handle_worker_handshake(&mut self, worker_num: u8, reuse: bool) {
        self.pending_worker_handshakes.retain(|x| x != &worker_num);
        if reuse {
            let tree_ref = self.byte_range_tree.as_ref().unwrap();

            let reuse_request_nodes =
                tree_ref.lowest_level_nodes_by_status(ByteRangeStatus::ReuseRequested);

            let related_node = reuse_request_nodes
                .iter()
                .find(|x| x.borrow().worker_number == worker_num);

            if let Some(node) = related_node {
                node.borrow_mut().status = ByteRangeStatus::RefreshRequested;
            }
        }
    }

    fn handle_range_download_completion(
        &mut self,
        worker_num: u8,
        range: ByteRange,
    ) -> anyhow::Result<()> {
        println!("Handling completion #{} {}", worker_num, range);
        if self.reuse_worker_queue.contains(&worker_num) {
            self.reuse_worker_queue.push_back(worker_num);
        }
        let tree_ref = self.byte_range_tree.as_ref().unwrap();
        println!("Tree\n{}", tree_ref);
        let node = tree_ref.search_node(&range);
        if node.is_none() {
            println!(
                "handle_range_download_completion:: Failed to find node {} in tree for worker number {}",
                range, worker_num
            );
            let node = tree_ref.search_node(&range);
            anyhow::bail!(
                "handle_range_download_completion:: Failed to find node {} in tree for worker number {}",
                range,
                worker_num
            );
        }
        node.unwrap().borrow_mut().status = ByteRangeStatus::Complete;
        let all_ranges_complete = tree_ref
            .lowest_level_nodes
            .iter()
            .all(|n| n.borrow().status == ByteRangeStatus::Complete);
        if all_ranges_complete {
            self.state = EngineState::WorkersComplete;
        }
        Ok(())
    }

    async fn handle_refresh_byte_range_overlap(
        &mut self,
        worker_num: u8,
        requested_range: ByteRange,
        refreshed_range: ByteRange,
        new_valid_range: ByteRange,
        reuse: bool,
    ) -> anyhow::Result<()> {
        println!(
            "Handling overlap from worker {} with requested range {}",
            worker_num, requested_range
        );
        if self.byte_range_tree.is_none() {
            engine_warn!("handle_refresh_byte_range_overlap:: Byte range tree is empty")
        }

        let tree_ref = self.byte_range_tree.as_ref().unwrap();
        print!("{}", tree_ref);
        let node = tree_ref.search_node(&requested_range);
        if node.is_none() {
            engine_warn!("handle_refresh_byte_range_overlap:: Failed to find segment node.");
        }

        self.update_worker_range(worker_num, refreshed_range.clone())?;

        let parent_rc = node.unwrap().borrow().parent.clone().upgrade().unwrap();
        let left_child_rc;
        let right_child_rc;
        let new_worker_number;
        let new_worker_node;

        {
            let mut parent = parent_rc.borrow_mut();

            left_child_rc = parent.left_child.as_ref().unwrap().clone();
            right_child_rc = parent.right_child.as_ref().unwrap().clone();

            {
                let mut left_child = left_child_rc.borrow_mut();
                left_child.range = refreshed_range.clone();
                left_child.status = ByteRangeStatus::Downloading;
            }

            {
                let mut right_child = right_child_rc.borrow_mut();
                right_child.status = ByteRangeStatus::ReuseRequested;
                right_child.range = new_valid_range.clone();
                new_worker_number = right_child.worker_number;
                new_worker_node = right_child_rc.clone();
            }

            parent.status = ByteRangeStatus::Outdated;
        }

        if reuse {
            self.send_start_command_reuse_worker(new_worker_number, new_valid_range.clone())
                .await?;
            new_worker_node.borrow_mut().status = ByteRangeStatus::Downloading;
        } else {
            self.spawn_worker(new_worker_number, right_child_rc).await?;
        }
        println!(
            "Tree after spawn for worker {}\n{}",
            worker_num,
            self.byte_range_tree.as_ref().unwrap()
        );

        Ok(())
    }

    async fn handle_refresh_byte_range_refused(
        &mut self,
        worker_number: u8,
        requested_range: ByteRange,
        reuse: bool,
    ) -> anyhow::Result<()> {
        println!(
            "HandleRefreshByteRangeRefused coming from worker {}",
            worker_number
        );
        if self.byte_range_tree.is_none() {
            return Ok(());
        }
        let tree_ref = self.byte_range_tree.as_ref().unwrap();
        let node = tree_ref.search_node(requested_range);
        if node.is_none() {
            engine_warn!("handle_refresh_byte_range_refused:: Failed to find segment node.")
        }
        let parent_weak = node.unwrap().borrow().parent.clone();
        let parent_rc = parent_weak.upgrade().unwrap();
        let mut parent = parent_rc.borrow_mut();
        if reuse {
            self.reuse_worker_queue.push_back(worker_number);
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
            let tree = self.byte_range_tree.as_mut().unwrap();
            tree.lowest_level_nodes
                .insert(idx, parent_weak.upgrade().clone().unwrap());

            let left_child_range = parent.left_child.as_ref().unwrap().borrow().range.clone();
            let right_child_range = parent.right_child.as_ref().unwrap().borrow().range.clone();
            drop(parent);

            let l_idx = tree
                .lowest_level_nodes
                .iter()
                .position(|node| node.borrow().range == left_child_range);

            if let Some(idx) = l_idx {
                tree.lowest_level_nodes.remove(idx);
            } else {
                println!("failed to find left child node in lowest level nodes!");
            }

            let r_idx = tree
                .lowest_level_nodes
                .iter()
                .position(|node| node.borrow().range == right_child_range);

            if let Some(idx) = r_idx {
                tree.lowest_level_nodes.remove(idx);
            } else {
                println!("failed to find right child node in lowest level nodes!");
            }
        } else {
            println!("RefreshSegmentRefused:: Failed to find segment node to insert");
        }
        parent_rc.borrow_mut().remove_children();
        Ok(())
    }

    async fn handle_refresh_byte_range_success(
        &mut self,
        worker_num: u8,
        requested_range: ByteRange,
        reuse: bool,
    ) -> anyhow::Result<()> {
        if self.byte_range_tree.is_none() {
            return Ok(());
        }
        println!(
            "Handling refresh success from worker {} with requested range {}",
            worker_num, requested_range
        );
        let node = {
            let tree = self.byte_range_tree.as_ref().unwrap();
            print!("{}", tree);
            tree.search_node(&requested_range)
        };
        if node.is_none() {
            engine_warn!("handle_refresh_byte_range_success:: Failed to find segment node")
        }

        {
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
                let node_worker_num = worker_node_ref.worker_number;
                drop(worker_node_ref);
                self.spawn_worker(node_worker_num, worker_node).await?;
                self.pending_worker_handshakes.push(node_worker_num);
                worker_node_ref = parent.right_child.as_ref().unwrap().borrow_mut();
            }
            parent.left_child.as_ref().unwrap().borrow_mut().status = ByteRangeStatus::Downloading;
            worker_node_ref.status = ByteRangeStatus::Downloading;
        }
        println!(
            "Tree after spawn for worker {}\n{}",
            worker_num,
            self.byte_range_tree.as_ref().unwrap()
        );
        Ok(())
    }

    fn update_worker_range(&mut self, worker_num: u8, new_range: ByteRange) -> anyhow::Result<()> {
        if let Some(worker) = self.workers.get_mut(&worker_num) {
            worker.range = new_range;
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to find worker {} updating refreshed range",
                worker_num
            )
        }
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
            anyhow::bail!(
                "Failed to find worker {} when sending start command reuse worker",
                worker_num
            )
        }
    }

    /// Handles the start command for the download engine.
    ///
    /// On the initial start, it validates the integrity of temporary files to remove any corrupted files.
    /// It then builds the byte range tree based on missing byte ranges (those not yet downloaded).
    ///
    /// If the missing byte ranges are empty (i.e., this is a fresh download), the first download worker
    /// is spawned and assigned the entire byte range. Additional workers will be gradually spawned
    /// by `run_worker_spawner_ticker` if the file size justifies it.
    ///
    /// If the missing byte ranges are not empty (i.e., resuming a previous download), all allowed
    /// connections are created immediately.
    ///
    async fn handle_start(&mut self) -> anyhow::Result<()> {
        if self.byte_range_tree.is_none() {
            println!("Inside start");
            self.prepopulate_worker_handles();
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
            self.spawn_worker(0, node_ref).await?;
        } else {
            // TODO: handle resume not initial
        }
        Ok(())
    }

    /// Pre-assigns worker handles for a given number of workers before they are spawned.
    ///
    /// This allows sending messages to workers that have not yet spawned by the `worker_spawner_ticker`.
    /// For example, if a pause command arrives just before a worker is spawned, the message can
    /// still be queued on the appropriate channel and will be received as soon as the worker starts.
    fn prepopulate_worker_handles(&mut self) {
        for worker_num in 0..self.setting.total_connections {
            println!("Prepopulating worker #{}", worker_num);
            let (to_worker_tx, from_engine_rx) =
                tokio::sync::mpsc::channel::<EngineToWorkerMsg>(100);
            let progress = Arc::new(Mutex::new(WorkerProgress::new()));
            let worker_handle = DownloadWorkerHandle {
                assigned_to_worker: false,
                range: ByteRange::empty(),
                to_worker_tx: to_worker_tx.clone(),
                engine_to_worker_rx: Some(from_engine_rx),
                progress_arc: progress.clone(),
                awaiting_reset_response: false,
            };
            self.workers.insert(worker_num, worker_handle);
        }
    }

    async fn spawn_worker(&mut self, worker_num: u8, tree_node: NodeRef) -> anyhow::Result<()> {
        let handle = unwrap_or_bail!(
            self.workers.get_mut(&worker_num),
            "Failed to spawn worker #{}. Handle not found",
            worker_num
        );

        let engine_to_worker_rx = unwrap_or_bail!(
            handle.engine_to_worker_rx.take(),
            "Failed to spawn worker #{}. receiver not found",
            worker_num
        );

        let range = {
            let mut node = tree_node.borrow_mut();
            node.status = ByteRangeStatus::Downloading;
            node.range.clone()
        };

        let mut worker = HttpDownloadWorker::new(
            worker_num,
            self.setting.clone(),
            self.download_item.clone(),
            range.clone(),
            self.from_worker_tx.clone(),
            engine_to_worker_rx,
            handle.progress_arc.clone(),
        );

        handle.range = range;
        handle.assigned_to_worker = true;

        self.pending_worker_handshakes.push(worker_num);
        println!("Spawned worker #{}", worker_num);
        thread::spawn(move || worker.run());
        Ok(())
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
            if curr_file.end_byte > next_file.start_byte {
                println!(
                    "WTFFF:: {}-{}, {}-{}",
                    curr_file.start_byte,
                    curr_file.end_byte,
                    next_file.start_byte,
                    next_file.end_byte
                );
                for fs in &temp_files {
                    println!("#{}-{}-{}", fs.worker_number, fs.start_byte, fs.end_byte);
                }
            };
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
        !self.assemble_requested
    }

    fn assemble_file(&mut self) -> anyhow::Result<bool> {
        self.assemble_requested = true;
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
            self.state = EngineState::Complete;
            // TODO kill workers
        } else {
            println!("Assemble failed");
        }
        println!("File assembled successfully");
        // TODO: notify progress

        Ok(success)
    }
}
