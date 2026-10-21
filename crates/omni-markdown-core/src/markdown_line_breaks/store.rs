//! Line-break-specific 32-byte opportunity encoding. Shared page I/O handles
//! fixed records without interpreting this format or reading document text.
use super::{
    MarkdownLineBreakBoundary as Boundary, MarkdownLineBreakError as Error,
    MarkdownLineBreakKind as Kind, Result, machine,
};
use crate::markdown_layout_index::{LayoutIndexObserver, PagedLayoutIndex};

pub(super) struct LineBreakIndex {
    page: PagedLayoutIndex<32>,
    last: Option<Boundary>,
}
impl LineBreakIndex {
    /// Acquires one actual paged anonymous index, initially with no published
    /// opportunity or invented endpoint for the source.
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            page: PagedLayoutIndex::new()?,
            last: None,
        })
    }
    /// Substitutes a real descriptor only for storage-permission regressions;
    /// the same record encoding and actual paged operations still execute.
    #[cfg(test)]
    pub(super) fn test_with_file(file: std::fs::File) -> Result<Self> {
        Ok(Self {
            page: PagedLayoutIndex::test_with_file(file)?,
            last: None,
        })
    }
    /// Encodes a real machine opportunity in strictly increasing byte and UTF-16
    /// order. The remembered endpoint changes only after append succeeds.
    pub(super) fn push(&mut self, event: machine::Break, dictionary: bool) -> Result<()> {
        let byte = usize::try_from(event.byte_offset).map_err(|_| Error::CoordinateOverflow)?;
        let utf16 = usize::try_from(event.utf16_offset).map_err(|_| Error::CoordinateOverflow)?;
        if self
            .last
            .is_some_and(|old| old.byte >= byte || old.utf16 >= utf16)
        {
            return Err(Error::CorruptMetadata);
        }
        // Persist the actual rule/kind and a conservative dictionary-needed fact.
        // An unsupported rule cannot be silently encoded as an ordinary allowed break.
        let mut encoded = [0; 32];
        encoded[..8].copy_from_slice(&event.byte_offset.to_le_bytes());
        encoded[8..16].copy_from_slice(&event.utf16_offset.to_le_bytes());
        encoded[16] = match event.kind {
            Kind::Allowed => 0,
            Kind::Mandatory => 1,
        };
        encoded[17] = u8::from(dictionary);
        encoded[18] = 1; // Version and reserved bytes prevent interpreting another record format as a break.
        encoded[19] = match event.rule {
            "LB3" => 3,
            "LB4" => 4,
            "LB5" => 5,
            "LB8" => 8,
            "LB15c" => 15,
            "LB18" => 18,
            "LB20" => 20,
            "LB31" => 31,
            _ => return Err(Error::CorruptMetadata),
        };
        self.page.append(encoded)?;
        self.last = Some(Boundary {
            byte,
            utf16,
            kind: event.kind,
            needs_dictionary: dictionary,
        });
        Ok(())
    }
    /// Decodes one original record, rejecting unknown tags/versions/reserved
    /// bytes rather than presenting corrupt metadata as a valid text boundary.
    pub(super) fn get(&mut self, index: usize) -> Result<Boundary> {
        let bytes = self.page.read(index)?;
        let kind = match bytes[16] {
            0 => Kind::Allowed,
            1 => Kind::Mandatory,
            _ => return Err(Error::CorruptMetadata),
        };
        if bytes[17] > 1
            || bytes[18] != 1
            || !matches!(bytes[19], 3 | 4 | 5 | 8 | 15 | 18 | 20 | 31)
            || bytes[20..].iter().any(|byte| *byte != 0)
        {
            return Err(Error::CorruptMetadata);
        }
        Ok(Boundary {
            byte: usize::try_from(u64::from_le_bytes(
                bytes[..8].try_into().expect("固定8B字段"),
            ))
            .map_err(|_| Error::CoordinateOverflow)?,
            utf16: usize::try_from(u64::from_le_bytes(
                bytes[8..16].try_into().expect("固定8B字段"),
            ))
            .map_err(|_| Error::CoordinateOverflow)?,
            kind,
            needs_dictionary: bytes[17] == 1,
        })
    }
    /// Returns completed opportunity count without reading a page or the source.
    pub(super) fn len(&self) -> usize {
        self.page.len()
    }

    /// Borrows completed record bounds from the original sole append page without
    /// copying it or treating unused capacity as line-break opportunities.
    /// `get` still validates each actual record. Older pages retain their original
    /// descriptor, first-failure, and revocation ownership rules.
    pub(super) fn append_page_records(&mut self) -> Result<Option<std::ops::Range<usize>>> {
        self.page.append_page_records().map_err(Into::into)
    }

    /// Reports actual derived-index I/O, separate from source-byte consumption.
    pub(super) fn page_loads(&self) -> u64 {
        self.page.page_loads()
    }
    /// Permanently revokes this failed derived index, including cached-page hits.
    pub(super) fn poison(&mut self) {
        self.page.poison();
    }
    /// Completes actual append storage before the source job may advertise Ready.
    pub(super) fn seal(&mut self) -> Result<()> {
        self.page.seal().map_err(Into::into)
    }
    /// Explicitly returns this owner's file/page resources and exposes a real
    /// cleanup failure instead of relying on an unreportable Drop result.
    pub(super) fn close(&mut self) -> Result<()> {
        self.page.close().map_err(Into::into)
    }
    /// Retains fixed release facts only, without prolonging index or text lifetime.
    pub(super) fn resource_observer(&self) -> LayoutIndexObserver {
        self.page.resource_observer()
    }
}
