//! Read-only access receives the original sealed file without owning a mapping,
//! a page cache, or a document-text copy.

use std::{fs::File, ops::Range, os::unix::fs::FileExt, sync::Arc};

use super::{Error, LayoutIndexObserver, PAGE, ResourceState, Result, close_file, read_exact_at};

/// A transferable reader owns its file and fixed atomic receipts independently
/// of the builder that issued it.
pub(crate) struct LayoutIndexReader<const N: usize> {
    file: Option<File>,
    count: usize,
    resources: Arc<ResourceState>,
    #[cfg(feature = "paged-shaping-frontier")]
    upstream: Option<Arc<ResourceState>>,
    #[cfg(feature = "paged-shaping-frontier")]
    prefix_claim: bool,
}

impl<const N: usize> LayoutIndexReader<N> {
    pub(super) fn from_sealed(file: File, count: usize, resources: Arc<ResourceState>) -> Self {
        // Final file handoff observes this same scope's first failure; an older
        // prefix can still report an actual underlying I/O failure.
        #[cfg(feature = "paged-shaping-frontier")]
        let upstream = Some(Arc::clone(&resources));
        Self {
            file: Some(file),
            count,
            resources,
            #[cfg(feature = "paged-shaping-frontier")]
            upstream,
            #[cfg(feature = "paged-shaping-frontier")]
            prefix_claim: false,
        }
    }

    #[cfg(feature = "paged-shaping-frontier")]
    /// Own one real cloned descriptor with a fixed logical prefix EOF and a
    /// separate close receipt, while observing its original producer's health.
    pub(super) fn from_prefix(file: File, count: usize, upstream: Arc<ResourceState>) -> Self {
        let resources = Arc::new(ResourceState::default());
        resources.file_open.store(true, super::Ordering::Release);
        Self {
            file: Some(file),
            count,
            resources,
            upstream: Some(upstream),
            prefix_claim: true,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }

    pub(crate) fn resource_observer(&self) -> LayoutIndexObserver {
        LayoutIndexObserver {
            own: Arc::clone(&self.resources),
            #[cfg(feature = "paged-shaping-frontier")]
            upstream: self.upstream.clone(),
        }
    }

    /// Missing ownership or any prior local/upstream failure ends reading.
    pub(crate) fn healthy(&self) -> Result<()> {
        if self.file.is_none()
            || self.resources.reader_failed.load(super::Ordering::Acquire)
            || self.resources.cleanup_failed.load(super::Ordering::Acquire)
            || self.upstream_has_failed()
        {
            // First failure revoked the complete output. Remaining correct
            // records cannot reclassify a later read as a complete publication.
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    fn upstream_has_failed(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.upstream.as_ref().is_some_and(|upstream| {
                upstream.producer_failed.load(super::Ordering::Acquire)
                    || upstream.cleanup_failed.load(super::Ordering::Acquire)
            })
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }

    /// Fixed EOF and File ownership stay immutable. Atomic rejection allows
    /// shared reads without holding a lock across I/O.
    pub(crate) fn read(&self, record: usize) -> Result<[u8; N]> {
        self.healthy()?;
        let end = match record.checked_add(1) {
            Some(end) => end,
            None => {
                // A single-record overflow still permanently rejects this
                // reader and remains visible to its resource observer.
                self.fail();
                return Err(Error::Overflow);
            }
        };
        let mut output = [0; N];
        self.read_batch(record..end, std::slice::from_mut(&mut output))?;
        Ok(output)
    }

    /// Read at most 256 records and 16 KiB into exactly matching output space.
    ///
    /// An I/O error may fill a prefix; the caller must discard the whole
    /// candidate instead of publishing that incomplete output.
    pub(crate) fn read_batch(&self, range: Range<usize>, output: &mut [[u8; N]]) -> Result<()> {
        self.healthy()?;
        let result = self.read_batch_inner(range, output);
        #[cfg(feature = "paged-shaping-frontier")]
        if matches!(&result, Err(Error::Io(_)))
            && let Some(upstream) = &self.upstream
        {
            upstream
                .producer_failed
                .store(true, super::Ordering::Release);
        } // Actual file I/O failure affects every prefix on the same inode.
        // Invalid caller coordinates revoke only their own reader.
        if result.is_err() {
            // Both invalid input and I/O failure end this reader's authority;
            // the current Result still carries its original cause.
            self.fail();
        }
        result
    }

    /// Validate the complete fixed interval before the first positioned read.
    fn read_batch_inner(&self, range: Range<usize>, output: &mut [[u8; N]]) -> Result<()> {
        let records = range
            .end
            .checked_sub(range.start)
            .filter(|&records| records == output.len() && records <= 256)
            .ok_or(Error::InvalidRecord)?;
        let bytes = records.checked_mul(N).ok_or(Error::Overflow)?;
        if range.end > self.count || bytes > PAGE {
            // This limit is the actual read budget; shortening the request
            // cannot count as completing the original call.
            return Err(Error::InvalidRecord);
        }
        let start = range.start.checked_mul(N).ok_or(Error::Overflow)?;
        let offset = u64::try_from(start).map_err(|_| Error::Overflow)?;
        offset
            .checked_add(u64::try_from(bytes).map_err(|_| Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        let file = self.file.as_ref().ok_or(Error::Poisoned)?;
        // Reuse the builder's exact-extent protocol. The outer boundary rejects
        // this whole reader on failure while returning the actual first cause.
        read_exact_at(output.as_flattened_mut(), offset, |output, position| {
            file.read_at(output, position)
        })
    }

    /// Revoke this reader without modifying or closing another reader's FD.
    fn fail(&self) {
        self.resources
            .reader_failed
            .store(true, super::Ordering::Release);
    }

    /// Return the owned descriptor once, settling a prefix reservation only
    /// after an actual successful close.
    fn close_resources(&mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            // close_file performs the actual unique close; it creates no second
            // File and never retries a possibly reused descriptor number.
            let closed = close_file(file, &self.resources);
            #[cfg(feature = "paged-shaping-frontier")]
            if let Some(upstream) = &self.upstream {
                if closed.is_ok() && self.prefix_claim {
                    // Only this prefix's proven successful close returns its
                    // publication slot to the original producer.
                    upstream
                        .prefix_readers
                        .fetch_sub(1, super::Ordering::AcqRel);
                    self.prefix_claim = false;
                } else if closed.is_err() {
                    // Keep an old prefix's real close failure in original
                    // receipts so subsequent publication cannot erase it.
                    upstream
                        .cleanup_failed
                        .store(true, super::Ordering::Release);
                    upstream
                        .producer_failed
                        .store(true, super::Ordering::Release);
                }
            } // Unknown close outcomes retain their negative reservation and
            // cannot authorize opening another prefix descriptor.
            closed?;
        }
        if self.resources.cleanup_failed.load(super::Ordering::Acquire) {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Consume the reader and return its resources while retaining any earlier
    /// read/upstream failure as a separate negative operation outcome.
    pub(crate) fn close(mut self) -> Result<()> {
        self.close_resources()?;
        if self.resources.reader_failed.load(super::Ordering::Acquire) || self.upstream_has_failed()
        {
            // Real resource return cannot erase a previous read failure;
            // observers distinguish the I/O result from cleanup facts.
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
}

impl<const N: usize> Drop for LayoutIndexReader<N> {
    fn drop(&mut self) {
        // Settle only an as-yet-owned FD. Read rejection alone does not claim
        // that its actual kernel close failed.
        if self.close_resources().is_err() {
            self.resources
                .cleanup_failed
                .store(true, super::Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests;
