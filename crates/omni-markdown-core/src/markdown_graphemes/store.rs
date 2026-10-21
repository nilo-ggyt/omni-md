//! Grapheme boundaries have two representations: anonymous 16-byte Unicode records, or dense
//! integer coordinates authorized by exact whole-source printable-ASCII proof.
use super::{MarkdownGraphemeBoundary as Boundary, MarkdownGraphemeError as Error, Result};
use crate::markdown_layout_index::{LayoutIndexError, LayoutIndexObserver, PagedLayoutIndex};

#[cfg(feature = "paged-shaping-frontier")]
mod dense;

enum Representation {
    Paged(PagedLayoutIndex<16>),
    #[cfg(feature = "paged-shaping-frontier")]
    DensePrintableAscii(dense::DenseIndex),
}
pub(super) struct BoundaryIndex {
    inner: Representation,
    reads: u64,
    writes: u64,
    #[cfg(test)]
    pub(super) fail_record: Option<usize>,
}

/// Dense creates no LayoutIndex; its observer cannot pass off a fictitious zero-sized mapping as released storage.
#[derive(Clone)]
pub(super) enum BoundaryResourceObserver {
    Paged(LayoutIndexObserver),
    #[cfg(feature = "paged-shaping-frontier")]
    Dense(dense::DenseObserver),
}
pub(super) struct BoundaryResources {
    pub allocated: bool,
    pub released: bool,
    pub file_open: bool,
    pub mapped_bytes: usize,
    pub peak_mapped_bytes: usize,
    pub buffer_bytes: usize,
    pub peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
}
impl BoundaryResourceObserver {
    pub(super) fn snapshot(&self) -> BoundaryResources {
        match self {
            Self::Paged(observer) => {
                let actual = observer.snapshot();
                BoundaryResources {
                    allocated: true,
                    // A live owner with no records may have neither FD nor buffer. Only actual
                    // close plus successful cleanup establishes that its owner was released.
                    released: actual.owner_closed
                        && !actual.file_open
                        && actual.mapped_bytes == 0
                        && actual.buffer_bytes == 0
                        && !actual.cleanup_failed,
                    file_open: actual.file_open,
                    mapped_bytes: actual.mapped_bytes,
                    peak_mapped_bytes: actual.peak_mapped_bytes,
                    buffer_bytes: actual.buffer_bytes,
                    peak_buffer_bytes: actual.peak_buffer_bytes,
                    cleanup_failed: actual.cleanup_failed,
                }
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Self::Dense(observer) => BoundaryResources {
                allocated: false,
                released: observer.released(),
                file_open: false,
                mapped_bytes: 0,
                peak_mapped_bytes: 0,
                buffer_bytes: 0,
                peak_buffer_bytes: 0,
                cleanup_failed: false,
            },
        }
    }
}
impl From<LayoutIndexError> for Error {
    fn from(value: LayoutIndexError) -> Self {
        match value {
            LayoutIndexError::InvalidRecord => Self::CorruptMetadata,
            LayoutIndexError::Overflow => Self::CoordinateOverflow,
            LayoutIndexError::Poisoned => Self::Poisoned,
            LayoutIndexError::Io(error) => Self::Storage(error),
        }
    }
}
impl BoundaryIndex {
    fn from_representation(inner: Representation) -> Self {
        Self {
            inner,
            reads: 0,
            writes: 0,
            #[cfg(test)]
            fail_record: None,
        }
    }
    pub(super) fn new() -> Result<Self> {
        Ok(Self::from_representation(Representation::Paged(
            PagedLayoutIndex::new()?,
        )))
    }
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn dense_ascii(source_length: usize) -> Result<Self> {
        Ok(Self::from_representation(
            Representation::DensePrintableAscii(dense::DenseIndex::new(source_length)?),
        ))
    }
    #[cfg(test)]
    pub(super) fn with_file(file: std::fs::File) -> Result<Self> {
        Ok(Self::from_representation(Representation::Paged(
            PagedLayoutIndex::test_with_file(file)?,
        )))
    }
    pub(super) fn is_dense(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            matches!(self.inner, Representation::DensePrintableAscii(_))
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }
    pub(super) fn push(&mut self, boundary: Boundary) -> Result<()> {
        let inner = match &mut self.inner {
            Representation::Paged(inner) => inner,
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(_) => return Err(Error::CorruptMetadata),
        }; // Whole-source proof already closes Dense; the streaming machine cannot append or change its character domain.
        #[cfg(test)]
        if self.fail_record == Some(inner.len()) {
            inner.poison();
            return Err(Error::Storage(std::io::Error::other(
                "synthetic metadata write failure",
            )));
        }
        // Persist original byte and UTF-16 coordinates together as fixed little-endian
        // values; failed conversion or append never increments successful-write accounting.
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(
            &u64::try_from(boundary.byte)
                .map_err(|_| Error::CoordinateOverflow)?
                .to_le_bytes(),
        );
        bytes[8..].copy_from_slice(
            &u64::try_from(boundary.utf16)
                .map_err(|_| Error::CoordinateOverflow)?
                .to_le_bytes(),
        );
        let next = self
            .writes
            .checked_add(1)
            .ok_or(Error::CoordinateOverflow)?;
        inner.append(bytes)?;
        self.writes = next; // Count only successful physical records, not logical Dense boundaries.
        Ok(())
    }
    pub(super) fn get(&mut self, record: usize) -> Result<Boundary> {
        let inner = match &mut self.inner {
            Representation::Paged(inner) => inner,
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(inner) => return inner.get(record),
        };
        // Decode only the requested original record. Unrepresentable stored coordinates
        // poison the actual paged owner rather than leaving cached metadata reusable.
        let next = self.reads.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        let bytes = inner.read(record)?;
        self.reads = next;
        let byte = u64::from_le_bytes(bytes[..8].try_into().expect("固定八字节字段"));
        let utf16 = u64::from_le_bytes(bytes[8..].try_into().expect("固定八字节字段"));
        let result = (|| {
            Ok(Boundary {
                byte: usize::try_from(byte).map_err(|_| Error::CoordinateOverflow)?,
                utf16: usize::try_from(utf16).map_err(|_| Error::CoordinateOverflow)?,
            })
        })();
        if result.is_err() {
            inner.poison();
        }
        result
    }

    /// Borrow the original owner's initialized append range without copying a page or inferring its coordinates.
    pub(super) fn append_page_records(&mut self) -> Result<Option<std::ops::Range<usize>>> {
        match &mut self.inner {
            Representation::Paged(inner) => inner.append_page_records().map_err(Into::into),
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(_) => Ok(None), // Dense proof has no physical append page.
        }
    }

    /// Export at most 256 original boundaries: Dense computes integers and Paged reads actual records.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn read_batch(
        &mut self,
        range: std::ops::Range<usize>,
        output: &mut [Boundary],
    ) -> Result<()> {
        if range.end.checked_sub(range.start) != Some(output.len())
            || output.len() > 256
            || range.end > self.len()
        {
            match &mut self.inner {
                Representation::Paged(inner) => inner.poison(),
                #[cfg(feature = "paged-shaping-frontier")]
                Representation::DensePrintableAscii(_) => {}
            }
            return Err(Error::InvalidRange);
        }
        for (record, slot) in range.zip(output.iter_mut()) {
            *slot = self.get(record)?;
        }
        Ok(()) // The outer owner revokes the whole batch on failure; an already written prefix is not publishable.
    }
    pub(super) fn seal(&mut self) -> Result<()> {
        match &mut self.inner {
            Representation::Paged(inner) => inner.seal().map_err(Into::into),
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(inner) => inner.require(),
        }
    }
    pub(super) fn close(&mut self) -> Result<()> {
        match &mut self.inner {
            Representation::Paged(inner) => inner.close().map_err(Into::into),
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(inner) => {
                inner.close();
                Ok(())
            }
        }
    }
    pub(super) fn len(&self) -> usize {
        match &self.inner {
            Representation::Paged(inner) => inner.len(),
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(inner) => inner.len(),
        }
    }
    pub(super) fn physical_records(&self) -> usize {
        match &self.inner {
            Representation::Paged(inner) => inner.len(),
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(_) => 0,
        }
    }
    pub(super) fn io_counts(&self) -> (u64, u64) {
        (self.reads, self.writes)
    }
    pub(super) fn page_loads(&self) -> u64 {
        match &self.inner {
            Representation::Paged(inner) => inner.page_loads(),
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(_) => 0,
        }
    }
    pub(super) fn resource_observer(&self) -> BoundaryResourceObserver {
        match &self.inner {
            Representation::Paged(inner) => {
                BoundaryResourceObserver::Paged(inner.resource_observer())
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Representation::DensePrintableAscii(inner) => {
                BoundaryResourceObserver::Dense(inner.observer())
            }
        }
    }
}
