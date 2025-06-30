use crate::download_engine::DownloadInfo;
use crate::download_engine::download_worker::DownloadWorker;
use crate::download_engine::http::segment::byte_range::ByteRange;
use crate::download_engine::http::{ClientError, HttpClient};
use crate::download_engine::utils::shared_data::{CachedData, ReadHandle};
use http_body_util::{BodyExt, Empty};
use hyper::Request;
use hyper::body::Bytes;
use std::fs::File;
use std::io::Write;
use std::thread;
use std::thread::JoinHandle;

/// The worker responsible for downloading a file using HTTP
pub struct HttpDownloadWorker<'a> {
    download_info: ReadHandle<DownloadInfo>,
    worker_number: u8,
    byte_range: CachedData<'a, ByteRange>,
}

impl<'a> HttpDownloadWorker<'a> {
    pub fn new(info: ReadHandle<DownloadInfo>, byte_range: &'a ByteRange) -> Self {
        HttpDownloadWorker {
            download_info: info,
            worker_number: 0,
            byte_range: CachedData::new(byte_range),
        }
    }
}

impl<'a> DownloadWorker for HttpDownloadWorker<'a> {
    fn spawn_worker_thread(&self) -> JoinHandle<()> {
        todo!()
    }
    // fn spawn_worker_thread(self) -> thread::JoinHandle<()> {
    //     thread::spawn(move || {
    //         println!("Spawned download thread");
    //         let rt = tokio::runtime::Builder::new_current_thread()
    //             .enable_all()
    //             .build()
    //             .unwrap();
    //
    //         rt.block_on(async move {
    //             self.download_info.read();
    //             let url = "https://github.com/BrisklyDev/brisk-browser-extension/releases/download/v1.3.0/brisk-browser-extension.v1.3.0.edge.zip";
    //             if let Err(err) = download_to_file(url, "winrar.zip").await {
    //                 eprintln!("Download failed: {}", err);
    //             } else {
    //                 println!("Download complete");
    //             }
    //         });
    //     })
    // }

    fn start(&self) {
        println!("Spawned download thread");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async move {
            self.download_info.read();
            let url = "https://github.com/BrisklyDev/brisk-browser-extension/releases/download/v1.3.0/brisk-browser-extension.v1.3.0.edge.zip";
            if let Err(err) = download_to_file(url, "winrar.zip").await {
                eprintln!("Download failed: {}", err);
            } else {
                println!("Download complete");
            }
        });
    }

    fn pause(&self) {
        todo!()
    }

    fn cancel(&self) {
        todo!()
    }
}

async fn download_to_file(url: &str, path: &str) -> Result<(), ClientError> {
    println!("Downloading {} using hyper...", url);

    let client = HttpClient::new();
    let req = Request::builder()
        .method("GET")
        .uri(url)
        .header("User-Agent", "rust-hyper/1.0") // TODO proper user agent
        .body(Empty::<Bytes>::new())?;

    let resp = client.send(req).await?;
    let mut body = resp.into_body();
    let mut file = File::create(path).unwrap();

    // Stream the body chunks to the file
    while let Some(frame) = body.frame().await {
        println!("Receiving frame!!");
        let frame = frame.unwrap(); // TODO remove unwrap
        if let Some(data) = frame.data_ref() {
            let _ = file.write_all(data);
        }
    }
    Ok(())
}
