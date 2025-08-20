use crate::download_engine::http::FileInfo;
use crate::download_engine::http::http_download_engine::HttpDownloadEngine;
use crate::download_engine::http::message::{DownloadCommand, EngineToMainMsg, EngineToWorkerMsg};
use crate::download_engine::setting::DownloadSetting;
use crate::download_engine::utils::sync_ext::MpscChannel;
use std::collections::HashMap;
use std::path::PathBuf;
use std::thread;
use std::thread::JoinHandle;
use strum_macros::{Display, EnumString};
use uuid::Uuid;

mod errors;
pub mod http;
mod macros;
pub mod setting;
pub mod utils;

#[derive(PartialEq, Copy, Clone, Display, EnumString)]
pub enum EngineState {
    Complete,
    WorkersComplete,
    Initial,
    Running,
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

impl DownloadInfo {
    pub fn from_url(url: String) -> Self {
        DownloadInfo {
            url,
            uid: None,
            supports_range: None,
            file_size: None,
            filename: None,
        }
    }
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

pub trait RunnableTask {
    fn run(&mut self);
}

pub fn spawn_http_engine(
    setting: DownloadSetting,
    info: DownloadInfo,
) -> (
    MpscChannel<DownloadCommand, EngineToMainMsg>,
    JoinHandle<()>,
) {
    let (engine_to_main_tx, engine_to_main_rx) = tokio::sync::mpsc::channel::<EngineToMainMsg>(100);
    let (main_to_engine_tx, main_to_engine_rx) = tokio::sync::mpsc::channel::<DownloadCommand>(100);
    let handle = thread::spawn(move || {
        HttpDownloadEngine::new(main_to_engine_rx, engine_to_main_tx, info, setting, None)
            .0
            .run();
    });
    let channel = MpscChannel {
        sender: main_to_engine_tx,
        receiver: engine_to_main_rx,
    };
    (channel, handle)
}
