//! 只有全冻结源Printable ASCII精确证明可构造此表示；每整数坐标都是原GCB，且无FD/页。
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// 原owner的真实close/Drop收据，只保一个原子位，不持Source、正文或伪造layout-index。
#[derive(Clone)]
pub(in crate::markdown_graphemes) struct DenseObserver(Arc<AtomicBool>);
impl DenseObserver {
    pub(super) fn released(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
pub(super) struct DenseIndex {
    count: usize,
    closed: DenseObserver,
}
impl DenseIndex {
    pub(super) fn new(source_length: usize) -> Result<Self> {
        let count = source_length
            .checked_add(1)
            .ok_or(Error::CoordinateOverflow)?;
        Ok(Self {
            count,
            closed: DenseObserver(Arc::new(AtomicBool::new(false))),
        })
    }
    pub(super) fn get(&self, ordinal: usize) -> Result<Boundary> {
        if self.closed.released() {
            return Err(Error::Poisoned);
        }
        if ordinal >= self.count {
            return Err(Error::CorruptMetadata);
        }
        Ok(Boundary {
            byte: ordinal,
            utf16: ordinal,
        })
    }
    pub(super) fn len(&self) -> usize {
        self.count
    }
    pub(super) fn observer(&self) -> DenseObserver {
        self.closed.clone()
    }
    pub(super) fn require(&self) -> Result<()> {
        if self.closed.released() {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
    pub(super) fn close(&mut self) {
        self.closed.0.store(true, Ordering::Release);
    }
}
impl Drop for DenseIndex {
    fn drop(&mut self) {
        self.close();
    }
}
