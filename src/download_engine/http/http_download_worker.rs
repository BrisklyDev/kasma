use crate::download_engine::http::http_download_engine::EngineToWorkerMsg;
use crate::download_engine::http::http_download_engine::EngineToWorkerMsg::Stop;
use crate::download_engine::http::http_download_worker::Status::{Complete, Starting, Stopped};
use crate::download_engine::http::segment::byte_range::ByteRange;
use crate::download_engine::http::{ClientError, HttpClient};
use crate::download_engine::utils::{TempFileMetadata, list_files_in_dir, now_millis};
use crate::download_engine::{DownloadInfo, Runnable};
use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Frame};
use hyper::{Request, http};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::fs::{File, create_dir_all};
use tokio::io::AsyncWriteExt;
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
    download_info: DownloadInfo,
    byte_range: ByteRange,
    data_buffer: Vec<Bytes>,
    speed_check_bytes: u64,
    to_engine_tx: Sender<WorkerToEngineMsg>,
    from_engine_rx: Receiver<EngineToWorkerMsg>,
    status: Arc<Mutex<Status>>,
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
        info: DownloadInfo,
        byte_range: ByteRange,
        to_engine_tx: Sender<WorkerToEngineMsg>,
        from_engine_rx: Receiver<EngineToWorkerMsg>,
        status: Arc<Mutex<Status>>,
        progress: Arc<Mutex<WorkerProgress>>,
    ) -> Self {
        HttpDownloadWorker {
            download_info: info,
            worker_number: 0,
            byte_range,
            data_buffer: vec![],
            speed_check_bytes: 0,
            to_engine_tx,
            from_engine_rx,
            status,
            progress,
            last_speed_check_epoch_millis: now_millis(),
            temp_bytes_received: 0,
            buffer_flush_threshold: 0,
            prev_buffer_end_byte: 0,
            total_request_bytes_received: 0,
            total_bytes_received: 0,
            cached_temp_files: vec![],
            terminated_on_completion: false,
        }
    }
}

impl Runnable for HttpDownloadWorker {
    fn run(&mut self) {
        println!("Spawned download thread");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async move {
            self.try_download(false).await;
            loop {
                match self.from_engine_rx.recv().await {
                    Some(EngineToWorkerMsg::Start) => {
                        self.try_download(false).await;
                    }
                    Some(EngineToWorkerMsg::Stop) => {
                        println!("Cancel received");
                        *self.status.lock().unwrap() = Stopped;
                    }
                    None => {
                        // happens when the sender is dropped. Can be used to break and cleanup
                    }
                    _ => {}
                }
            }
        });
    }
}

impl HttpDownloadWorker {
    /// Tries downloading the file and sends the proper messages to the engine
    async fn try_download(&mut self, reuse: bool) {
        match self.start_download(reuse).await {
            Ok(Complete) => self.send_to_engine(ToEngineMessage::Completed).await,
            Ok(Stopped) => self.send_to_engine(ToEngineMessage::Stopped).await,
            Err(_) => self.send_to_engine(ToEngineMessage::Failed).await,
            _ => {}
        }
    }

    /// Starts the download and retries if failed.
    /// The `from_engine_rx` is listened for any actions necessary inside the loop. The loop uses a
    /// `tokio:select` macro between the received chunk and messages from the engine to take actions
    /// based on those messages, e.g., to stop the download.
    /// `reuse` indicates if this start method is called with a connection reuse intention.
    /// By Connection reuse we essentially mean when a worker has finished its download and is assigned
    /// a new byte range to download.
    /// TODO: Reuse the client
    async fn start_download(&mut self, reuse: bool) -> Result<Status, DownloadError> {
        let client = HttpClient::new();
        if *self.status.lock().unwrap() == Starting {
            return Err(DownloadError::InvalidCommand);
        }
        let req = self.build_request(reuse)?;
        if let Err(e) = self.init().await {
            println!("Failed to initialize download: {}", e);
            *self.status.lock().unwrap() = Status::Failed;
            return Err(DownloadError::Other(e.to_string()));
        }
        if let Ok(Stop) = self.from_engine_rx.try_recv() {
            println!("Download cancelled before start.");
            *self.status.lock().unwrap() = Stopped;
            return Ok(Stopped);
        }
        match client.send(req).await {
            Ok(resp) => {
                let mut body = resp.into_body();
                loop {
                    tokio::select! {
                        frame = body.frame() => {
                            match frame {
                                Some(Ok(chunk)) => {
                                    self.process_chunk(chunk).await;
                                }
                                Some(Err(e)) => {
                                    return Err(DownloadError::Other(e.to_string()))
                                }
                                None => {
                                    println!("Download finished.");
                                    self.set_download_complete();
                                    return Ok(Complete);
                                }
                            }
                        }
                        Some(msg) = self.from_engine_rx.recv() => {
                            match msg {
                                Stop => {
                                    println!("Cancel message received from engine. Exiting download...");
                                    *self.status.lock().unwrap() = Stopped;
                                    return Ok(Stopped);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Err(e) => Err(DownloadError::Transport(e.to_string())),
        }
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
            *self.status.lock().unwrap(),
            Status::Downloading | Status::Connecting | Status::Starting
        ) && !conn_reset
    }

    async fn init(&mut self) -> Result<(), std::io::Error> {
        {
            *self.status.lock().unwrap() = Status::Connecting;
        }
        self.terminated_on_completion = false;
        self.total_request_bytes_received = 0;
        create_dir_all(self.temp_directory()).await?;
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

    /// Adds the received bytes to the buffer and flushes to disk periodically.
    /// Returns true of the download should terminate on this process cycle.
    async fn process_chunk(&mut self, data: Frame<Bytes>) -> bool {
        {
            *self.status.lock().unwrap() = Status::Downloading;
        }
        let chunk = data.data_ref().unwrap().clone();
        let chunk_size = chunk.len() as u64;
        self.update_received_bytes(chunk_size);
        self.calculate_speed(chunk_size);
        self.data_buffer.push(chunk);
        self.calculate_flush_threshold();
        self.update_download_progress();
        // if receivedbytes exceed end byte
        if self.download_exceeded_end_byte() {
            self.flush_buffer().await;

            return true;
        }
        if self.temp_bytes_received > self.buffer_flush_threshold {
            self.flush_buffer().await;
        }
        false
    }

    /// Cuts the excess bytes from the downloaded temp files.
    /// After the download has started, the engine might send a refresh byte range command which updates
    /// the workers' assigned byte range. Since there is no way to customize the chunk sizes received
    /// from the http client, we calculate and cut the excess bytes from the latest flushed buffer
    /// which exceeded the newly assigned range.
    fn cut_temp_files(&self) {
       println!("Cutting temp files...");
        let temp_files = self.temp_files_sorted(true);
        // TODO add logging
        let to_delete = vec!<>[];
        
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
    }

    async fn flush_buffer(&mut self) {
        if self.data_buffer.is_empty() {
            return;
        }
        let temp_file_name = format!(
            "{}#{}-{}",
            self.worker_number,
            self.temp_file_start_byte(),
            self.temp_file_end_byte(),
        );
        let file_path = self.temp_directory().join(&temp_file_name);
        let mut file = File::create(&file_path).await.unwrap();
        let mut temp_file_len: u64 = 0;
        for chunk in &self.data_buffer {
            file.write_all(chunk).await.unwrap();
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
        };
        println!("Flushed buffer {}", file_meta.name);
        self.cached_temp_files.push(file_meta);
        self.total_request_bytes_received += temp_file_len;
        self.prev_buffer_end_byte += temp_file_len;
        self.reset_data_buffer();
    }

    fn resolve_range(&self, reuse: bool) -> ByteRange {
        let req_start_byte = if reuse {
            self.byte_range.start
        } else {
            self.new_start_byte()
        };
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
            .map(|p| TempFileMetadata::from_path_buf(p))
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
            .cloned()
            .filter(|x| x.is_in_range(self.byte_range.clone()))
            .collect();

        in_range.sort_by(|a, b| a.start_byte.cmp(&b.start_byte));
        in_range
    }

    pub fn build_request(&self, reuse: bool) -> Result<Request<Empty<Bytes>>, http::Error> {
        let url = &self.download_info.url;
        let range_header = self.resolve_range(reuse).to_header();
        Ok(Request::builder()
            .method("GET")
            .uri(url)
            .header("User-Agent", "rust-hyper/1.0") // TODO proper user agent
            .header(range_header.0, range_header.1)
            .body(Empty::<Bytes>::new())?)
    }

    fn download_exceeded_end_byte(&self) -> bool {
        self.byte_range.start + self.total_request_bytes_received + 1 > self.byte_range.end
    }

    fn temp_directory(&self) -> PathBuf {
        Path::new("C:\\Users\\RyeWell\\Desktop\\asda").join(self.download_info.uid.clone())
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

    fn set_download_complete(&self) {
        *self.status.lock().unwrap() = Status::Complete;
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

#[derive(Debug)]
pub struct WorkerToEngineMsg {
    worker_number: u8,
    message: ToEngineMessage,
}

#[derive(Debug)]
pub enum ToEngineMessage {
    Completed,
    Stopped,
    Failed,
}

#[derive(PartialEq)]
pub enum Status {
    Initial,
    Stopped,
    Complete,
    Downloading,
    Starting,
    Connecting,
    Failed,
}

pub enum DownloadError {
    Transport(String),
    Other(String),
    InvalidCommand,
}

impl From<ClientError> for DownloadError {
    fn from(err: ClientError) -> Self {
        match err {
            ClientError::Transport(transport_err) => {
                DownloadError::Transport(transport_err.to_string())
            }
            ClientError::Other(other_err) => DownloadError::Other(other_err),
        }
    }
}

impl From<hyper::http::Error> for DownloadError {
    fn from(err: hyper::http::Error) -> Self {
        DownloadError::Other(err.to_string())
    }
}

pub struct WorkerProgress {
    pub speed_bytes_per_sec: u64,
    pub worker_download_progress: f64,
    pub total_download_progress: f64,
}

impl WorkerProgress {
    pub(crate) fn new() -> Self {
        WorkerProgress {
            speed_bytes_per_sec: 0,
            worker_download_progress: 0.0,
            total_download_progress: 0.0,
        }
    }
}
