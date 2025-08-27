use crate::download_engine::handle::spawn_http_engine;
use crate::download_engine::http::byte_range::ByteRange;
use crate::download_engine::http::byte_range::byte_range_tree::{
    ByteRangeNode, ByteRangeStatus, ByteRangeTree,
};
use crate::download_engine::http::message::{DownloadCommand, EngineToMainMsg};
use crate::download_engine::setting::DownloadSetting;
use crate::download_engine::utils::file::resolve_versioned_file_path;
use crate::download_engine::{
    DownloadInfo, RunnableTask, http::http_download_engine::HttpDownloadEngine,
};
use crossterm::event::{self, Event, KeyCode};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::{result, thread};
use tokio::runtime::Runtime;

pub mod download_engine;

fn main() {
    let info = DownloadInfo::from_url(
        "https://dl5.dlhas.ir/hosein/Game/May2025/24/Updates/Google_Chrome_v109.0.5414.120_32-bit_www.Downloadha.com_.msi".to_string()
    );

    let setting = DownloadSetting::builder()
        .base_save_dir("C:\\Users\\RyeWell\\Desktop\\kasma-out")
        .base_temp_dir("C:\\Users\\RyeWell\\Desktop\\kasma-out\\temp")
        .total_connections(8)
        .reset_timeout_millis(6000)
        .progress_polling_milliseconds(200)
        .with_logger()
        .build();

    let mut handle = spawn_http_engine(setting, info);

    let pb = ProgressBar::new(100);
    let style = ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {percent}%")
        .unwrap()
        .progress_chars("#>-");
    pb.set_style(style);

    loop {
        match handle.rx.try_recv() {
            Ok(msg) => match msg {
                EngineToMainMsg::Progress(p) => {
                    pb.set_position((p * 100.0) as u64);
                }
                EngineToMainMsg::Complete => {
                    pb.finish_with_message("Download complete");
                    break;
                }
                _ => {}
            },
            _ => {}
        }
        if event::poll(std::time::Duration::from_millis(10)).unwrap() {
            if let Event::Key(key_event) = event::read().unwrap() {
                match key_event.code {
                    KeyCode::Char('p') => {
                        let _ = handle.tx.try_send(DownloadCommand::Pause);
                        println!("Download paused!");
                    }
                    KeyCode::Char('s') => {
                        let _ = handle.tx.try_send(DownloadCommand::Start);
                    }
                    _ => {}
                }
            }
        }
    }

    handle.join.join().unwrap();
}

// fn main() {
//     let node = ByteRangeNode::new(
//         None,
//         ByteRange::new(0, 30725219),
//         ByteRangeStatus::ToDownload,
//         0,
//     );
//
//     let mut tree =
//         ByteRangeTree::from_missing_bytes(92213248, 8, vec![ByteRange::new(0, 92213248)]);
//
//     let res = tree.split();
//     let node = tree.search_node(ByteRange::new(0, 46106624)).unwrap();
//     tree.split_byte_range_node(&node, true);
//     let res = tree.split();
//     if let Err(err) = res {
//         println!("ERORR: {}", err);
//     }
//     let res = tree.split();
//     if let Err(err) = res {
//         println!("ERORR: {}", err);
//     }
//     println!("{}", tree);
// }
