use std::cell::RefCell;
use std::rc::{Rc, Weak};

use crate::download_engine::http::byte_range::{ByteRange, ByteRangeStatus};

type NodeRef = Rc<RefCell<ByteRangeNode>>;

pub struct ByteRangeNode {
    pub range: ByteRange,
    pub parent: Weak<RefCell<ByteRangeNode>>,
    pub worker_number: u8,
    pub status: ByteRangeStatus,
    pub right_child: Option<NodeRef>,
    pub left_child: Option<NodeRef>,
    pub right_neighbor: Option<NodeRef>,
    pub left_neighbor: Option<NodeRef>,
}

impl ByteRangeNode {
    /// Creates a right child and links it to this node
    pub fn create_right_child(
        self_rc: &NodeRef,
        start_byte: u64,
        end_byte: u64,
        status: ByteRangeStatus,
        worker_number: u8,
    ) {
        let child = Rc::new(RefCell::new(ByteRangeNode {
            range: ByteRange::new(start_byte, end_byte),
            parent: Rc::downgrade(self_rc),
            worker_number,
            status,
            right_child: None,
            left_child: None,
            right_neighbor: None,
            left_neighbor: None,
        }));

        self_rc.borrow_mut().right_child = Some(child);
    }

    pub fn create_left_child(
        self_rc: &NodeRef,
        start_byte: u64,
        end_byte: u64,
        status: ByteRangeStatus,
        worker_number: u8,
    ) {
        let child = Rc::new(RefCell::new(ByteRangeNode {
            range: ByteRange::new(start_byte, end_byte),
            parent: Rc::downgrade(self_rc),
            worker_number,
            status,
            right_child: None,
            left_child: None,
            right_neighbor: None,
            left_neighbor: None,
        }));

        self_rc.borrow_mut().left_child = Some(child);
    }
}
