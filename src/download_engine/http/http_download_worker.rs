use crate::download_engine::http::http_download_engine::{EngineToWorkerMsg, WorkerToEngineMsg};
use crate::download_engine::http::http_download_worker::Status::{Starting, Stopped};
use crate::download_engine::http::segment::byte_range::ByteRange;
use crate::download_engine::http::{ClientError, HttpClient};
use crate::download_engine::utils::now_millis;
use crate::download_engine::utils::shared_data::ReadHandle;
use crate::download_engine::{DownloadInfo, Runnable};
use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Frame};
use hyper::{Request, http};
use std::net::Incoming;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{Receiver, Sender};

const SPEED_CHECK_WINDOW_MILLIS: u32 = 1100;

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

/// The worker responsible for downloading a file using HTTP
pub struct HttpDownloadWorker {
    pub worker_number: u8,
    download_info: ReadHandle<DownloadInfo>,
    byte_range: ByteRange,
    data_buffer: Vec<Bytes>,
    speed_check_buffer: Vec<Bytes>,
    to_engine_tx: Sender<WorkerToEngineMsg>,
    from_engine_rx: Receiver<EngineToWorkerMsg>,
    status: Arc<Mutex<Status>>,
    last_speed_check_epoch_millis: u128,
    temp_bytes_recieved: u64,
}

impl HttpDownloadWorker {
    pub fn new(
        info: ReadHandle<DownloadInfo>,
        byte_range: ByteRange,
        to_engine_tx: Sender<WorkerToEngineMsg>,
        from_engine_rx: Receiver<EngineToWorkerMsg>,
        status: Arc<Mutex<Status>>,
    ) -> Self {
        HttpDownloadWorker {
            download_info: info,
            worker_number: 0,
            byte_range,
            data_buffer: vec![],
            speed_check_buffer: vec![],
            to_engine_tx,
            from_engine_rx,
            status,
            last_speed_check_epoch_millis: now_millis(),
            temp_bytes_recieved: 0,
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
            // loop {
            //     if let Ok(EngineToWorkerMsg::Start) = self.from_engine_rx.try_recv() {
            //         println!("Download start received.");
            //         self.stopped = false;
            //     }
            //     if (self.stopped) {
            //         continue;
            //     }
            //     if let Err(err) = self.start_download("winrar.zip").await {
            //         eprintln!("Download failed: {}", err);
            //     } else {
            //         println!("Download complete");
            //     }
            // }
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
        // let mut file = File::create(path).unwrap();
        // while let Some(frame) = body.frame().await {
        //     println!("Receiving frame!!");
        //     let frame = frame.unwrap(); // TODO remove unwrap
        //     if let Some(data) = frame.data_ref() {
        //         self.data_buffer.push(data.clone());
        //         let _ = file.write_all(data);
        //     }
        // }
        loop {
            tokio::select! {
                frame = body.frame() => {
                    match frame {
                        Some(Ok(chunk)) => {
                            self.process_chunk(chunk);
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
    fn process_chunk(&mut self, data: Frame<Bytes>) {
        let chunk = data.data_ref().unwrap().clone();
        *self.status.lock().unwrap() = Status::Downloading;
        self.temp_bytes_recieved += chunk.len() as u64;
        self.data_buffer.push(chunk.clone());
        println!("Received chunk of size: {}", data.data_ref().unwrap().len());
        self.calculate_speed(chunk);
    }

    fn calculate_speed(&mut self, chunk: Bytes) {
        self.speed_check_buffer.push(chunk);
        let now = now_millis();
        if self.last_speed_check_epoch_millis + SPEED_CHECK_WINDOW_MILLIS as u128 > now {
            return;
        }
        let time_check_before = self.last_speed_check_epoch_millis;
        self.last_speed_check_epoch_millis = now;
        let elapsed_sec = (now - time_check_before) as f64 / 1000.0;
        let total_len: u64 = self.speed_check_buffer.iter().map(|c| c.len() as u64).sum();
        
        if total_len == 0 || elapsed_sec < 0.001 {
            return;
        }
        
        let speed_MB = (total_len as f64 / 1048576.0) / elapsed_sec;
        let speed_KB = (total_len as f64 / 1024.0) / elapsed_sec;
        let speed_B = total_len as f64 / elapsed_sec;
        
        if speed_MB > 1.0 {
            println!("Speed {:.2} MB/s", speed_MB);
        } else if speed_KB > 1.0 {
            println!("Speed {:.2} KB/s", speed_KB);
        } else {
            println!("Speed {:.2} B/s", speed_B);
        }
        self.speed_check_buffer.clear();
    }

    pub fn build_request(&self) -> Result<Request<Empty<Bytes>>, http::Error> {
        let url = &self.download_info.read().url;
        let range_header = self.byte_range.to_header();
        Ok(Request::builder()
            .method("GET")
            .uri(url)
            .header("User-Agent", "rust-hyper/1.0") // TODO proper user agent
            .header(range_header.0, range_header.1)
            .body(Empty::<Bytes>::new())?)
    }
}
