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
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::{result, thread};
use tokio::runtime::Runtime;

pub mod download_engine;

fn main() {
    let info = DownloadInfo::from_url(
        "https://download-installer.cdn.mozilla.net/pub/firefox/releases/150.0/linux-x86_64/en-US/firefox-150.0.tar.xz".to_string()
    );

    let setting = DownloadSetting::builder()
        .base_save_dir("/home/ryewell/Personal/kasmaout")
        .base_temp_dir("/home/ryewell/Personal/kasmaout/temp")
        .total_connections(8)
        .reset_timeout_millis(6000)
        .progress_polling_milliseconds(200)
        .with_logger()
        .build();

    let mut handle = spawn_http_engine(setting, info);

    enable_raw_mode().unwrap();

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
                        print!("pausing\r\n");
                        let _ = handle.tx.try_send(DownloadCommand::Pause);
                        print!("Download paused!\r\n");
                    }
                    KeyCode::Char('s') => {
                        let _ = handle.tx.try_send(DownloadCommand::Start);
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode().unwrap();
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
