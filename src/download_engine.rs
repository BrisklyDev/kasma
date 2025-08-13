use crate::download_engine::http::FileInfo;
use std::collections::HashMap;
use std::path::PathBuf;
use strum_macros::{Display, EnumString};
use uuid::Uuid;

pub mod http;
pub mod setting;
pub mod utils;
mod errors;
mod macros;

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
