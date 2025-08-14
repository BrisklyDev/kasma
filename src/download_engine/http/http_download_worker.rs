use crate::download_engine::errors::DownloadError;
use crate::download_engine::http::byte_range::ByteRange;
use crate::download_engine::http::http_download_engine::MINIMUM_DOWNLOADABLE_BYTE_RANGE_LEN;
use crate::download_engine::http::http_download_worker::Status::RangeComplete;
use crate::download_engine::http::message::EngineToWorkerMsg::RefreshByteRange;
use crate::download_engine::http::message::{
    EngineToWorkerMsg, ToEngineMessage, WorkerToEngineMsg,
};
use crate::download_engine::http::progress::WorkerProgress;
use crate::download_engine::http::{ClientError, HttpClient};
use crate::download_engine::setting::DownloadSetting;
use crate::download_engine::utils::file::{TempFileMetadata, list_files_in_dir};
use crate::download_engine::utils::now_millis;
use crate::download_engine::utils::sync_ext::MutexAnyhowExt;
use crate::download_engine::{DownloadItem, RunnableTask};
use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Frame};
use hyper::{Request, http};
use std::fs::{File, create_dir_all, remove_file};
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::vec;
use tokio::sync::mpsc::{Receiver, Sender};

const SPEED_CHECK_WINDOW_MILLIS: u32 = 1100;
const MIN_FLUSH: u64 = 64 * 1024; // 64 KB
const MAX_FLUSH: u64 = 8 * 1024 * 1024; // 8 MB

/// The worker is responsible for downloading a file using HTTP. Workers are spawned in their own
/// threads by the engine, and a single-threaded tokio runtime is spawned on their threads.
/// Workers communicate with the engine via the `to_engine_tx` and `from_engine_rx` to send and
/// receive messages respectively.
pub struct HttpDownloadWorker {
    pub worker_number: u8,
    client: HttpClient,
    setting: DownloadSetting,
    status_downloading: bool,
    download_info: DownloadItem,
    byte_range: ByteRange,
    data_buffer: Vec<Bytes>,
    speed_check_bytes: u64,
    to_engine_tx: Sender<WorkerToEngineMsg>,
    from_engine_rx: Receiver<EngineToWorkerMsg>,
    progress: Arc<Mutex<WorkerProgress>>,
    last_speed_check_epoch_millis: u128,
    temp_bytes_received: u64,
    buffer_flush_threshold: u64,
    prev_buffer_end_byte: u64,
    total_request_bytes_received: u64,
    total_bytes_received: u64,
    cached_temp_files: Vec<TempFileMetadata>,
    terminated_on_completion: bool,
}

impl HttpDownloadWorker {
    pub fn new(
        worker_number: u8,
        setting: DownloadSetting,
        info: DownloadItem,
        byte_range: ByteRange,
        to_engine_tx: Sender<WorkerToEngineMsg>,
        from_engine_rx: Receiver<EngineToWorkerMsg>,
        progress: Arc<Mutex<WorkerProgress>>,
    ) -> Self {
        HttpDownloadWorker {
            download_info: info,
            client: HttpClient::new(),
            status_downloading: false,
            worker_number,
            byte_range,
            data_buffer: vec![],
            speed_check_bytes: 0,
            to_engine_tx,
            from_engine_rx,
            progress,
            last_speed_check_epoch_millis: now_millis(),
            temp_bytes_received: 0,
            buffer_flush_threshold: 0,
            prev_buffer_end_byte: 0,
            total_request_bytes_received: 0,
            total_bytes_received: 0,
            cached_temp_files: vec![],
            terminated_on_completion: false,
            setting,
        }
    }
}

impl RunnableTask for HttpDownloadWorker {
    fn run(&mut self) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(self.run_async());
    }
}

impl HttpDownloadWorker {
    async fn run_async(&mut self) {
        let handshake = ToEngineMessage::HandshakeResponse { reuse: false };
        self.send_to_engine(handshake).await;
        let _ = self.start_download(false, false).await;
        self.run_event_loop().await;
    }

    async fn run_event_loop(&mut self) {
        loop {
            match self.from_engine_rx.recv().await {
                Some(EngineToWorkerMsg::Start) => {
                    let _ = self.start_download(false, false).await;
                }
                Some(EngineToWorkerMsg::Stop) => {
                    println!("#{} Cancel received", self.worker_number);
                    self.progress.lock().unwrap().status = Status::Stopped;
                }
                Some(EngineToWorkerMsg::Reset) => {
                    println!("#{} Reset received in event loop", self.worker_number);
                    let _ = self.start_download(false, true).await;
                }
                None => {
                    // happens when the sender is dropped. Can be used to break and cleanup
                }
                _ => {}
            }
        }
    }

    /// Starts the download process.
    ///
    /// The worker is initialized with proper values, and then a `tokio::select!` runs with two branches:
    ///
    /// - `self.from_engine_rx.recv()`: listens to messages coming from the engine.
    /// - `response = client.send(req)`: sends the HTTP request via hyper.
    ///
    /// The purpose of this initial `tokio::select!` is to handle scenarios such as:
    /// the request is sent but takes time due to network or server issues. While waiting,
    /// if a reset or pause command is received from the engine, it is processed immediately
    /// because the select is biased toward listening to engine messages first, allowing
    /// the download to terminate instantaneously without waiting for the network.
    ///
    /// If the request completes successfully without interruption, an infinite loop over another
    /// `tokio::select!` is run. This loop also listens to `self.from_engine_rx.recv()` for engine
    /// messages, while simultaneously processing the incoming data chunks from the network.
    ///
    /// The `reuse` parameter indicates whether this start is for connection reuse:
    /// when a worker finishes downloading and is assigned a new byte range.
    ///
    async fn start_download(&mut self, reuse: bool, reset: bool) -> Result<Status, DownloadError> {
        //TODO: pass reset to method
        println!(
            "#{} Starting download with range {}",
            self.worker_number, self.byte_range
        );
        if self.progress.lock().unwrap().status == Status::Starting
            || self.is_start_not_allowed(reuse, reset)
        {
            println!("#{} start not allowed", self.worker_number);
            return Err(DownloadError::InvalidCommand);
        }
        if let Err(e) = self.init().await {
            println!("Failed to initialize download: {}", e);
            self.progress.lock().unwrap().status = Status::Failed;
            return Err(DownloadError::Other(e.to_string()));
        }
        if reset {
            println!(
                "Resetting worker {} with range {}",
                self.worker_number, self.byte_range
            );
            self.reset_status();
        }
        if let Ok(EngineToWorkerMsg::Stop) = self.from_engine_rx.try_recv() {
            println!("Download cancelled before start.");
            self.progress.lock().unwrap().status = Status::Stopped;
            return Ok(Status::Stopped);
        }

        let req = self.build_request(reuse)?;
        tokio::select! {
            biased;

            Some(msg) = self.from_engine_rx.recv() => {
                match msg {
                    EngineToWorkerMsg::Stop => {
                        self.handle_stop_message();
                        Ok(Status::Stopped)
                    }
                    RefreshByteRange(new_range, reuse) => {
                        println!("Refreshed byte range in outer for worker {}", self.worker_number);
                        self.handle_refresh_byte_range_message(new_range, reuse).await;
                        self.set_reset_status();
                        Ok(Status::Resetting)
                    }
                    EngineToWorkerMsg::Reset => {
                        self.set_reset_status();
                        Ok(Status::Resetting)
                    }
                    EngineToWorkerMsg::Start => {
                        /// TODO fix
                        Ok(Status::Resetting)
                    },
                    EngineToWorkerMsg::StartReuseConnection(_) => {
                        /// TODO fix
                        Ok(Status::Resetting)
                    }
                }
            },

            response = self.client.send(req) => {
                match response {
                    Ok(resp) => {
                        let mut body = resp.into_body();

                        loop {
                            tokio::select! {
                                biased;

                                Some(msg) = self.from_engine_rx.recv() => {
                                    match msg {
                                        EngineToWorkerMsg::Stop => {
                                            self.handle_stop_message();
                                            return Ok(Status::Stopped);
                                        }
                                        RefreshByteRange(new_range, reuse) => {
                                            println!("Refreshed byte range in inner for worker {}", self.worker_number);
                                            self.handle_refresh_byte_range_message(new_range, reuse).await;
                                        }
                                        EngineToWorkerMsg::Reset => {
                                            self.set_reset_status();
                                            return Ok(Status::Resetting);
                                        },
                                        EngineToWorkerMsg::Start => {
                                            /// TODO fix
                                            return Ok(Status::Resetting);
                                        },
                                        EngineToWorkerMsg::StartReuseConnection(_) => {
                                            /// TODO fix
                                            return Ok(Status::Resetting);
                                        }
                                    }
                                },

                                frame = body.frame() => {
                                    match frame {
                                        Some(Ok(chunk)) => {
                                            match self.process_chunk(chunk).await {
                                                Ok(stop) if stop => return Ok(RangeComplete),
                                                Ok(_) => {}
                                                Err(_) => return Err(DownloadError::ProcessChunk),
                                            }
                                        }
                                        Some(Err(e)) => {
                                            return Err(DownloadError::Other(e.to_string()));
                                        }
                                        None => {
                                            println!("#{} inside None", self.worker_number);
                                            return self.handle_download_complete();
                                        }
                                    }
                                },
                            }
                        }
                    }
                    Err(e) => {
                        Err(DownloadError::Transport(e.to_string()))
                    }
                }
            }
        }
    }

    fn handle_download_complete(&mut self) -> Result<Status, DownloadError> {
        println!("Download finished.");
        match self.flush_buffer() {
            Ok(_) => {
                self.set_download_complete();
                Ok(RangeComplete)
            }
            Err(_) => Err(DownloadError::ProcessChunk),
        }
    }

    fn handle_stop_message(&mut self) {
        println!("Cancel message received from engine. Exiting download...");
        self.progress.lock().unwrap().status = Status::Stopped;
        self.status_downloading = false;
    }

    async fn handle_refresh_byte_range_message(&mut self, new_range: ByteRange, reuse: bool) {
        let result = self.refresh_byte_range(new_range, reuse);
        self.send_to_engine(result).await;
    }

    fn set_reset_status(&mut self) {
        println!("Reset message received from engine. Exiting download...");
        self.progress.lock().unwrap().status = Status::Resetting;
        self.status_downloading = false;
    }

    fn is_start_not_allowed(&self, reuse: bool, conn_reset: bool) -> bool {
        if self.byte_range.start >= self.byte_range.end
            || self.byte_range.start > self.download_info.file_size
            || self.byte_range.end > self.download_info.file_size
        {
            println!(
                "Invalid byte range: {}-{}. Skipping...",
                self.byte_range.start, self.byte_range.end
            );
            return true;
        }

        if reuse {
            return false;
        }
        matches!(
            self.progress.lock().unwrap().status,
            Status::Downloading | Status::Connecting | Status::Starting
        ) && !conn_reset
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        {
            self.progress.lock_anyhow()?.status = Status::Connecting;
        }
        self.terminated_on_completion = false;
        self.total_request_bytes_received = 0;
        self.status_downloading = false;
        create_dir_all(self.temp_directory())?;
        self.init_temp_files_cache();
        Ok(())
    }

    async fn send_to_engine(&mut self, message: ToEngineMessage) {
        let msg = WorkerToEngineMsg {
            worker_number: self.worker_number,
            message,
        };
        if let Err(e) = self.to_engine_tx.send(msg).await {
            println!("Failed to send message to engine: {}", e);
        }
    }

    fn refresh_byte_range(
        &mut self,
        new_range: ByteRange,
        reuse_connection: bool,
    ) -> ToEngineMessage {
        let prev_end_byte = self.byte_range.end;
        if self.progress.lock().unwrap().status == RangeComplete {
            return ToEngineMessage::ByteRangeRefreshRefused {
                requested_range: new_range,
                reuse: reuse_connection,
            };
        }
        if self.byte_range.start + self.total_request_bytes_received >= new_range.end {
            let split_byte = (self.byte_range.end
                - (self.byte_range.start + self.total_request_bytes_received))
                / 2;
            let new_end = split_byte + self.byte_range.start + self.total_request_bytes_received;
            let new_valid_end = prev_end_byte;
            let new_valid_start = self.byte_range.start;

            return if new_end > 0
                && new_range.start < new_end
                && new_valid_start + MINIMUM_DOWNLOADABLE_BYTE_RANGE_LEN < new_valid_end
            {
                self.byte_range = ByteRange::new(new_range.start, new_end);
                println!(
                    "#{} Byte range refreshed {}",
                    self.worker_number, self.byte_range
                );
                ToEngineMessage::ByteRangeRefreshOverlapped {
                    requested_range: new_range.clone(),
                    new_valid_range: ByteRange::new(self.byte_range.end + 1, prev_end_byte),
                    refreshed_range: self.byte_range.clone(),
                    reuse: reuse_connection,
                }
            } else {
                ToEngineMessage::ByteRangeRefreshRefused {
                    requested_range: new_range,
                    reuse: reuse_connection,
                }
            };
        }
        if new_range.start >= new_range.end || new_range.start + 1 >= new_range.end {
            return ToEngineMessage::ByteRangeRefreshRefused {
                requested_range: new_range,
                reuse: reuse_connection,
            };
        }

        self.byte_range = new_range;
        println!(
            "#{} Byte range refreshed {}",
            self.worker_number, self.byte_range
        );
        ToEngineMessage::ByteRangeRefreshSuccess {
            requested_range: self.byte_range.clone(),
            reuse: reuse_connection,
        }
    }

    /// Adds the received bytes to the buffer and flushes to disk periodically.
    /// Returns true if the byte range has been downloaded and should terminate the connection.
    /// TODO: Gracefully handle poisoned locks to send panic to the engine and recover
    async fn process_chunk(&mut self, data: Frame<Bytes>) -> anyhow::Result<bool> {
        if !self.status_downloading {
            self.progress.lock_anyhow()?.status = Status::Downloading;
            self.status_downloading = true;
            self.send_to_engine(ToEngineMessage::ConnectionSuccess)
                .await;
        }
        let chunk = data.data_ref().unwrap().clone();
        let chunk_size = chunk.len() as u64;
        self.update_received_bytes(chunk_size);
        self.calculate_speed(chunk_size);
        self.data_buffer.push(chunk);
        self.calculate_flush_threshold();
        self.update_download_progress();
        if self.download_exceeded_end_byte() {
            println!(
                "Exceeded end byte: Current range: {}-{}, total_req: {}",
                self.byte_range.start, self.byte_range.end, self.total_request_bytes_received
            );
            self.flush_buffer()?;
            self.cut_temp_files().await?;
            self.terminated_on_completion = true;
            self.set_download_complete();
            return Ok(true);
        }
        if self.download_match_end_byte() {
            println!("Matched endbyte. not doing anything");
        }
        if self.temp_bytes_received > self.buffer_flush_threshold {
            self.flush_buffer()?;
            self.set_download_complete();
        }
        Ok(false)
    }

    /// Cuts the excess bytes from the downloaded temp files.
    /// After the download has started, the engine might send a refresh byte range command which updates
    /// the workers' assigned byte range. Since there is no way to customize the chunk sizes received
    /// from the http client, we calculate and cut the excess bytes from the latest flushed buffer
    /// which exceeded the newly assigned range.
    /// TODO: add logging
    async fn cut_temp_files(&mut self) -> anyhow::Result<()> {
        println!("Cutting temp files...");
        let temp_files = self.temp_files_sorted(true);
        let mut to_delete: Vec<&TempFileMetadata> = vec![];
        let mut new_buf_start_byte: Option<u64> = None;
        let mut new_buf_to_write: Option<Vec<u8>> = None;
        for file_meta in &temp_files {
            if self.byte_range.end < file_meta.start_byte {
                println!(
                    "Temp file to delete: {} :: {}",
                    file_meta.name, file_meta.size
                );
                to_delete.push(file_meta);
                continue;
            }
            if self.byte_range.end < file_meta.end_byte {
                println!("File to cut {} :: {}", file_meta.name, file_meta.size);
                new_buf_start_byte = Some(file_meta.start_byte);
                let cut_len = self.byte_range.end - file_meta.start_byte + 1;
                println!("Cut len: {}", cut_len);
                let mut file = File::open(&file_meta.path)?;
                new_buf_to_write = Some(vec![0u8; cut_len as usize]);
                file.read_exact(new_buf_to_write.as_mut().unwrap())?;
                to_delete.push(file_meta);
            }
        }

        for to_delete_meta in &to_delete {
            self.total_bytes_received -= to_delete_meta.size;
            remove_file(&to_delete_meta.path)?;
            let pos = self
                .cached_temp_files
                .iter()
                .position(|f| f == *to_delete_meta);
            if let Some(pos) = pos {
                self.cached_temp_files.remove(pos);
            }
        }

        if let Some(buf_to_write) = new_buf_to_write {
            let new_start_byte = new_buf_start_byte.unwrap();
            let new_end_byte = new_start_byte as usize + buf_to_write.len() - 1;
            let filename = format!("{}#{}-{}", self.worker_number, new_start_byte, new_end_byte);
            let file_path = self.temp_directory().join(&filename);
            let mut file = File::create(&file_path)?;
            println!(
                "#{} New file writing with range {}-{}",
                self.worker_number, new_start_byte, new_end_byte
            );
            file.write_all(&buf_to_write)?;
            let file_meta = TempFileMetadata {
                name: filename.clone(),
                start_byte: new_start_byte,
                end_byte: new_end_byte as u64,
                worker_number: self.worker_number,
                size: buf_to_write.len() as u64,
                path: file_path.clone(),
            };
            self.cached_temp_files.push(file_meta);
            self.total_bytes_received += buf_to_write.len() as u64;
        }
        self.progress.lock_anyhow()?.total_download_progress =
            self.total_bytes_received as f64 / self.download_info.file_size as f64;

        println!("Temp file fix complete");
        Ok(())
    }

    fn update_received_bytes(&mut self, len: u64) {
        self.temp_bytes_received += len;
        self.total_request_bytes_received += len;
        self.total_bytes_received += len;
    }

    fn calculate_speed(&mut self, len: u64) {
        self.speed_check_bytes += len;
        let total_len: u64 = self.speed_check_bytes;
        let now = now_millis();
        if self.last_speed_check_epoch_millis + SPEED_CHECK_WINDOW_MILLIS as u128 > now {
            return;
        }
        let time_check_before = self.last_speed_check_epoch_millis;
        self.last_speed_check_epoch_millis = now;
        let elapsed_sec = (now - time_check_before) as f64 / 1000.0;

        if total_len == 0 || elapsed_sec < 0.001 {
            return;
        }

        let speed_mb = (total_len as f64 / 1048576.0) / elapsed_sec;
        let speed_kb = (total_len as f64 / 1024.0) / elapsed_sec;
        let speed_b = total_len as f64 / elapsed_sec;

        if speed_mb > 1.0 {
            println!("Speed {:.2} MB/s", speed_mb);
        } else if speed_kb > 1.0 {
            println!("Speed {:.2} KB/s", speed_kb);
        } else {
            println!("Speed {:.2} B/s", speed_b);
        }
        self.speed_check_bytes = 0;
        self.progress.lock().unwrap().speed_bytes_per_sec = speed_b as u64;
    }

    fn calculate_flush_threshold(&mut self) {
        self.buffer_flush_threshold = self
            .progress
            .lock()
            .unwrap()
            .speed_bytes_per_sec
            .saturating_mul(2)
            .clamp(MIN_FLUSH, MAX_FLUSH);
    }

    fn update_download_progress(&self) {
        let mut progress = self.progress.lock().unwrap();
        progress.worker_download_progress =
            self.total_request_bytes_received as f64 / self.byte_range.len() as f64;
        progress.total_download_progress =
            self.total_bytes_received as f64 / self.download_info.file_size as f64;
        if progress.worker_download_progress > 1.0 {
            let excess_bytes = self.total_request_bytes_received - self.byte_range.len();
            progress.total_download_progress = (self.total_bytes_received as f64
                - excess_bytes as f64)
                / self.download_info.file_size as f64;
        }
        progress.total_bytes_received = self.total_bytes_received;
        progress.last_response_time = now_millis();
    }

    fn flush_buffer(&mut self) -> anyhow::Result<()> {
        if self.data_buffer.is_empty() {
            return Ok(());
        }
        let temp_file_name = format!(
            "{}#{}-{}",
            self.worker_number,
            self.temp_file_start_byte(),
            self.temp_file_end_byte(),
        );
        let file_path = self.temp_directory().join(&temp_file_name);
        let mut file = File::create(&file_path)?;
        let mut temp_file_len: u64 = 0;
        for chunk in &self.data_buffer {
            file.write_all(chunk)?;
            temp_file_len += chunk.len() as u64;
        }
        // if tempFileStartByte > downloadItem.fileSize {
        //     _sendEnginePanic();
        // }
        let file_meta = TempFileMetadata {
            name: temp_file_name,
            start_byte: self.temp_file_start_byte(),
            end_byte: self.temp_file_end_byte(),
            worker_number: self.worker_number,
            size: temp_file_len,
            path: file_path.clone(),
        };
        println!("Flushed buffer {}", file_meta.name);
        self.cached_temp_files.push(file_meta);
        self.prev_buffer_end_byte += temp_file_len;
        self.reset_data_buffer();
        Ok(())
    }

    fn resolve_range(&self, reuse: bool) -> ByteRange {
        let req_start_byte = if reuse {
            self.byte_range.start
        } else {
            self.new_start_byte()
        };
        println!(
            "New startbyte: {} end: {}",
            req_start_byte, self.byte_range.end
        );
        ByteRange::new(req_start_byte, self.byte_range.end)
    }

    /// Returns the next start byte to download. This is used for the pause/resume mechanism so that
    /// the next range is downloaded
    fn new_start_byte(&self) -> u64 {
        let files = self.temp_files_sorted(true);
        if files.is_empty() {
            return self.byte_range.start;
        }
        files.last().unwrap().end_byte + 1
    }

    /// Adds the temp files downloaded by this worker to its cache.
    fn init_temp_files_cache(&mut self) {
        if self.cached_temp_files.is_empty() || !self.temp_directory().exists() {
            return;
        }
        let files = list_files_in_dir(self.temp_directory()).unwrap();
        self.cached_temp_files = files
            .iter()
            .map(TempFileMetadata::from_path_buf)
            .filter(|f| f.worker_number == self.worker_number)
            .collect();
    }

    fn temp_files_sorted(&self, this_range_only: bool) -> Vec<TempFileMetadata> {
        if self.cached_temp_files.is_empty() {
            return vec![];
        }
        if !this_range_only {
            let mut cloned_files = self.cached_temp_files.clone();
            cloned_files.sort_by(|a, b| a.start_byte.cmp(&b.start_byte));
            return cloned_files;
        }

        let mut in_range: Vec<TempFileMetadata> = self
            .cached_temp_files
            .iter()
            .filter(|x| x.is_in_range(self.byte_range.clone()))
            .cloned()
            .collect();

        in_range.sort_by(|a, b| a.start_byte.cmp(&b.start_byte));
        in_range
    }

    pub fn build_request(&mut self, reuse: bool) -> Result<Request<Empty<Bytes>>, http::Error> {
        let url = &self.download_info.url;
        let request_range = self.resolve_range(reuse);
        let range_header = request_range.to_header();
        let req = Request::builder()
            .method("GET")
            .uri(url)
            .header("User-Agent", "rust-hyper/1.0") // TODO: proper user agent
            .header(range_header.0, range_header.1)
            .body(Empty::<Bytes>::new())?;

        let total_existing_len = self.total_written_bytes(false);
        let total_req_received_bytes = self.total_written_bytes(true);
        let mut progress = self.progress.lock().unwrap();
        progress.worker_download_progress =
            (total_req_received_bytes / self.byte_range.len()) as f64;

        self.total_bytes_received = total_existing_len;
        self.total_request_bytes_received = total_req_received_bytes;
        progress.total_bytes_received = self.total_bytes_received;
        progress.total_download_progress =
            (self.total_bytes_received / self.download_info.file_size) as f64;

        if request_range.start == self.byte_range.start {
            self.prev_buffer_end_byte = 0;
            println!("Request range was == with start");
        } else {
            self.prev_buffer_end_byte = request_range.start - self.byte_range.start;
            println!("wasn't, prev: {}", self.prev_buffer_end_byte);
        }

        Ok(req)
    }

    fn reset_status(&mut self) {
        {
            let mut progress = self.progress.lock().unwrap();
            progress.worker_download_progress = 0f64;
            progress.last_response_time = now_millis();
        }
        self.total_request_bytes_received = 0;
        self.prev_buffer_end_byte = 0;
        self.data_buffer.clear();
        self.reset_data_buffer();
    }

    fn download_exceeded_end_byte(&self) -> bool {
        self.byte_range.start + self.total_request_bytes_received + 1 > self.byte_range.end
    }

    fn download_match_end_byte(&self) -> bool {
        self.byte_range.start + self.total_request_bytes_received + 1 == self.byte_range.end
    }

    fn temp_directory(&self) -> PathBuf {
        Path::new(&self.setting.base_temp_dir).join(self.download_info.uid.clone())
    }

    fn temp_file_start_byte(&self) -> u64 {
        self.byte_range.start + self.prev_buffer_end_byte
    }

    fn temp_file_end_byte(&self) -> u64 {
        self.byte_range.start + self.prev_buffer_end_byte + self.temp_bytes_received - 1
    }

    fn reset_data_buffer(&mut self) {
        self.data_buffer.clear();
        self.temp_bytes_received = 0;
    }

    fn set_download_complete(&mut self) {
        let mut progress = self.progress.lock().unwrap();
        progress.status = Status::RangeComplete;
        drop(progress);

        let mut progress = self.progress.lock().unwrap();
        progress.worker_download_progress = 1f64;
        let total_bytes = self.total_written_bytes(false);
        self.status_downloading = false;
        progress.total_download_progress = total_bytes as f64 / self.download_info.file_size as f64;
    }

    fn total_written_bytes(&self, this_range_only: bool) -> u64 {
        if self.cached_temp_files.is_empty() {
            return 0;
        }
        let temp_files = self.temp_files_sorted(this_range_only);
        if temp_files.is_empty() {
            return 0;
        }
        temp_files.iter().map(|f| f.size).sum()
    }
}

/// Increments the retry count based on an exponential backoff and sleeps for the specified time.
/// `retry_backoff` is used so that we can reset the backoff when it reaches 16. This is to prevent
/// unreasonably waiting for a prolonged amount of time before retrying again. This is because
/// sometimes some connections might get stuck and multiple retries are required.
async fn increment_retry_and_wait(retry_count: &mut u32, retry_backoff: &mut u32) {
    *retry_count += 1;
    *retry_backoff += 1;
    let mut delay_secs = 2u32.saturating_pow(*retry_backoff);
    if delay_secs > 16 {
        *retry_backoff = 1;
        delay_secs = 2;
    }
    tokio::time::sleep(Duration::from_secs(delay_secs as u64)).await;
}

/// Initial: The worker has not yet started a download
/// Stopped: Download is paused
/// Complete: The total download is complete
/// RangeComplete: The designated range has been fully downloaded
/// Resetting: The connection is being reset
/// Starting: The download is starting
/// Connecting: Connecting to server (no data has been received yet)
/// Failed: Download has failed
#[derive(PartialEq, Debug)]
pub enum Status {
    Initial,
    Stopped,
    Complete,
    RangeComplete,
    Resetting,
    Downloading,
    Starting,
    Connecting,
    Failed,
}
