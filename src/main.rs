use crate::download_engine::http::byte_range::ByteRange;
use crate::download_engine::http::byte_range::byte_range_tree::ByteRangeTree;
use crate::download_engine::http::message::{DownloadCommand, EngineToMainMsg};
use crate::download_engine::utils::file::resolve_versioned_file_path;
use crate::download_engine::{
    DownloadInfo, DownloadSetting, RunnableTask, http::http_download_engine::HttpDownloadEngine,
};
use std::path::PathBuf;
use std::thread;
use tokio::runtime::Runtime;

pub mod download_engine;

fn main() {
    let (engine_to_main_tx, engine_to_main_rx) = tokio::sync::mpsc::channel::<EngineToMainMsg>(100);
    let (main_to_engine_tx, main_to_engine_rx) = tokio::sync::mpsc::channel::<DownloadCommand>(100);
    let info = DownloadInfo {
        url: "https://github.com/BrisklyDev/brisk/releases/download/v2.3.2/Brisk-v2.3.2-macos.dmg"
            .to_string(),
        uid: None,
        supports_range: None,
        file_size: None,
        filename: None,
    };
    let setting = DownloadSetting {
        proxy: None,
        total_connections: 8,
        base_save_dir: PathBuf::from("C:\\Users\\RyeWell\\Desktop\\kasma-out"),
        base_temp_dir: PathBuf::from("C:\\Users\\RyeWell\\Desktop\\kasma-out\\temp"),
    };
    let handle = thread::spawn(move || {
        HttpDownloadEngine::new(main_to_engine_rx, engine_to_main_tx, info, setting, None)
            .0
            .run();
    });
    handle.join().unwrap();
    let aaa = resolve_versioned_file_path(
        "ttt.tar.gz",
        PathBuf::from("C:\\Users\\RyeWell\\Desktop\\kasma-out"),
    );
    println!("{}", aaa.unwrap().to_str().unwrap());
}

// fn main() {
//     let node = ByteRangeNode::new(ByteRange::new(0, 100000), ByteRangeStatus::ToDownload, 0);
//
//     let mut tree = ByteRangeTree::new(node, 8);
//     tree.split();
//     tree.split();
//     println!("{}", tree);
// }

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
