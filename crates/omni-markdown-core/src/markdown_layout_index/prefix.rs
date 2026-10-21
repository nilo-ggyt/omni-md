//! An immutable committed prefix owns one descriptor on the original anonymous
//! index, with no copied records or writable mapping.

use super::*;

const MAX_PREFIX_READERS: usize = 2;

/// Prefix publication alone owns backpressure. Ordinary GCB/line-break indexes
/// retain their existing I/O error contract without this publication state.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LayoutIndexSnapshotError {
    #[error("原索引的两份在用前缀快照尚未归还")]
    ReadersBusy,
    #[error("原前缀索引发布失败：{0}")]
    Index(#[from] LayoutIndexError),
}
type SnapshotResult<T> = std::result::Result<T, LayoutIndexSnapshotError>;

/// Reserve a real descriptor slot before publication. Failed construction
/// returns it synchronously; a constructed reader settles it by actual close.
struct Reservation {
    resources: Arc<ResourceState>,
    transferred: bool,
}
impl Reservation {
    /// Admit at most the existing current/candidate prefixes without waiting for
    /// Native release or manufacturing another reader descriptor.
    fn new(resources: &Arc<ResourceState>) -> SnapshotResult<Self> {
        let mut count = resources.prefix_readers.load(Ordering::Acquire);
        loop {
            if count >= MAX_PREFIX_READERS {
                return Err(LayoutIndexSnapshotError::ReadersBusy);
            } // Retain the current frame and stop publication while both
            // original prefix slots remain owned.
            match resources.prefix_readers.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => count = actual, // Retry only atomic contention
                                               // for this slot, with no layout/native polling.
            }
        }
        Ok(Self {
            resources: Arc::clone(resources),
            transferred: false,
        })
    }
    /// Move slot settlement to the reader that now owns its real descriptor.
    fn transfer(mut self) -> Arc<ResourceState> {
        self.transferred = true;
        Arc::clone(&self.resources)
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.transferred {
            self.resources.prefix_readers.fetch_sub(1, Ordering::AcqRel);
        } // Construction failure issued no reader FD; return its slot now.
    }
}

impl<const N: usize> PagedLayoutIndex<N> {
    /// Publish only the upper layer's committed LineEnd extent. This generic
    /// index grants no document-text or line-semantic authority of its own.
    pub(crate) fn snapshot_reader(
        &mut self,
        committed_records: usize,
    ) -> SnapshotResult<LayoutIndexReader<N>> {
        self.healthy()?;
        if committed_records == 0 || committed_records > self.count || self.sealed {
            return Err(Error::InvalidRecord.into());
        } // Invalid coordinates perform no I/O and cannot restore append
        // permission to an already sealed index.
        committed_records
            .checked_mul(N)
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(Error::Overflow)?;
        let reservation = Reservation::new(&self.resources)?;
        let result: Result<LayoutIndexReader<N>> = (|| {
            // Write through this committed endpoint only. Later appends reuse
            // the same buffer without rewriting any published prefix byte.
            self.flush_through(committed_records)?;
            let file = self.file.as_ref().ok_or(Error::Poisoned)?;
            let prefix_file = file.try_clone()?;
            // The reader uses positioned reads and a fixed logical EOF. Shared
            // file cursor or later file growth cannot extend that boundary.
            Ok(LayoutIndexReader::from_prefix(
                prefix_file,
                committed_records,
                reservation.transfer(),
            ))
        })();
        if result.is_err() {
            self.mark_failed();
        } // Actual write/descriptor-clone failure permanently rejects the
        // producer; slot exhaustion returned separately before this block.
        result.map_err(Into::into)
    }
}
