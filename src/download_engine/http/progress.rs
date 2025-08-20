use crate::download_engine::{http::http_download_worker::Status, utils::now_millis};

pub struct WorkerProgress {
    pub status: Status,
    pub speed_bytes_per_sec: u64,
    pub worker_download_progress: f64,
    pub total_download_progress: f64,
    pub total_bytes_received: u64,
    pub last_response_time: u128,
}

impl WorkerProgress {
    pub fn new() -> Self {
        WorkerProgress {
            status: Status::Initial,
            speed_bytes_per_sec: 0,
            worker_download_progress: 0.0,
            total_download_progress: 0.0,
            total_bytes_received: 0,
            last_response_time: now_millis(),
        }
    }
}

pub struct DownloadProgress {
    pub speed_bytes_per_sec: u64,
    pub speed_str: String,
    pub total_download_progress: f64,
    pub status: Status,
    pub workers_progress: Vec<WorkerProgress>,
    pub estimated_remaining: String,
    pub estimated_remaining_sec: u64,
}

impl DownloadProgress {
    pub fn new() -> Self {
        DownloadProgress {
            speed_bytes_per_sec: 0,
            speed_str: "".to_string(),
            total_download_progress: 0.0,
            status: Status::Initial,
            estimated_remaining: "".to_string(),
            estimated_remaining_sec: 0,
            workers_progress: vec![],
        }
    }
}
