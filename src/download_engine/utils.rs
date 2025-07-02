use crate::download_engine::http::segment::byte_range::ByteRange;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, io};

#[derive(Clone)]
pub struct TempFileMetadata {
    pub name: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub worker_number: u8,
    pub size: u64,
}

impl TempFileMetadata {
    pub fn is_in_range(&self, range: ByteRange) -> bool {
        (self.start_byte >= range.start
            && self.start_byte < range.end
            && self.end_byte <= range.end
            && self.end_byte > range.start)
            || (self.start_byte < range.end && self.end_byte > range.end)
    }

    pub fn from_path_buf(path: &PathBuf) -> TempFileMetadata {
        let meta = fs::metadata(path).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        let (worker_number, start_byte, end_byte) = extract_worker_and_range(name).unwrap();
        TempFileMetadata {
            worker_number,
            start_byte,
            end_byte,
            name: name.to_string(),
            size: meta.len(),
        }
    }
}

fn extract_worker_and_range(s: &str) -> Option<(u8, u64, u64)> {
    let parts: Vec<&str> = s.split('#').collect();
    if parts.len() != 2 {
        return None;
    }
    let worker = parts[0].parse::<u8>().ok()?;
    let range_part = parts[1];
    let range: Vec<&str> = range_part.split('-').collect();
    if range.len() != 2 {
        return None;
    }
    let start = range[0].parse::<u64>().ok()?;
    let end = range[1].parse::<u64>().ok()?;
    Some((worker, start, end))
}
pub fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards!")
        .as_millis()
}

pub fn list_files_in_dir(dir: PathBuf) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            files.push(path);
        }
    }
    Ok(files)
}
