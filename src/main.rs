use std::thread;

use tokio::runtime::Runtime;

use crate::download_engine::{Engine, http::http_download_engine::HttpDownloadEngine};

pub mod download_engine;

#[tokio::main]
async fn main() {
    // Example: spawn thread per download
    let engine = HttpDownloadEngine::new();
    let handle = spawn_engine(&engine);
    handle.join().unwrap();
}

fn spawn_engine<T: Engine>(engine: &T) -> thread::JoinHandle<()> {
    engine.spawn_engine_thread()
}

fn run_download_task() {
    let mut handles = vec![];

    for worker_id in 0..4 {
        let handle = thread::spawn(move || {
            worker_main(worker_id);
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

fn worker_main(worker_id: usize) {
    // Build a single-threaded Tokio runtime for this worker thread
    let rt = Runtime::new().unwrap();

    rt.block_on(async move {});
}
