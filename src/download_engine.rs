use crate::download_engine::http::FileInfo;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

pub mod http;
pub mod utils;

pub struct DownloadSetting {
    pub proxy: Option<NetworkProxy>,
    pub total_connections: u8,
    pub base_save_dir: PathBuf,
    pub base_temp_dir: PathBuf,
}

#[derive(Clone)]
pub struct DownloadItem {
    uid: String,
    url: String,
    prefetched_info: bool,
    headers: HashMap<String, String>,
    supports_range: bool,
    file_size: u64,
    file_name: String,
}

pub struct DownloadInfo {
    pub url: String,
    pub uid: Option<String>,
    pub supports_range: Option<bool>,
    pub file_size: Option<u64>,
    pub filename: Option<String>,
}

impl DownloadItem {
    pub fn from(file_info: &FileInfo) -> Self {
        Self {
            uid: Uuid::new_v4().to_string(),
            url: file_info.url.clone(),
            prefetched_info: false,
            headers: HashMap::new(),
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

pub trait RunnableTask {
    fn run(&mut self);
}

pub fn run_download_engine() {
    println!("Engine started");

    // let download_handle = thread::spawn(run_download_thread);

    // download_handle.join().unwrap();
}
