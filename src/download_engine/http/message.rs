use crate::download_engine::http::{byte_range::ByteRange, progress::DownloadProgress};

pub enum DownloadCommand {
    Pause,
    Start,
}

pub struct ButtonAvailability {
    pub pause_available: bool,
    pub resume_available: bool,
}

#[derive(Debug, Clone)]
pub enum EngineToWorkerMsg {
    Start,
    Stop,
    Reset,
    // args: new byte range, reuse
    RefreshByteRange(ByteRange, bool),
    StartReuseWorker(ByteRange),
}

pub enum EngineToMainMsg {
    Uid(String),
    Progress(f64),
    Complete,
}

#[derive(Debug)]
pub struct WorkerToEngineMsg {
    pub(crate) worker_number: u8,
    pub(crate) message: ToEngineMessage,
}

#[derive(Debug)]
pub enum ToEngineMessage {
    ConnectionSuccess,
    Complete(ByteRange),
    HandshakeResponse {
        reuse: bool,
    },
    ByteRangeRefreshSuccess {
        requested_range: ByteRange,
        reuse: bool,
    },
    ByteRangeRefreshRefused {
        requested_range: ByteRange,
        reuse: bool,
    },
    ByteRangeRefreshOverlapped {
        requested_range: ByteRange,
        refreshed_range: ByteRange,
        new_valid_range: ByteRange,
        reuse: bool,
    },
    Log(String),
    Stopped,
    Failed,
}
