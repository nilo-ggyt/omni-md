//! Logical clusters and 26.6 advance prefixes for one actual shaping job, retaining only one page
//! of derived metadata in memory.
//!
//! A cluster may combine multiple graphemes; positions inside it do not become safe boundaries for
//! glyph reuse. This index decides neither soft lines, reshaping, dictionaries, font switching, nor
//! carets; consumers must complete their own actual stages.

use std::{ops::Range, sync::Weak};

use thiserror::Error;

use crate::{
    MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority, MarkdownShapedGlyph,
    MarkdownShapingDirection, MarkdownShapingError, MarkdownShapingJob,
    markdown_layout_index::{LayoutIndexError, LayoutIndexObserver, PagedLayoutIndex},
};

/// Maximum actual glyphs consumed per call. Even an arbitrarily long cluster retains the same
/// pending state across yields instead of being scanned to completion in one call.
pub const MARKDOWN_GLYPH_CLUSTER_STEP: usize = 256;

#[derive(Debug, Error)]
pub enum MarkdownGlyphClusterError {
    #[error("字形cluster索引已经撤权")]
    Poisoned,
    #[error("字形cluster索引不属于此任务")]
    JobMismatch,
    #[error("字形cluster配额或查询范围无效")]
    InvalidRequest,
    #[error("字形cluster坐标、计数或累计advance溢出")]
    Overflow,
    #[error("字形cluster顺序或元数据与原任务不一致")]
    InconsistentGlyphs,
    #[error("无法访问原字形任务：{0}")]
    Shaping(#[from] MarkdownShapingError),
    #[error("字形cluster分页操作失败：{0}")]
    Storage(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, MarkdownGlyphClusterError>;

impl From<LayoutIndexError> for MarkdownGlyphClusterError {
    fn from(error: LayoutIndexError) -> Self {
        match error {
            LayoutIndexError::InvalidRecord => Self::InconsistentGlyphs,
            LayoutIndexError::Overflow => Self::Overflow,
            LayoutIndexError::Poisoned => Self::Poisoned,
            LayoutIndexError::Io(error) => Self::Storage(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGlyphClusterStatus {
    Building,
    Ready,
    Cancelled,
    Poisoned,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Original global UTF-16 bounds and half-open visual glyph bounds. Prefix advances accumulate in
/// logical source order without renumbering the original visual array.
pub struct MarkdownGlyphCluster {
    pub source_utf16: Range<usize>,
    pub glyphs: Range<usize>,
    pub prefix_x_26_6: i64,
    pub prefix_y_26_6: i64,
    pub x_advance_26_6: i64,
    pub y_advance_26_6: i64,
    /// Preserve the original flags' union; unsafe-to-break/concat never grants a safe boundary.
    pub flags: u32,
    pub missing_glyphs: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownGlyphClusterAnswer {
    Pending,
    Known(Option<MarkdownGlyphCluster>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGlyphClusterStep {
    pub glyphs: usize,
    pub clusters: usize,
    pub status: MarkdownGlyphClusterStatus,
}

#[derive(Clone)]
/// Observe actual descriptor/mapping/write-buffer facts without retaining the index, job, font,
/// or original text. These owned-resource receipts are not kernel RSS measurements.
pub struct MarkdownGlyphClusterResourceObserver(LayoutIndexObserver);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGlyphClusterResources {
    /// The original index writer actually closed. A private single page may have no File, but a
    /// live owner must not be reported as completed solely because its descriptor is absent.
    pub owner_closed: bool,
    pub file_open: bool,
    pub mapped_bytes: usize,
    pub peak_mapped_bytes: usize,
    /// Unique metadata write buffer; after the reader returns this buffer, only its original File remains.
    pub buffer_bytes: usize,
    pub peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
}
impl MarkdownGlyphClusterResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownGlyphClusterResources {
        let value = self.0.snapshot();
        MarkdownGlyphClusterResources {
            owner_closed: value.owner_closed,
            file_open: value.file_open,
            mapped_bytes: value.mapped_bytes,
            peak_mapped_bytes: value.peak_mapped_bytes,
            buffer_bytes: value.buffer_bytes,
            peak_buffer_bytes: value.peak_buffer_bytes,
            cleanup_failed: value.cleanup_failed,
        }
    }
}

struct Payload {
    index: PagedLayoutIndex<64>,
    pending: Option<MarkdownGlyphCluster>,
}

/// Retain no strong edge to the original job. Every advance/read/EOF query must borrow the same
/// Ready shaping task that created this index.
pub struct MarkdownGlyphClusterIndex {
    payload: Option<Payload>,
    identity: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    source_range: Range<usize>,
    glyph_count: usize,
    backwards: bool,
    consumed: usize,
    committed_end: usize,
    prefix_x: i64,
    prefix_y: i64,
    status: MarkdownGlyphClusterStatus,
    resources: MarkdownGlyphClusterResourceObserver,
}

impl MarkdownGlyphClusterIndex {
    pub fn new(
        job: &mut MarkdownShapingJob,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Self> {
        // Profile checks the actual job, then an EOF glyph read validates the caller's original
        // Source/font authorities. A job that is not Ready cannot create this derived index.
        let profile = job.profile()?;
        job.glyph(source, font, profile.glyph_count)?;
        let backwards = matches!(
            job.direction(),
            MarkdownShapingDirection::RightToLeft | MarkdownShapingDirection::BottomToTop
        );
        // The fixed index owner retains only derived records and one pending cluster; the job
        // continues to own all actual glyph storage and Source/font lifetimes.
        let index = PagedLayoutIndex::new()?;
        let resources = MarkdownGlyphClusterResourceObserver(index.resource_observer());
        let source_range = job.source_range();
        Ok(Self {
            payload: Some(Payload {
                index,
                pending: None,
            }),
            identity: job.index_identity(),
            source: source.clone(),
            font: font.clone(),
            committed_end: source_range.start,
            source_range,
            glyph_count: profile.glyph_count,
            backwards,
            consumed: 0,
            prefix_x: 0,
            prefix_y: 0,
            status: MarkdownGlyphClusterStatus::Building,
            resources,
        })
    }

    fn guard(&self, job: &mut MarkdownShapingJob) -> Result<()> {
        if !Weak::ptr_eq(&self.identity, &job.index_identity()) {
            return Err(MarkdownGlyphClusterError::JobMismatch);
        }
        // After same-job identity, actual Source/font failure takes precedence over local index
        // status; old metadata cannot conceal a failed shared source, even for EOF queries.
        if job
            .glyph(&self.source, &self.font, self.glyph_count)?
            .is_some()
        {
            return Err(MarkdownGlyphClusterError::InconsistentGlyphs);
        }
        if self.payload.is_none() {
            return Err(MarkdownGlyphClusterError::Poisoned);
        }
        Ok(())
    }

    fn fail<T>(&mut self, error: MarkdownGlyphClusterError) -> Result<T> {
        self.status = MarkdownGlyphClusterStatus::Poisoned;
        drop(self.payload.take()); // Return this index's actual storage before reporting the cause; retain no partial publishable result.
        Err(error)
    }

    /// Read complete original glyph records and group their actual clusters in logical order.
    /// A step boundary never commits an unfinished cluster.
    pub fn step(
        &mut self,
        job: &mut MarkdownShapingJob,
        budget: usize,
    ) -> Result<MarkdownGlyphClusterStep> {
        let result = self.advance(job, budget);
        match result {
            Ok(step) => Ok(step),
            Err(error) => self.fail(error),
        }
    }

    fn advance(
        &mut self,
        job: &mut MarkdownShapingJob,
        budget: usize,
    ) -> Result<MarkdownGlyphClusterStep> {
        self.guard(job)?;
        if budget == 0 {
            return Err(MarkdownGlyphClusterError::InvalidRequest);
        }
        let before = self.count();
        let mut read = 0;
        while self.consumed < self.glyph_count && read < budget.min(MARKDOWN_GLYPH_CLUSTER_STEP) {
            // RTL/BTT only traverse the original visual array backward; do not reverse glyph
            // contents or allocate a sorting vector.
            let at = if self.backwards {
                self.glyph_count - self.consumed - 1
            } else {
                self.consumed
            };
            let glyph = job
                .glyph(&self.source, &self.font, at)?
                .ok_or(MarkdownGlyphClusterError::InconsistentGlyphs)?;
            self.push(at, glyph)?;
            self.consumed += 1;
            read += 1;
        }
        if self.consumed == self.glyph_count && self.status != MarkdownGlyphClusterStatus::Ready {
            // Only the actual glyph-array EOF finalizes the last cluster and any source interval
            // whose default-ignorable characters the shaper removed.
            self.commit(self.source_range.end)?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownGlyphClusterError::Poisoned)?;
            payload.index.seal()?;
            self.guard(job)?; // Recheck the original job after seal's actual I/O, rather than inferring Ready from counts alone.
            self.status = MarkdownGlyphClusterStatus::Ready;
        }
        Ok(MarkdownGlyphClusterStep {
            glyphs: read,
            clusters: self.count() - before,
            status: self.status,
        })
    }

    fn push(&mut self, at: usize, glyph: MarkdownShapedGlyph) -> Result<()> {
        if !self.source_range.contains(&glyph.source_utf16_cluster) {
            return Err(MarkdownGlyphClusterError::InconsistentGlyphs);
        }
        let previous = self
            .payload
            .as_ref()
            .and_then(|payload| payload.pending.as_ref())
            .map(|cluster| cluster.source_utf16.start);
        if let Some(previous) = previous {
            if glyph.source_utf16_cluster < previous {
                return Err(MarkdownGlyphClusterError::InconsistentGlyphs); // The original MonotoneGraphemes direction contract was violated.
            }
            if glyph.source_utf16_cluster > previous {
                self.commit(glyph.source_utf16_cluster)?;
            }
        } else if self.committed_end < glyph.source_utf16_cluster {
            // Removed leading default-ignorables retain actual source coordinates but no drawable
            // glyph; record that interval without fabricating output.
            self.commit(glyph.source_utf16_cluster)?;
        }
        let pending = self
            .payload
            .as_mut()
            .ok_or(MarkdownGlyphClusterError::Poisoned)?
            .pending
            .get_or_insert_with(|| MarkdownGlyphCluster {
                source_utf16: glyph.source_utf16_cluster..glyph.source_utf16_cluster,
                glyphs: at..at + 1,
                prefix_x_26_6: self.prefix_x,
                prefix_y_26_6: self.prefix_y,
                x_advance_26_6: 0,
                y_advance_26_6: 0,
                flags: 0,
                missing_glyphs: 0,
            });
        // A cluster's glyphs remain contiguous in the original array; reverse traversal does not
        // change their actual half-open visual range.
        pending.glyphs.start = pending.glyphs.start.min(at);
        pending.glyphs.end = pending.glyphs.end.max(at + 1);
        pending.x_advance_26_6 = pending
            .x_advance_26_6
            .checked_add(i64::from(glyph.x_advance))
            .ok_or(MarkdownGlyphClusterError::Overflow)?;
        pending.y_advance_26_6 = pending
            .y_advance_26_6
            .checked_add(i64::from(glyph.y_advance))
            .ok_or(MarkdownGlyphClusterError::Overflow)?;
        pending.flags |= glyph.flags;
        pending.missing_glyphs += usize::from(glyph.glyph_id == 0);
        Ok(())
    }

    fn commit(&mut self, end: usize) -> Result<()> {
        // A real next-cluster boundary or final EOF closes this pending source interval. With no
        // pending glyphs, preserve its actual zero-glyph extent and unchanged signed prefix.
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownGlyphClusterError::Poisoned)?;
        let mut value = payload.pending.take().unwrap_or(MarkdownGlyphCluster {
            source_utf16: self.committed_end..self.committed_end,
            glyphs: if self.backwards {
                self.glyph_count..self.glyph_count
            } else {
                0..0
            },
            prefix_x_26_6: self.prefix_x,
            prefix_y_26_6: self.prefix_y,
            x_advance_26_6: 0,
            y_advance_26_6: 0,
            flags: 0,
            missing_glyphs: 0,
        });
        if end == value.source_utf16.start && self.source_range.is_empty() {
            return Ok(()); // An empty run has no cluster; Ready separately expresses actual EOF.
        }
        if value.source_utf16.start != self.committed_end
            || end <= self.committed_end
            || end > self.source_range.end
        {
            return Err(MarkdownGlyphClusterError::InconsistentGlyphs);
        }
        value.source_utf16.end = end;
        // Preserve signed advances. Negative kerning cannot be clamped to zero or treated as a
        // proof of monotonic widths; overflow rejects this candidate.
        let next_x = self
            .prefix_x
            .checked_add(value.x_advance_26_6)
            .ok_or(MarkdownGlyphClusterError::Overflow)?;
        let next_y = self
            .prefix_y
            .checked_add(value.y_advance_26_6)
            .ok_or(MarkdownGlyphClusterError::Overflow)?;
        payload.index.append(encode(&value)?)?;
        self.prefix_x = next_x;
        self.prefix_y = next_y;
        self.committed_end = end;
        Ok(())
    }

    /// Deliver sealed records in original logical source order without making a caller binary-search
    /// the same record again when its actual ordinal is already known.
    #[cfg(any(feature = "paged-line-compose", test))]
    pub(crate) fn at_ordinal(
        &mut self,
        job: &mut MarkdownShapingJob,
        ordinal: usize,
    ) -> Result<Option<MarkdownGlyphCluster>> {
        let result = (|| {
            self.guard(job)?; // The same original job's Source/font failure precedes local Ready/ordinal decisions.
            if self.status != MarkdownGlyphClusterStatus::Ready {
                return Err(MarkdownGlyphClusterError::InvalidRequest);
            } // A committed prefix cannot grant complete ordinal access to an unsealed pending tail.
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownGlyphClusterError::Poisoned)?;
            if ordinal > payload.index.len() {
                return Err(MarkdownGlyphClusterError::InvalidRequest);
            } // Only the exact record endpoint is EOF; positions beyond it cannot masquerade as absence.
            let value = if ordinal == payload.index.len() {
                None
            } else {
                // Read one original fixed record. RTL and zero-glyph intervals keep actual commit
                // order instead of inferred cluster numbering.
                Some(decode(payload.index.read(ordinal)?)?)
            };
            self.guard(job)?; // Revalidate after page I/O/return; even EOF cannot bypass real Source revocation.
            Ok(value)
        })();
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail(error), // Return the actual cause and dispose this index's storage immediately.
        }
    }

    /// Find the actual shaping cluster containing one original global UTF-16 unit. The unfinished
    /// tail remains Pending, even when an earlier prefix is already committed.
    pub fn containing(
        &mut self,
        job: &mut MarkdownShapingJob,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        let result = self.containing_inner(job, offset);
        match result {
            Ok(answer) => Ok(answer),
            Err(error) => self.fail(error),
        }
    }

    fn containing_inner(
        &mut self,
        job: &mut MarkdownShapingJob,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        self.guard(job)?;
        if offset < self.source_range.start || offset > self.source_range.end {
            return Err(MarkdownGlyphClusterError::InvalidRequest);
        }
        if offset >= self.committed_end {
            // Completed EOF proves no record. Before seal, the same coordinate cannot establish
            // an empty document or a completed tail.
            return Ok(if self.status == MarkdownGlyphClusterStatus::Ready {
                MarkdownGlyphClusterAnswer::Known(None)
            } else {
                MarkdownGlyphClusterAnswer::Pending
            });
        }
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownGlyphClusterError::Poisoned)?;
        let mut low = 0;
        let mut high = payload.index.len();
        while low < high {
            // Committed source ranges strictly increase, bounding binary search by usize::BITS
            // without a glyph-count-sized in-memory directory.
            let middle = low + (high - low) / 2;
            let value = decode(payload.index.read(middle)?)?;
            if offset < value.source_utf16.start {
                high = middle;
            } else if offset >= value.source_utf16.end {
                low = middle + 1;
            } else {
                self.guard(job)?; // Check the original job after actual page access, never returning revoked cached metadata.
                return Ok(MarkdownGlyphClusterAnswer::Known(Some(value)));
            }
        }
        Err(MarkdownGlyphClusterError::InconsistentGlyphs)
    }

    fn count(&self) -> usize {
        self.payload
            .as_ref()
            .map_or(0, |payload| payload.index.len())
    }
    #[must_use]
    pub fn status(&self) -> MarkdownGlyphClusterStatus {
        self.status
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownGlyphClusterResourceObserver {
        self.resources.clone()
    }
    pub fn cancel(&mut self) {
        if self.payload.is_some() {
            self.status = MarkdownGlyphClusterStatus::Cancelled;
            drop(self.payload.take()); // Return derived metadata now; the actual job owner independently controls its lifetime.
        }
    }
}

fn encode(value: &MarkdownGlyphCluster) -> Result<[u8; 64]> {
    // Encode fixed scalar fields without storing addresses or text. Check narrowing to u32 before
    // a record can enter storage; signed 26.6 advances retain their full i64 representation.
    let mut bytes = [0; 64];
    for (slot, value) in [
        value.source_utf16.start,
        value.source_utf16.end,
        value.glyphs.start,
        value.glyphs.end,
        value.missing_glyphs,
    ]
    .into_iter()
    .enumerate()
    {
        let value = u32::try_from(value).map_err(|_| MarkdownGlyphClusterError::Overflow)?;
        bytes[slot * 4..slot * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[20..24].copy_from_slice(&value.flags.to_le_bytes());
    for (slot, value) in [
        value.prefix_x_26_6,
        value.prefix_y_26_6,
        value.x_advance_26_6,
        value.y_advance_26_6,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[24 + slot * 8..32 + slot * 8].copy_from_slice(&value.to_le_bytes());
    }
    Ok(bytes) // Reserve the final eight bytes as zero, without pointers, body text, or unstable enum layout.
}

fn decode(bytes: [u8; 64]) -> Result<MarkdownGlyphCluster> {
    // Decode the same fixed layout and reject inconsistent ranges/counts or nonzero reserved bytes;
    // malformed storage cannot become an authorized semantic cluster.
    let small = |slot: usize| {
        usize::try_from(u32::from_le_bytes(
            bytes[slot * 4..slot * 4 + 4]
                .try_into()
                .expect("固定4B字段"),
        ))
        .map_err(|_| MarkdownGlyphClusterError::Overflow)
    };
    let signed = |slot: usize| {
        i64::from_le_bytes(
            bytes[24 + slot * 8..32 + slot * 8]
                .try_into()
                .expect("固定8B字段"),
        )
    };
    let value = MarkdownGlyphCluster {
        source_utf16: small(0)?..small(1)?,
        glyphs: small(2)?..small(3)?,
        missing_glyphs: small(4)?,
        flags: u32::from_le_bytes(bytes[20..24].try_into().expect("固定4B字段")),
        prefix_x_26_6: signed(0),
        prefix_y_26_6: signed(1),
        x_advance_26_6: signed(2),
        y_advance_26_6: signed(3),
    };
    if value.source_utf16.start >= value.source_utf16.end
        || value.glyphs.start > value.glyphs.end
        || value.missing_glyphs > value.glyphs.len()
        || bytes[56..].iter().any(|byte| *byte != 0)
    {
        return Err(MarkdownGlyphClusterError::InconsistentGlyphs);
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
