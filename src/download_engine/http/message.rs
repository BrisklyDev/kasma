use crate::download_engine::http::{byte_range::ByteRange, progress::DownloadProgress};

pub enum DownloadCommand {
    Pause,
    Start,
}

#[derive(Debug)]
pub enum EngineToWorkerMsg {
    Start,
    Stop,
    Reset,
    RefreshByteRange(ByteRange, bool),
    StartReuseConnection(ByteRange),
}

pub enum EngineToMainMsg {
    Uid(String),
    Progress(DownloadProgress),
}

#[derive(Debug)]
pub struct WorkerToEngineMsg {
    pub(crate) worker_number: u8,
    pub(crate) message: ToEngineMessage,
}

#[derive(Debug)]
pub enum ToEngineMessage {
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
    Stopped,
    Failed,
}
