use std::time::{SystemTime, UNIX_EPOCH};

pub mod shared_data;

pub fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards!")
        .as_millis()
}
