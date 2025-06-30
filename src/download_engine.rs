use crate::download_engine::http::FileInfo;
use std::collections::HashMap;
use std::thread;
use uuid::Uuid;

pub mod download_worker;
pub mod http;
pub mod utils;

pub struct Setting {
    proxy: Option<NetworkProxy>,
    total_connections: u8,
}

pub struct DownloadInfo {
    uid: String,
    url: String,
    headers: HashMap<String, String>,
    supports_range: bool,
    file_size: u64,
    file_name: String,
}

impl DownloadInfo {
    pub fn from(file_info: &FileInfo) -> Self {
        Self {
            uid: Uuid::new_v4().to_string(),
            url: file_info.url.clone(),
            headers: file_info.headers.clone(),
            supports_range: file_info.supports_range,
            file_size: file_info.file_size,
            file_name: file_info.file_name.clone(),
        }
    }
}

pub struct NetworkProxy {
    address: String,
    username: String,
    password: String,
}

pub trait Engine: Send + 'static {
    fn spawn_engine_thread(&self) -> thread::JoinHandle<()>;
}

pub fn run_download_engine() {
    println!("Engine started");

    // let download_handle = thread::spawn(run_download_thread);

    // download_handle.join().unwrap();
}
