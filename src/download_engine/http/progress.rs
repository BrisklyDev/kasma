use crate::download_engine::http::http_download_worker::Status;

pub struct WorkerProgress {
    pub speed_bytes_per_sec: u64,
    pub worker_download_progress: f64,
    pub total_download_progress: f64,
}

impl WorkerProgress {
    pub fn new() -> Self {
        WorkerProgress {
            speed_bytes_per_sec: 0,
            worker_download_progress: 0.0,
            total_download_progress: 0.0,
        }
    }
}

pub struct DownloadProgress {
    pub speed_bytes_per_sec: u64,
    pub speed_str: String,
    pub total_download_progress: f64,
    pub status: Status,
    pub workers_progress: Vec<WorkerProgress>,
}

impl DownloadProgress {
    pub fn new() -> Self {
        DownloadProgress {
            speed_bytes_per_sec: 0,
            speed_str: "".to_string(),
            total_download_progress: 0.0,
            status: Status::Initial,
            workers_progress: vec![],
        }
    }
}
