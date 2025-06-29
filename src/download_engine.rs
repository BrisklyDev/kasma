use std::{iter::Map, thread};

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
    headers: Map<String, String>,
    supports_byte_range: bool,
    file_size: u64,
    file_name: String,
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
