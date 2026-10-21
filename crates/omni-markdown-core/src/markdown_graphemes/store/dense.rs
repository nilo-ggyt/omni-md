//! Exact whole-frozen-source printable-ASCII proof authorizes this representation: every integer
//! coordinate is an original GCB, without any file descriptor or index page.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Receipt for this owner's actual close/Drop, retaining one atomic flag and no Source, text, or dummy index.
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
        // Include both BOF and actual EOF; overflow cannot be represented as a truncated boundary count.
        let count = source_length
            .checked_add(1)
            .ok_or(Error::CoordinateOverflow)?;
        Ok(Self {
            count,
            closed: DenseObserver(Arc::new(AtomicBool::new(false))),
        })
    }
    pub(super) fn get(&self, ordinal: usize) -> Result<Boundary> {
        // Closed owners grant no arithmetic result. Exact ASCII proof makes byte, UTF-16,
        // and ordinal coordinates equal only within this original finite extent.
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
