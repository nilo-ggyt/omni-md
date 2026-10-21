//! Deliver original Ready output by copying fixed glyph records sequentially, without copying text or transferring mutable shaper arrays.
//!
//! This proves immutable output origin only; Native still checks its active document, generation, and input authority.
//! After sealing, workers return original Jobs while the receiving thread owns the anonymous File and necessary shared Source/Font edges.

use super::{
    MarkdownShapedGlyph, MarkdownShapingCancellation, MarkdownShapingDirection,
    MarkdownShapingError, MarkdownShapingJob, MarkdownShapingStatus,
};
use crate::{
    MarkdownDocumentBufferError, MarkdownLayoutFont, MarkdownLayoutFontAuthority,
    MarkdownLayoutFontResourceObserver, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLayoutTextSourceObserver, MarkdownOriginalBackingObserver,
};
use std::{
    ops::Range,
    sync::{Arc, Weak},
};
use thiserror::Error;

mod store;
#[cfg(test)]
mod tests;
use store::Store;
pub use store::{MarkdownShapedSpoolIoError, MarkdownShapedSpoolIoOperation};

/// Build one finite batch per call rather than implicitly scanning an arbitrarily long run in one main-thread call.
pub const MARKDOWN_SHAPED_SPOOL_STEP: usize = 256;
const RECORD: usize = 32;

#[derive(Debug, Error)]
pub enum MarkdownShapedSpoolError {
    #[error("只读字形不属于原任务输出")]
    OutputMismatch,
    #[error("只读字形Source权限不匹配")]
    SourceMismatch,
    #[error("只读字形Font权限不匹配")]
    FontMismatch,
    #[error("只读字形配额、坐标或记录无效")]
    InvalidRequest,
    #[error("只读字形尚未完成全部写入")]
    Pending,
    #[error("只读字形已取消")]
    Cancelled,
    #[error("只读字形已永久撤权")]
    Poisoned,
    #[error("原字形任务不可用：{0}")]
    Shaping(#[from] MarkdownShapingError),
    #[error("原只读字形Source不可用：{0}")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("只读字形文件操作失败：{0}")]
    Storage(#[from] MarkdownShapedSpoolIoError),
    #[cfg(feature = "paged-safe-glyph-slices")]
    #[error("原安全字形切片已失权：{0}")]
    SafeSlice(#[from] crate::MarkdownSafeGlyphSliceError),
}
type Result<T> = std::result::Result<T, MarkdownShapedSpoolError>;

/// Weak authority for the original Job output nonce; another job with equal text/font/range cannot acquire it.
#[derive(Clone)]
pub struct MarkdownShapedSpoolAuthority(Weak<()>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownShapedSpoolStatus {
    Building,
    AwaitingSeal,
    Ready,
    Cancelled,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapedSpoolStep {
    pub glyphs: usize,
    pub status: MarkdownShapedSpoolStatus,
}

/// Fixed receipts retain no File, Source, Font, or Job; other font-Blob aliases require their own VM release observations.
#[derive(Clone)]
pub struct MarkdownShapedSpoolResourceObserver {
    file: store::Observer,
    source: MarkdownLayoutTextSourceObserver,
    font: MarkdownLayoutFontResourceObserver,
    backing: Option<MarkdownOriginalBackingObserver>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapedSpoolResources {
    pub file_open: bool,
    pub close_succeeded: bool,
    pub cleanup_failed: bool,
    pub first_error: Option<MarkdownShapedSpoolIoError>,
    pub source_payload_released: bool,
    pub font_payload_released: bool,
}
impl MarkdownShapedSpoolResourceObserver {
    #[must_use]
    /// Multiple atomics are not a live transactional snapshot; inspect final resource combinations after worker join/owner close.
    pub fn snapshot(&self) -> MarkdownShapedSpoolResources {
        let file = self.file.snapshot();
        MarkdownShapedSpoolResources {
            file_open: file.file_open,
            close_succeeded: file.close_succeeded,
            cleanup_failed: file.cleanup_failed,
            first_error: file.first_error,
            source_payload_released: self.source.payload_released(),
            font_payload_released: self.font.payload_released(),
        }
    }
    fn failure(&self) -> Result<()> {
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        } // Original Source failure precedes file/cancellation errors; observing its first cause does not retain the mapping.
        if let Some(error) = self.file.snapshot().first_error {
            return Err(error.into());
        }
        Ok(())
    }
}

struct Origin {
    nonce: Arc<()>,
    source: MarkdownLayoutTextSource,
    source_authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    cancellation: MarkdownShapingCancellation,
    #[cfg(feature = "paged-safe-glyph-slices")]
    measure_cancellation: Option<crate::MarkdownSoftLineMeasureCancellation>,
}
impl Origin {
    fn permits(
        &self,
        authority: &MarkdownShapedSpoolAuthority,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<()> {
        self.source.health()?;
        // Original output still obeys the same cancellation bit after seal; constructing another reader cannot bypass it.
        if self.cancellation.requested() {
            return Err(MarkdownShapedSpoolError::Cancelled);
        }
        #[cfg(feature = "paged-safe-glyph-slices")]
        if self
            .measure_cancellation
            .as_ref()
            .is_some_and(|value| value.is_cancelled())
        {
            return Err(MarkdownShapedSpoolError::Cancelled);
        } // Sealing transfers data ownership without erasing explicit cancellation of the measure that issued this line.
        // Weak tickets compare original allocation identity, not equal text/range/font in place of the original job nonce.
        if !Weak::ptr_eq(&authority.0, &Arc::downgrade(&self.nonce)) {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        // Same frozen-Source ownership grants read eligibility; the Native caller separately checks the active document's newer generation.
        if !self.source.permits(source) || !self.source.permits(&self.source_authority) {
            return Err(MarkdownShapedSpoolError::SourceMismatch);
        }
        // Preserve exact Font payload identity; PSName/axis comparisons cannot authorize another resolved instance.
        if !self.font.permits(font) || !self.font.permits(&self.font_authority) {
            return Err(MarkdownShapedSpoolError::FontMismatch);
        }
        Ok(())
    }
}
struct Payload {
    file: Store,
    origin: Origin,
}

/// Advance the builder in batches on its original worker; only the distinct fully sealed type can read glyph records.
pub struct MarkdownShapedSpoolBuilder {
    payload: Option<Payload>,
    resources: MarkdownShapedSpoolResourceObserver,
    authority: MarkdownShapedSpoolAuthority,
    source_range: Range<usize>,
    count: usize,
    job_count: usize,
    glyph_start: usize,
    #[cfg(feature = "paged-safe-glyph-slices")]
    safe_slice: Option<crate::MarkdownSafeGlyphSlice>,
    consumed: usize,
    generation: u64,
    direction: MarkdownShapingDirection,
    points: f32,
    status: MarkdownShapedSpoolStatus,
}

impl MarkdownShapedSpoolBuilder {
    /// Check actual original Ready/EOF before creating a unique FD; construction neither shapes, copies text, nor certifies Native geometry.
    pub fn new(
        job: &mut MarkdownShapingJob,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Self> {
        let profile = job.profile()?;
        if job.status() != MarkdownShapingStatus::Ready {
            return Err(MarkdownShapedSpoolError::Pending); // Before Ready, create no anonymous FD and lend no partial Buffer.
        }
        job.glyph(source, font, profile.glyph_count)?;
        let value = job
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        let file = Store::new()?;
        let resources = MarkdownShapedSpoolResourceObserver {
            file: file.observer(),
            source: value.source.resource_observer(),
            font: value.font.resource_observer(),
            backing: value.source.original_backing_observer(),
        };
        let authority = MarkdownShapedSpoolAuthority(job.index_identity());
        Ok(Self {
            payload: Some(Payload {
                file,
                origin: Origin {
                    nonce: Arc::clone(&job.identity),
                    source: value.source.clone(),
                    source_authority: source.clone(),
                    font: value.font.clone(),
                    font_authority: font.clone(),
                    cancellation: job.cancellation(),
                    #[cfg(feature = "paged-safe-glyph-slices")]
                    measure_cancellation: None,
                },
            }),
            resources,
            authority,
            source_range: job.source_range(),
            count: profile.glyph_count,
            job_count: profile.glyph_count,
            glyph_start: 0,
            #[cfg(feature = "paged-safe-glyph-slices")]
            safe_slice: None,
            consumed: 0,
            generation: profile.source_generation,
            direction: job.direction(),
            points: job.points(),
            status: if profile.glyph_count == 0 {
                MarkdownShapedSpoolStatus::AwaitingSeal
            } else {
                MarkdownShapedSpoolStatus::Building
            },
        })
    }

    /// Consume the original factory's safe-subrange ticket, not a public Range, without another shaper or copying the remaining run.
    #[cfg(feature = "paged-safe-glyph-slices")]
    pub fn from_safe_slice(
        job: &mut MarkdownShapingJob,
        mut slice: crate::MarkdownSafeGlyphSlice,
    ) -> Result<Self> {
        slice.require(job)?; // Check the actual owner before FD creation; unready/revoked candidates acquire no output resources.
        let facts = slice.facts();
        let mut builder = Self::new(job, &slice.source, &slice.font)?;
        builder.source_range = facts.source_utf16;
        builder.glyph_start = facts.visual_glyphs.start;
        builder.count = facts.visual_glyphs.len();
        builder.status = if builder.count == 0 {
            MarkdownShapedSpoolStatus::AwaitingSeal
        } else {
            MarkdownShapedSpoolStatus::Building
        };
        builder
            .payload
            .as_mut()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?
            .origin
            .measure_cancellation = Some(slice.cancellation());
        builder.safe_slice = Some(slice);
        if let Err(error) = builder.guard(job) {
            return builder.fail(error);
        }
        Ok(builder)
    }

    fn guard(&mut self, job: &mut MarkdownShapingJob) -> Result<()> {
        self.resources.failure()?;
        #[cfg(feature = "paged-safe-glyph-slices")]
        if let Some(slice) = self.safe_slice.as_mut() {
            slice.require(job)?;
        }
        let value = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        value.origin.permits(
            &self.authority,
            &value.origin.source_authority,
            &value.origin.font_authority,
        )?;
        // A Rust-movable builder still requires its original Job to continue writing; another Ready result cannot substitute.
        if !Arc::ptr_eq(&value.origin.nonce, &job.identity) {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        job.glyph(
            &value.origin.source_authority,
            &value.origin.font_authority,
            self.job_count,
        )?;
        let profile = job.profile()?;
        // Profile is the original job's aggregate; changed facts cannot be combined with an already written prefix into a complete file.
        if profile.glyph_count != self.job_count || profile.source_generation != self.generation {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        Ok(())
    }
    fn fail<T>(&mut self, error: MarkdownShapedSpoolError) -> Result<T> {
        self.status = if matches!(error, MarkdownShapedSpoolError::Cancelled) {
            MarkdownShapedSpoolStatus::Cancelled
        } else {
            MarkdownShapedSpoolStatus::Poisoned
        };
        drop(self.payload.take()); // End borrows before actual close and returning required Source/Font strong edges.
        if let Err(cause) = self.resources.failure() {
            self.status = MarkdownShapedSpoolStatus::Poisoned;
            return Err(cause); // Actual first I/O failure observed during Drop cannot be hidden by clean cancellation.
        }
        Err(error)
    }

    /// Use an explicit original-worker batch budget; failure destroys the candidate while independent observers retain actual FD-return receipts.
    pub fn step(
        &mut self,
        job: &mut MarkdownShapingJob,
        budget: usize,
    ) -> Result<MarkdownShapedSpoolStep> {
        let result = (|| {
            self.guard(job)?;
            if budget == 0 || budget > MARKDOWN_SHAPED_SPOOL_STEP {
                return Err(MarkdownShapedSpoolError::InvalidRequest);
            }
            let count = budget.min(self.count - self.consumed);
            let mut bytes = [0u8; RECORD * MARKDOWN_SHAPED_SPOOL_STEP];
            for offset in 0..count {
                let value = self
                    .payload
                    .as_ref()
                    .ok_or(MarkdownShapedSpoolError::Poisoned)?;
                let glyph = job
                    .glyph(
                        &value.origin.source_authority,
                        &value.origin.font_authority,
                        self.glyph_start + self.consumed + offset,
                    )?
                    .ok_or(MarkdownShapedSpoolError::InvalidRequest)?;
                if !self.source_range.contains(&glyph.source_utf16_cluster) {
                    return Err(MarkdownShapedSpoolError::OutputMismatch);
                } // Subranges retain global source clusters; RTL selects original visual intervals without renumbering or discarding invalid records.
                bytes[offset * RECORD..(offset + 1) * RECORD].copy_from_slice(&encode(glyph)?);
            } // The stack holds at most 256 actual seven-field glyphs; a failed prefix cannot be written as a completed batch.
            self.guard(job)?;
            self.payload
                .as_mut()
                .ok_or(MarkdownShapedSpoolError::Poisoned)?
                .file
                .append(&bytes[..count * RECORD])?;
            self.guard(job)?;
            self.consumed += count;
            if self.consumed == self.count {
                self.status = MarkdownShapedSpoolStatus::AwaitingSeal;
            }
            Ok(MarkdownShapedSpoolStep {
                glyphs: count,
                status: self.status,
            })
        })();
        match result {
            Ok(step) => Ok(step),
            Err(error) => self.fail(error),
        }
    }

    /// Consume the completed writer; callers still return the original Job and join its worker, since seal does not prove job-resource release.
    pub fn seal(mut self, job: &mut MarkdownShapingJob) -> Result<MarkdownShapedSpool> {
        let result = (|| {
            self.guard(job)?;
            if self.status != MarkdownShapedSpoolStatus::AwaitingSeal || self.consumed != self.count
            {
                return Err(MarkdownShapedSpoolError::Pending);
            }
            let bytes = self
                .count
                .checked_mul(RECORD)
                .ok_or(MarkdownShapedSpoolError::InvalidRequest)?;
            self.payload
                .as_mut()
                .ok_or(MarkdownShapedSpoolError::Poisoned)?
                .file
                .seal(bytes)?;
            self.guard(job)?; // Recheck original Job/token after synchronous I/O; revocation cannot issue a read-only owner.
            Ok(())
        })();
        if let Err(error) = result {
            return self.fail(error);
        }
        Ok(MarkdownShapedSpool {
            payload: self.payload.take(),
            #[cfg(feature = "paged-safe-glyph-slices")]
            slice_identity: Arc::new(()),
            resources: self.resources.clone(),
            authority: self.authority.clone(),
            source_range: self.source_range.clone(),
            count: self.count,
            generation: self.generation,
            direction: self.direction,
            points: self.points,
            status: MarkdownShapedSpoolStatus::Ready,
        })
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownShapedSpoolResourceObserver {
        self.resources.clone()
    }
}

/// Own a unique File and immutable Source/Font edges, without Rc, raw pointers, or an unsafe Send implementation.
pub struct MarkdownShapedSpool {
    #[cfg(feature = "paged-safe-glyph-slices")]
    slice_identity: Arc<()>,
    payload: Option<Payload>,
    resources: MarkdownShapedSpoolResourceObserver,
    authority: MarkdownShapedSpoolAuthority,
    source_range: Range<usize>,
    count: usize,
    generation: u64,
    direction: MarkdownShapingDirection,
    points: f32,
    status: MarkdownShapedSpoolStatus,
}
impl MarkdownShapedSpool {
    #[must_use]
    pub fn authority(&self) -> MarkdownShapedSpoolAuthority {
        self.authority.clone()
    }
    #[must_use]
    pub fn glyph_count(&self) -> usize {
        self.count
    }
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.source_range.clone()
    }
    #[must_use]
    pub fn source_generation(&self) -> u64 {
        self.generation
    }
    #[must_use]
    pub fn direction(&self) -> MarkdownShapingDirection {
        self.direction
    }
    #[must_use]
    pub fn points(&self) -> f32 {
        self.points
    }
    #[must_use]
    pub fn status(&self) -> MarkdownShapedSpoolStatus {
        self.status
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownShapedSpoolResourceObserver {
        self.resources.clone()
    }

    pub fn glyph(
        &mut self,
        authority: &MarkdownShapedSpoolAuthority,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        index: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        if index >= self.count {
            self.read_batch(authority, source, font, self.count..self.count, &mut [])?;
            return Ok(None); // Actual sealed EOF checks all original tickets too; a Building prefix cannot masquerade as EOF.
        }
        let mut output = [empty_glyph(); 1];
        self.read_batch(authority, source, font, index..index + 1, &mut output)?;
        Ok(Some(output[0]))
    }

    /// Read at most 256 glyphs per actual pread batch, without a resident page cache or full-run Vec.
    ///
    /// # Errors
    /// Failure retires the entire reader; `output` is caller-owned candidate storage and must be discarded in full on Err.
    /// Even if candidate values were written before a later I/O/authority failure, no partial values become Ready.
    pub fn read_batch(
        &mut self,
        authority: &MarkdownShapedSpoolAuthority,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
        output: &mut [MarkdownShapedGlyph],
    ) -> Result<usize> {
        let result = (|| {
            self.resources.failure()?;
            let value = self.payload.as_ref().ok_or_else(|| {
                if self.status == MarkdownShapedSpoolStatus::Cancelled {
                    MarkdownShapedSpoolError::Cancelled
                } else {
                    MarkdownShapedSpoolError::Poisoned
                }
            })?;
            value.origin.permits(authority, source, font)?;
            if range.start > range.end
                || range.end > self.count
                || range.len() != output.len()
                || output.len() > MARKDOWN_SHAPED_SPOOL_STEP
            {
                return Err(MarkdownShapedSpoolError::InvalidRequest); // Reject oversized or mismatched batch ranges before reading records.
            }
            let mut bytes = [0u8; RECORD * MARKDOWN_SHAPED_SPOOL_STEP];
            let used = range.len() * RECORD;
            if used != 0 {
                let offset = range
                    .start
                    .checked_mul(RECORD)
                    .ok_or(MarkdownShapedSpoolError::InvalidRequest)?;
                value.file.read_into(offset, &mut bytes[..used])?;
            } // An actual empty batch creates neither glyphs nor I/O, but the final original-authority check still applies at EOF.
            for record in bytes[..used].as_chunks::<RECORD>().0 {
                let glyph = decode(*record)?;
                if !self.source_range.contains(&glyph.source_utf16_cluster) {
                    return Err(MarkdownShapedSpoolError::InvalidRequest);
                }
            } // Validate the entire batch before touching output, so a corrupt final record cannot publish a known prefix.
            self.resources.failure()?;
            value.origin.permits(authority, source, font)?;
            for (target, record) in output.iter_mut().zip(bytes[..used].as_chunks::<RECORD>().0) {
                *target = decode(*record)?;
            }
            value.origin.permits(authority, source, font)?; // Recheck original tickets before return; callers discard the whole candidate batch on Err.
            Ok(output.len())
        })();
        match result {
            Ok(count) => Ok(count),
            Err(error) => {
                self.status = if matches!(error, MarkdownShapedSpoolError::Cancelled) {
                    MarkdownShapedSpoolStatus::Cancelled
                } else {
                    MarkdownShapedSpoolStatus::Poisoned
                };
                drop(self.payload.take());
                if let Err(cause) = self.resources.failure() {
                    self.status = MarkdownShapedSpoolStatus::Poisoned;
                    return Err(cause);
                }
                Err(error)
            }
        }
    }

    /// Return this owner's resources without implying operation success; independently observe underlying shared Source/Font mappings.
    pub fn close(mut self) -> Result<()> {
        drop(self.payload.take());
        self.resources.failure() // Check original failure after Drop; observers distinguish actual close success from an earlier operation error.
    }
}

fn empty_glyph() -> MarkdownShapedGlyph {
    MarkdownShapedGlyph {
        glyph_id: 0,
        source_utf16_cluster: 0,
        flags: 0,
        x_advance: 0,
        y_advance: 0,
        x_offset: 0,
        y_offset: 0,
    }
}

fn encode(glyph: MarkdownShapedGlyph) -> Result<[u8; RECORD]> {
    let mut bytes = [0; RECORD];
    bytes[..4].copy_from_slice(&glyph.glyph_id.to_le_bytes());
    bytes[4..8].copy_from_slice(&glyph.flags.to_le_bytes());
    bytes[8..16].copy_from_slice(
        &u64::try_from(glyph.source_utf16_cluster)
            .map_err(|_| MarkdownShapedSpoolError::InvalidRequest)?
            .to_le_bytes(),
    );
    for (chunk, value) in bytes[16..].as_chunks_mut::<4>().0.iter_mut().zip([
        glyph.x_advance,
        glyph.y_advance,
        glyph.x_offset,
        glyph.y_offset,
    ]) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}
fn decode(bytes: [u8; RECORD]) -> Result<MarkdownShapedGlyph> {
    let u32_at = |range: Range<usize>| {
        u32::from_le_bytes(bytes[range].try_into().expect("固定4B record字段"))
    };
    let i32_at = |range: Range<usize>| {
        i32::from_le_bytes(bytes[range].try_into().expect("固定4B record字段"))
    };
    Ok(MarkdownShapedGlyph {
        glyph_id: u32_at(0..4),
        flags: u32_at(4..8),
        source_utf16_cluster: usize::try_from(u64::from_le_bytes(
            bytes[8..16].try_into().expect("固定8B cluster"),
        ))
        .map_err(|_| MarkdownShapedSpoolError::InvalidRequest)?,
        x_advance: i32_at(16..20),
        y_advance: i32_at(20..24),
        x_offset: i32_at(24..28),
        y_offset: i32_at(28..32),
    })
}

/// Weak identity for one concrete sealed file; multiple spools from the same original Job cannot substitute for this ticket.
#[cfg(feature = "paged-safe-glyph-slices")]
#[derive(Clone)]
pub(crate) struct MarkdownShapedSpoolInstance(Weak<()>);
#[cfg(feature = "paged-safe-glyph-slices")]
impl MarkdownShapedSpool {
    pub(crate) fn instance_identity(&self) -> MarkdownShapedSpoolInstance {
        MarkdownShapedSpoolInstance(Arc::downgrade(&self.slice_identity))
    }
    pub(crate) fn require_instance(
        &mut self,
        instance: &MarkdownShapedSpoolInstance,
    ) -> Result<()> {
        self.resources.failure()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        payload.origin.source.health()?;
        if !Weak::ptr_eq(&instance.0, &Arc::downgrade(&self.slice_identity)) {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        let source = payload.origin.source_authority.clone();
        let font = payload.origin.font_authority.clone();
        let authority = self.authority.clone();
        self.read_batch(&authority, &source, &font, self.count..self.count, &mut [])?;
        Ok(()) // Weak identity alone is not Ready; actual use rechecks original Source/cancellation/FD failure and reader state.
    }
}

/// Historical-item receipts retain weak authority and fixed facts; another output of the same Job cannot replace this concrete spool instance.
#[cfg(feature = "paged-safe-glyph-slices")]
#[derive(Clone)]
pub(crate) struct MarkdownSpoolSliceOrigin {
    pub instance: MarkdownShapedSpoolInstance,
    pub authority: MarkdownShapedSpoolAuthority,
    pub source: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub range: Range<usize>,
    pub count: usize,
    pub generation: u64,
    pub direction: MarkdownShapingDirection,
    pub points: u32,
}
#[cfg(feature = "paged-safe-glyph-slices")]
impl MarkdownShapedSpool {
    pub(crate) fn safe_slice_origin(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
    ) -> Result<MarkdownSpoolSliceOrigin> {
        self.resources.failure()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        let font = payload.origin.font_authority.clone();
        let authority = self.authority.clone();
        self.read_batch(&authority, source, &font, self.count..self.count, &mut [])?;
        Ok(MarkdownSpoolSliceOrigin {
            instance: self.instance_identity(),
            authority,
            source: source.clone(),
            font,
            range: self.source_range.clone(),
            count: self.count,
            generation: self.generation,
            direction: self.direction,
            points: self.points.to_bits(),
        }) // EOF still checks actual Source/Font/file failure first; old naked metadata cannot construct new authority.
    }
    pub(crate) fn require_safe_slice_origin(
        &mut self,
        origin: &MarkdownSpoolSliceOrigin,
    ) -> Result<()> {
        self.resources.failure()?;
        if let Some(payload) = &self.payload {
            payload.origin.source.health()?;
        }
        self.require_instance(&origin.instance)?;
        if origin.range != self.source_range
            || origin.count != self.count
            || origin.generation != self.generation
            || origin.direction != self.direction
            || origin.points != self.points.to_bits()
        {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        self.read_batch(
            &origin.authority,
            &origin.source,
            &origin.font,
            self.count..self.count,
            &mut [],
        )?;
        Ok(()) // Recheck all original authorities; same-name Font, another same-job spool, or equal-content Source cannot substitute.
    }
    pub(crate) fn require_safe_slice_job(
        &mut self,
        origin: &MarkdownSpoolSliceOrigin,
        job: &mut MarkdownShapingJob,
    ) -> Result<()> {
        self.require_safe_slice_origin(origin)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        if !Weak::ptr_eq(
            &Arc::downgrade(&payload.origin.nonce),
            &job.index_identity(),
        ) || origin.range != job.source_range()
        {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        job.glyph(&origin.source, &origin.font, origin.count)?;
        if job.profile()?.glyph_count != origin.count {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        Ok(()) // Record while the old Job lives at actual NeedNextItem, not later by reauthorizing history from equal Source alone.
    }
}
