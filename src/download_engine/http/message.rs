use crate::download_engine::http::byte_range::ByteRange;

pub enum DownloadCommand {
    Pause,
    Start,
}

pub enum EngineToMainMsg {}

#[derive(Debug)]
pub struct WorkerToEngineMsg {
    pub(crate) worker_number: u8,
    pub(crate) message: ToEngineMessage,
}

#[derive(Debug)]
pub enum ToEngineMessage {
    Complete,
    ByteRangeRefreshSuccess {
        refreshed_start_byte: u64,
        refreshed_end_byte: u64,
        reuse: bool,
    },
    ByteRangeRefreshRefused {
        requested_range: ByteRange,
        reuse: bool,
    },
    ByteRangeRefreshOverlapped {
        new_valid_start_byte: u64,
        new_valid_end_byte: u64,
        refreshed_start_byte: u64,
        refreshed_end_byte: u64,
    },
    Stopped,
    Failed,
}
