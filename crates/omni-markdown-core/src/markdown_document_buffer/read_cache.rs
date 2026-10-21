//! One derived 8-KiB UTF-16 window. The piece tree and anonymous mapping remain
//! the sole text authority; scrolling does not accumulate visited windows.

use std::sync::Weak;

use super::*;

pub(super) const READ_WINDOW_UTF16_UNITS: usize = 4096;

/// A weak key retains allocation identity without retaining the tree. Reuse of
/// another allocation address cannot make an old root authoritative.
struct ReadKey {
    root: Option<Weak<MarkdownPieceTreeNode>>,
    generation: u64,
    utf16_len: usize,
}

impl ReadKey {
    fn matches(&self, root: &PieceTreeLink, generation: u64, utf16_len: usize) -> bool {
        // Both generation and real aggregate length must agree. Comparing the root
        // never upgrades the weak reference or reads document bytes.
        if self.generation != generation || self.utf16_len != utf16_len {
            return false;
        }
        match (&self.root, root) {
            (Some(weak), Some(root)) => std::ptr::eq(weak.as_ptr(), Arc::as_ptr(root)),
            (None, None) => true,
            // Empty and nonempty roots cannot exchange authority even when an external
            // generation happens to match.
            _ => false,
        }
    }
}

#[derive(Default)]
/// Allocates no text before the first read. Refills reuse the same box rather
/// than keeping an ever-growing collection of visited pages.
pub(super) struct Utf16ReadCache {
    key: Option<ReadKey>,
    units: Option<Box<[u16; READ_WINDOW_UTF16_UNITS]>>,
    valid: Range<usize>,
}

impl Utf16ReadCache {
    #[cfg(test)]
    pub(super) fn allocated_window_for_test(&self) -> Option<(usize, usize)> {
        // Tests inspect this actual box and fixed capacity only; this does not expose
        // document bytes or create a production retention handle.
        self.units
            .as_ref()
            .map(|units| (units.as_ptr().addr(), std::mem::size_of_val(units.as_ref())))
    }

    pub(super) fn copy(
        &mut self,
        source: &impl PieceByteSource,
        root: &PieceTreeLink,
        generation: u64,
        offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        source.source_health()?;
        let utf16_len = piece_tree_metrics(root).utf16_len;
        let end = offset
            .checked_add(destination.len())
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if end > utf16_len {
            // Reject the entire invalid request before changing caller memory or
            // allocating a window; never silently truncate the requested range.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if !self
            .key
            .as_ref()
            .is_some_and(|key| key.matches(root, generation, utf16_len))
        {
            // Either root or generation change revokes the old window. The weak key
            // retains neither the previous text nor its undo history.
            self.valid = 0..0;
            self.key = Some(ReadKey {
                root: root.as_ref().map(Arc::downgrade),
                generation,
                utf16_len,
            });
        }
        let mut written = 0;
        while written < destination.len() {
            // Check shared source health on every iteration, including a reclamation
            // failure recorded by another thread after the last refill.
            source.source_health()?;
            let current = offset + written;
            if !self.valid.contains(&current) {
                // Align by UTF-16 units rather than scalars. A surrogate pair may span
                // two real windows without changing its external coordinates.
                let start = current - current % READ_WINDOW_UTF16_UNITS;
                let end = start.saturating_add(READ_WINDOW_UTF16_UNITS).min(utf16_len);
                self.refill(source, root, start..end)?;
            }
            let count = (self.valid.end - current).min(destination.len() - written);
            let local = current - self.valid.start;
            let units = self
                .units
                .as_ref()
                .ok_or(MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)?;
            // Copy only the requested intersection. Prefetching changes neither the
            // caller-visible length nor actual UTF-16 positions.
            destination[written..written + count].copy_from_slice(&units[local..local + count]);
            written += count;
        }
        // Even an empty request checks health; a cache hit or zero-character read
        // cannot bypass a permanent failure of the shared original source.
        source.source_health()
    }

    fn refill(
        &mut self,
        source: &impl PieceByteSource,
        root: &PieceTreeLink,
        range: Range<usize>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        // Revoke validity before overwriting the box. A read or real reclamation
        // failure leaves storage that cannot be published as a valid window.
        self.valid = 0..0;
        let units = self
            .units
            .get_or_insert_with(|| Box::new([0; READ_WINDOW_UTF16_UNITS]));
        let mut written = 0;
        copy_utf16_tree_range(
            source,
            root.as_deref(),
            range.clone(),
            0,
            &mut units[..range.len()],
            &mut written,
        )?;
        if written != range.len() {
            // The tree must cover the complete window. A short read cannot publish
            // validity or fill the missing document text with invented zeroes.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        source.source_health()?;
        self.valid = range;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
