use hyper::header::RANGE;
use std::fmt;

#[derive(Clone)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn new(start: u64, end: u64) -> Self {
        ByteRange { start, end }
    }

    pub fn is_in_range_of(&self, other: &ByteRange) -> bool {
        self.start >= other.end && self.start <= other.end
    }

    pub fn overlaps_with(&self, other: &ByteRange) -> bool {
        self.start <= other.start && self.end >= other.start
    }

    pub fn is_valid(&self) -> bool {
        self.start != self.end && self.start < self.end && self.start + 1 < self.end
    }
    
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn to_header(&self) -> (String, String) {
        (
            RANGE.to_string(),
            format!("bytes={}-{}", self.start, self.end),
        )
    }
}

impl PartialEq for ByteRange {
    fn eq(&self, other: &Self) -> bool {
        self.start == other.start && self.end == other.end
    }
}

impl fmt::Display for ByteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ByteRange::{}-{}", self.start, self.end)
    }
}
