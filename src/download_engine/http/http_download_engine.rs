use std::thread;

use crate::download_engine::Engine;

pub struct HttpDownloadEngine {
    workers: Vec<String>,
}

impl Engine for HttpDownloadEngine {
    fn spawn_engine_thread(&self) -> thread::JoinHandle<()> {
        thread::spawn(|| {})
    }
}

impl HttpDownloadEngine {
    pub fn new() -> Self {
        HttpDownloadEngine { workers: vec![] }
    }
}
