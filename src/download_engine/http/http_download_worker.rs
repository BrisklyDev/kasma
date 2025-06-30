use crate::download_engine::DownloadInfo;
use crate::download_engine::download_worker::DownloadWorker;
use crate::download_engine::http::http_download_engine::{EngineToWorkerMsg, WorkerToEngineMsg};
use crate::download_engine::http::segment::byte_range::ByteRange;
use crate::download_engine::http::{ClientError, HttpClient};
use crate::download_engine::utils::shared_data::ReadHandle;
use http_body_util::{BodyExt, Empty};
use hyper::Request;
use hyper::body::Bytes;
use std::thread::JoinHandle;
use tokio::sync::mpsc::{Receiver, Sender};

pub enum HttpDownloadWorkerCommand {
    Start,
    Pause,
    Cancel,
}

/// The worker responsible for downloading a file using HTTP
pub struct HttpDownloadWorker {
    pub worker_number: u8,
    download_info: ReadHandle<DownloadInfo>,
    byte_range: ByteRange,
    data_buffer: Vec<Bytes>,
    to_engine_tx: Sender<WorkerToEngineMsg>,
    from_engine_rx: Receiver<EngineToWorkerMsg>,
}

impl HttpDownloadWorker {
    pub fn new(
        info: ReadHandle<DownloadInfo>,
        byte_range: ByteRange,
        to_engine_tx: Sender<WorkerToEngineMsg>,
        from_engine_rx: Receiver<EngineToWorkerMsg>,
    ) -> Self {
        HttpDownloadWorker {
            download_info: info,
            worker_number: 0,
            byte_range,
            data_buffer: vec![],
            to_engine_tx,
            from_engine_rx,
        }
    }
}

impl DownloadWorker for HttpDownloadWorker {
    fn spawn_worker_thread(&self) -> JoinHandle<()> {
        todo!()
    }

    fn start(&mut self) {
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
        });
    }

    fn pause(&self) {
        self.cancel();
    }

    fn cancel(&self) {
        // println!("Cancelling download...");
        // // self.cancel_tx
        //     .send(true)
        //     .expect("Failed to send cancellation signal");
    }
}

impl HttpDownloadWorker {
    async fn start_download(&mut self, path: &str) -> Result<(), ClientError> {
        let url = &self.download_info.read().url;
        println!("Downloading {} using hyper...", url);

        let client = HttpClient::new();
        let req = Request::builder()
            .method("GET")
            .uri(url)
            .header("User-Agent", "rust-hyper/1.0") // TODO proper user agent
            .body(Empty::<Bytes>::new())?;

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
                chunk = body.frame() => {
                    match chunk {
                        Some(Ok(data)) => {
                            println!("Received chunk of size: {}", data.data_ref().unwrap().len());
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
                        EngineToWorkerMsg::Cancel => {
                            println!("Cancel message received from engine. Exiting download...");
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
}
