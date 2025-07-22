use hyper::header::RANGE;
use std::cell::RefCell;
use std::rc::Rc;
use std::thread;
use tokio::runtime::Runtime;

use crate::download_engine::http::byte_range::ByteRange;
use crate::download_engine::http::byte_range::byte_range_tree::{ByteRangeNode, ByteRangeTree};
use crate::download_engine::{Runnable, http::http_download_engine::HttpDownloadEngine};

pub mod download_engine;

fn main() {
    println!(
        "Building tree for total size {} and with missing bytes of {}",
        600000, "30-70, 300-600, 900-1500"
    );
    let tree = ByteRangeTree::new_from_missing_bytes(
        600000,
        8,
        Vec::from([
            ByteRange::new(30, 70),
            ByteRange::new(300, 600),
        ]),
    );
    println!("{}", tree);
}

// fn spawn_engine<T: Engine>(engine: &T) -> thread::JoinHandle<()> {
//     engine.spawn_engine_thread()
// }

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
