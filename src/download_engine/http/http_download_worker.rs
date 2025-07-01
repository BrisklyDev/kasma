use crate::download_engine::http::http_download_engine::{EngineToWorkerMsg, WorkerToEngineMsg};
use crate::download_engine::http::http_download_worker::Status::{Starting, Stopped};
use crate::download_engine::http::segment::byte_range::ByteRange;
use crate::download_engine::http::{ClientError, HttpClient};
use crate::download_engine::utils::{TempFileMetadata, now_millis};
use crate::download_engine::{DownloadInfo, Runnable};
use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Frame};
use hyper::{Request, http};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{Receiver, Sender};

const SPEED_CHECK_WINDOW_MILLIS: u32 = 1100;
const MIN_FLUSH: u64 = 64 * 1024; // 64 KB
const MAX_FLUSH: u64 = 8 * 1024 * 1024; // 8 MB

pub enum HttpDownloadWorkerCommand {
    Start,
    Stop,
}

#[derive(PartialEq)]
pub enum Status {
    Initial,
    Stopped,
    Downloading,
    Starting,
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

/// The worker responsible for downloading a file using HTTP
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
            if let Err(err) = self.start_download("winrar.zip").await {
                eprintln!("Download failed: {}", err);
            } else {
                println!("Download complete");
            }
            loop {
                match self.from_engine_rx.recv().await {
                    Some(EngineToWorkerMsg::Start) => {
                        let mut status_gaurd = self.status.lock().unwrap();
                        if *status_gaurd == Starting {
                            drop(status_gaurd);
                            continue;
                        }
                        *status_gaurd = Starting;
                        drop(status_gaurd);
                        if let Err(err) = self.start_download("winrar.zip").await {
                            eprintln!("Download failed: {}", err);
                        } else {
                            println!("Download complete");
                        }
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
    async fn start_download(&mut self, path: &str) -> Result<(), ClientError> {
        let client = HttpClient::new();
        let req = self.build_request()?;
        if let Ok(EngineToWorkerMsg::Stop) = self.from_engine_rx.try_recv() {
            println!("Download cancelled before start.");
            return Ok(());
        }
        let resp = client.send(req).await?;
        let mut body = resp.into_body();
        loop {
            tokio::select! {
                frame = body.frame() => {
                    match frame {
                        Some(Ok(chunk)) => {
                            self.process_chunk(chunk).await;
                        }
                        Some(Err(e)) => {
                            eprintln!("Error while reading: {}", e);
                            break;
                        }
                        None => {
                            println!("Download finished.");
                            break;
                        }
                    }
                }
                Some(msg) = self.from_engine_rx.recv() => {
                    match msg {
                        EngineToWorkerMsg::Stop => {
                            println!("Cancel message received from engine. Exiting download...");
                            *self.status.lock().unwrap() = Status::Stopped;
                            break;
                        }
                        // Handle other messages if needed
                        _ => {}
                    }
                }

            }
        }
        Ok(())
    }

    /// Adds the received bytes to the buffer and flushes to disk periodically
    async fn process_chunk(&mut self, data: Frame<Bytes>) {
        let chunk = data.data_ref().unwrap().clone();
        *self.status.lock().unwrap() = Status::Downloading;
        let chunk_size = chunk.len() as u64;
        println!("Received chunk of size: {}", data.data_ref().unwrap().len());
        self.update_received_bytes(chunk_size);
        self.calculate_speed(chunk_size);
        self.data_buffer.push(chunk);
        self.calculate_flush_threshold();
        self.update_download_progress();
        // if receivedbytes exceed end byte
        if self.temp_bytes_received > self.buffer_flush_threshold {
            self.flush_buffer().await;
        }
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
        std::fs::create_dir_all(&file_path.parent().unwrap()).unwrap();
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
        self.cached_temp_files.push(file_meta);
        self.total_request_bytes_received += temp_file_len;
        self.prev_buffer_end_byte += temp_file_len;
        self.reset_data_buffer();
    }

    pub fn build_request(&self) -> Result<Request<Empty<Bytes>>, http::Error> {
        let url = &self.download_info.url;
        let range_header = self.byte_range.to_header();
        Ok(Request::builder()
            .method("GET")
            .uri(url)
            .header("User-Agent", "rust-hyper/1.0") // TODO proper user agent
            .header(range_header.0, range_header.1)
            .body(Empty::<Bytes>::new())?)
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
}
