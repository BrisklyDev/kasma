use std::time::{SystemTime, UNIX_EPOCH};

pub struct TempFileMetadata {
    pub name: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub worker_number: u8,
    pub size: u64,
}

pub fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards!")
        .as_millis()
}
