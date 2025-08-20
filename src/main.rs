use crate::download_engine::http::byte_range::ByteRange;
use crate::download_engine::http::byte_range::byte_range_tree::{
    ByteRangeNode, ByteRangeStatus, ByteRangeTree,
};
use crate::download_engine::http::message::{DownloadCommand, EngineToMainMsg};
use crate::download_engine::setting::DownloadSetting;
use crate::download_engine::utils::file::resolve_versioned_file_path;
use crate::download_engine::{
    DownloadInfo, RunnableTask, http::http_download_engine::HttpDownloadEngine, spawn_http_engine,
};
use std::path::PathBuf;
use std::{result, thread};
use tokio::runtime::Runtime;

pub mod download_engine;

fn main() {
    let info = DownloadInfo::from_url("https://dl5.dlhas.ir/hosein/Game/May2025/24/Updates/Google_Chrome_v109.0.5414.120_32-bit_www.Downloadha.com_.msi".to_string());
    let setting = DownloadSetting::builder()
        .base_save_dir("C:\\Users\\RyeWell\\Desktop\\kasma-out")
        .base_temp_dir("C:\\Users\\RyeWell\\Desktop\\kasma-out\\temp")
        .total_connections(8)
        .reset_timeout_millis(6000)
        .progress_polling_milliseconds(200)
        .build();
    let result = spawn_http_engine(setting, info);
    result.1.join().unwrap();
}

// fn main() {
//     let node = ByteRangeNode::new(
//         None,
//         ByteRange::new(0, 30725219),
//         ByteRangeStatus::ToDownload,
//         0,
//     );
//
//     let mut tree = ByteRangeTree::from_missing_bytes(30725219, 8, vec![ByteRange::new(0, 30725219)]);
//
//     let res = tree.split();
//     println!("1st split =====\n{}", tree);
//     if res.is_err() {
//         println!("Err {}", res.unwrap_err());
//     }
//     let res = tree.split();
//     if res.is_err() {
//         println!("Err {}", res.unwrap_err());
//     }
//     println!("2nd split =====\n{}", tree);
//     let res = tree.split();
//     if res.is_err() {
//         println!("Err {}", res.unwrap_err());
//     }
//     println!("3nd split =====\n{}", tree);
//     // let res = tree.split();
// }

// fn spawn_engine<T: Engine>(engine: &T) -> thread::JoinHandle<()> {
//     engine.spawn_engine_thread()
// }
