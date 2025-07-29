use crate::download_engine::http::message::{DownloadCommand, EngineToMainMsg};
use crate::download_engine::{RunnableTask, http::http_download_engine::HttpDownloadEngine};
use std::thread;
use tokio::runtime::Runtime;

pub mod download_engine;

// #[tokio::main]
fn main() {
    // Example: spawn thread per download
    let (engine_to_main_tx, engine_to_main_rx) = tokio::sync::mpsc::channel::<EngineToMainMsg>(100);
    let (main_to_engine_tx, main_to_engine_rx) = tokio::sync::mpsc::channel::<DownloadCommand>(100);
    let handle = thread::spawn(move || {
        HttpDownloadEngine::new(main_to_engine_rx, engine_to_main_tx).run();
    });
    handle.join().unwrap();
}

// fn main() {
//     let node = ByteRangeNode::new(ByteRange::new(0, 100000), ByteRangeStatus::ToDownload, 0);
//
//     let mut tree = ByteRangeTree::new(node, 8);
//     tree.split();
//     tree.split();
//     println!("{}", tree);
// }

// fn main() {
//     println!(
//         "Building tree for total size {} and with missing bytes of {}",
//         600000, "30-70, 300-600, 900-1500"
//     );
//     let tree = ByteRangeTree::new_from_missing_bytes(
//         600000,
//         8,
//         Vec::from([
//             ByteRange::new(30, 70),
//             ByteRange::new(300, 600),
//             ByteRange::new(900, 1500),
//         ]),
//     );
//     println!("{}", tree);
// }
//

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
