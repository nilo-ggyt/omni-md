//! Certified finite prefixes of one original plan. Unread item tails never acquire glyph or
//! soft-line authority from a completed prefix.
//!
//! Used by the explicit coordinator only. Local ASCII support and actual single-scalar graphemes
//! require separate confined certificates. Prefix Ready is not LineReady; multi-scalar graphemes and
//! uncertified lookups retain their unresolved result rather than an invented completed line.

mod advance;
pub(super) mod local;
mod positioned;
mod unicode;
mod word;
pub(super) use word::WordPrefixProof;

use std::{
    ops::Range,
    sync::{Arc, Weak},
};

use thiserror::Error;

use super::{options::ValidatedOptions, *};
use crate::{
    MarkdownGraphemeAnswer, MarkdownGraphemeQuery, MarkdownLayoutBreakError,
    MarkdownLayoutBreakJob, markdown_layout_font::frontier::FontFrontierPlan,
};

/// Certify at most 256 scalars per center, accounting actual support input separately; a byte limit
/// never creates a new paragraph boundary.
pub(crate) const MARKDOWN_SHAPING_FRONTIER_UNITS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkdownShapingFrontierReason {
    Plan(harfrust::AsciiFrontierRejection),
    NonAscii,
    WordBeyondBudget,
    NotGraphemeBoundary,
    UncertifiedWord,
    NegativeAdvance,
    NonSimpleGrapheme,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkdownShapingFrontierAnswer {
    Pending,
    PendingConsumer,
    PartReady,
    Complete,
    NeedsFrontier(MarkdownShapingFrontierReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MarkdownShapingFrontierStep {
    pub answer: MarkdownShapingFrontierAnswer,
    pub source_bytes: usize,
    pub input_bytes: usize,
    pub scalars: usize,
    pub context_bytes: usize,
    pub shapes: usize,
    pub metadata_glyphs: usize,
    pub plan_records: usize,
    pub boundary_records: usize,
}

#[derive(Debug, Error)]
pub(crate) enum MarkdownShapingFrontierError {
    #[error("前沿原字形阶段失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("前沿原字素权限失败")]
    Breaks(#[from] MarkdownLayoutBreakError),
    #[error("前沿续行票不属于原任务或阶段")]
    ContinuationMismatch,
    #[error("前沿任务已经永久撤权")]
    Poisoned,
    #[error("原字体非负闭包阶段失败")]
    Advance(#[from] advance::AdvanceScanError),
}
type FrontierResult<T> = std::result::Result<T, MarkdownShapingFrontierError>;

struct FrontierPayload {
    source: MarkdownLayoutTextSource,
    font: MarkdownLayoutFont,
    options: ValidatedOptions,
    plan: Option<FontFrontierPlan>,
}

/// The unique owner retains the unresolved cursor; no public Range-to-partial-item certification
/// constructor or reset entry exists.
pub(crate) struct MarkdownShapingFrontierJob {
    identity: Arc<()>,
    payload: Option<FrontierPayload>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    cancellation: MarkdownShapingCancellation,
    original: Range<usize>,
    original_bytes: Range<usize>,
    next: usize,
    next_byte: usize,
    serial: u64,
    scan: [u8; MARKDOWN_SHAPING_FRONTIER_UNITS + 1],
    scanned: usize,
    candidate: Option<usize>,
    child: Option<MarkdownShapingJob>,
    outstanding: Option<Weak<()>>,
    answer: MarkdownShapingFrontierAnswer,
    word_candidate: bool,
    word_source_checked: bool,
    word_closure_checked: bool,
    word_closure: Option<harfrust::AsciiGlyphClosureJob>,
    advance_scan: Option<advance::DefaultAdvanceScan>,
    nonnegative: Option<advance::NonnegativeDefaultAdvanceProof>,
    word_end_cursor: Option<crate::MarkdownLayoutBreakCursor>,
    local_source_checked: bool,
    local_source_cursor: Option<crate::markdown_document_buffer::FrozenPrintableAsciiSuffix>,
    local_source_bounds: Option<(u8, u8)>,
    local_scan: Option<harfrust::AsciiLocalFrontierJob>,
    local_capability: Option<harfrust::AsciiLocalFrontierCapability>,
    local_certificate: Option<harfrust::AsciiLocalFrontierCertificate>,
    positioned_scan: Option<harfrust::AsciiPositionedAdvanceJob>,
    positioned_proof: Option<positioned::PositionedNonnegativeProof>,
    local_verified: usize,
    resources: Option<crate::markdown_line_composer::worker_resources::ComposerResourceScope>,
    unicode: Option<Box<unicode::UnicodeState>>,
    // Open SCX grants independent grapheme centers only, retaining the final actual GCB as support.
    open_scope: bool,
    // Original UBA item facts bound shaping input/context, never global editing grapheme boundaries.
    shaping_seams: [Option<crate::markdown_line_composer::ResolvedBidiShapingSeam>; 2],
    partial_grapheme: [bool; 2],
    context_bounds: Option<Range<usize>>,
}

/// One actual Ready child with its unique return ticket; copied public ranges cannot construct it.
pub(crate) struct MarkdownShapingFrontierPart {
    job: MarkdownShapingJob,
    continuation: MarkdownShapingFrontierContinuation,
    original: Range<usize>,
    scope_complete: bool,
    shaping_seams: [Option<crate::markdown_line_composer::ResolvedBidiShapingSeam>; 2],
}

/// Weak identities and scalars retain no Source/font/child task. The ticket cannot be cloned or
/// transferred to a different continuation owner.
pub(crate) struct MarkdownShapingFrontierContinuation {
    owner: Weak<()>,
    serial: u64,
    next: usize,
    next_byte: usize,
    child: Weak<()>,
    font_splice: bool,
}

/// Issued only when the original Unicode cursor encounters a complete blocking GCB; it authorizes
/// neither arbitrary short ranges nor a new SCX boundary.
pub(crate) struct MarkdownShapingTerminalGrapheme {
    owner: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    utf16: Range<usize>,
    bytes: Range<usize>,
    item_end: usize,
    cancellation: MarkdownShapingCancellation,
}

impl MarkdownShapingTerminalGrapheme {
    pub(crate) fn source_range(&self) -> Range<usize> {
        self.utf16.clone()
    }

    pub(crate) fn owner_scope(&self) -> Weak<()> {
        self.owner.clone()
    }

    pub(crate) fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }

    pub(crate) fn original_font(&self) -> &MarkdownLayoutFontAuthority {
        &self.font
    }

    pub(crate) fn byte_range(&self) -> Range<usize> {
        self.bytes.clone()
    }

    pub(crate) fn item_end(&self) -> usize {
        self.item_end
    }

    pub(crate) fn cancellation(&self) -> MarkdownShapingCancellation {
        self.cancellation.clone()
    }
}

impl MarkdownShapingFrontierPart {
    pub(crate) fn shaping_seams(
        &self,
    ) -> &[Option<crate::markdown_line_composer::ResolvedBidiShapingSeam>; 2] {
        &self.shaping_seams
    }
    /// Borrow this actual part's original Source identity; its public range cannot authorize an
    /// audit against another source.
    pub(crate) fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.job.source_authority
    }
    /// Audit/reshaping retain the same original cancellation, without a caller-created lifecycle
    /// detached from this task.
    pub(crate) fn cancellation(&self) -> MarkdownShapingCancellation {
        self.job.cancellation.clone()
    }
    /// The original job's actual finite support, including context outside the center; paragraph
    /// assumptions or budgets do not determine these bounds.
    pub(crate) fn shaping_range(&self) -> Range<usize> {
        self.job.source_utf16.clone()
    }
    pub(crate) fn certified_range(&self) -> Range<usize> {
        self.job.source_range()
    }
    pub(crate) fn original_range(&self) -> Range<usize> {
        self.original.clone()
    }
    pub(crate) fn is_item_complete(&self) -> bool {
        self.job.source_range().end == self.original.end
    }
    pub(crate) fn is_scope_complete(&self) -> bool {
        self.scope_complete
    }
    pub(crate) fn into_parts(self) -> (MarkdownShapingJob, MarkdownShapingFrontierContinuation) {
        (self.job, self.continuation) // Transfer the actual child, not metadata presented as completed consumption.
    }
}

/// Open scope carries the original SCX borrow and two certified shaping endpoint tickets; copied
/// coordinates cannot construct that authority.
pub(crate) enum MarkdownShapingFrontierScope<'a, 'job> {
    Closed(Range<usize>),
    /// Certify original style coordinates only, without granting open Script, independent glyph,
    /// or actual paragraph-end authority.
    CertifiedCoordinates {
        original: Range<usize>,
        endpoints: [&'a crate::markdown_line_composer::CertifiedShapingEndpoint; 2],
    },
    Open {
        original: Range<usize>,
        endpoints: [&'a crate::markdown_line_composer::CertifiedShapingEndpoint; 2],
        script: &'a crate::markdown_scripts::MarkdownStableScriptPrefix<'job>,
    },
}

impl MarkdownShapingFrontierJob {
    /// Plan/scalar rejection does not establish a missing glyph. Return only an already-read complete
    /// GCB for an actual font trial.
    pub(crate) fn terminal_grapheme(
        &self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> FrontierResult<Option<MarkdownShapingTerminalGrapheme>> {
        self.guard(source, font)?;
        if self.open_scope
            || self.answer
                != MarkdownShapingFrontierAnswer::NeedsFrontier(
                    MarkdownShapingFrontierReason::Plan(
                        harfrust::AsciiFrontierRejection::LookupBehavior,
                    ),
                )
        {
            return Ok(None); // Other plan rejections and open SCX do not qualify for this natural-tail handoff.
        }
        Ok(self.unicode.as_ref().and_then(|state| {
            state
                .terminal_grapheme(self.original.end)
                .filter(|(utf16, _)| {
                    !(utf16.start == self.original.start && self.partial_grapheme[0])
                        && !(utf16.end == self.original.end && self.partial_grapheme[1])
                }) // Local shaping GB1/GB2 is not a complete outer GCB for this terminal handoff.
                .map(|(utf16, bytes)| MarkdownShapingTerminalGrapheme {
                    owner: Arc::downgrade(&self.identity),
                    source: self.source.clone(),
                    font: self.font.clone(),
                    utf16,
                    bytes,
                    item_end: self.original.end,
                    cancellation: self.cancellation.clone(),
                })
        }))
    }

    /// The same font may resume only a natural final grapheme; a different font closes only the
    /// already certified remainder of the original font.
    pub(crate) fn resume_terminal_grapheme(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        terminal: &MarkdownShapingTerminalGrapheme,
        same_font: bool,
    ) -> FrontierResult<()> {
        self.guard(source, font)?;
        if (same_font && terminal.utf16.end != self.original.end)
            || !Weak::ptr_eq(&terminal.owner, &Arc::downgrade(&self.identity))
            || terminal.source != self.source
            || !terminal.font.same_payload(&self.font)
            || self.terminal_grapheme(source, font)?.is_none_or(|actual| {
                actual.utf16 != terminal.utf16 || actual.bytes != terminal.bytes
            })
        {
            return Err(MarkdownShapingFrontierError::ContinuationMismatch);
        } // Require the original owner and actual blocked position; equal ranges in another request cannot resume this cursor.
        self.candidate = Some(
            self.unicode
                .as_mut()
                .ok_or(MarkdownShapingError::Pending)?
                .resume_terminal_grapheme(same_font),
        );
        self.answer = MarkdownShapingFrontierAnswer::Pending;
        Ok(())
    }

    /// After the prior actual font run returns, hand off the next choice certified by original GCB
    /// queries. Preserve owner/SCX/cancellation; the caller supplies actual adjacent endpoint
    /// tickets and completed font trials.
    pub(crate) fn following_font_grapheme(
        &self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        previous: &MarkdownShapingTerminalGrapheme,
        endpoints: [&crate::markdown_layout_breaks::MarkdownCertifiedGraphemeEndpoint; 2],
    ) -> FrontierResult<MarkdownShapingTerminalGrapheme> {
        self.guard(source, font)?;
        let start = endpoints[0].boundary(source)?;
        let end = endpoints[1].boundary(source)?;
        if !Weak::ptr_eq(&previous.owner, &Arc::downgrade(&self.identity))
            || previous.source != self.source
            || !previous.font.same_payload(&self.font)
            || self.outstanding.is_some()
            || self.next != start.utf16
            || self.next_byte != start.byte
            || start.utf16 < previous.utf16.end
            || end.utf16 <= start.utf16
            || end.utf16 > self.original.end
            || !endpoints[0].same_scope(endpoints[1])
        {
            return Err(MarkdownShapingFrontierError::ContinuationMismatch);
        } // Do not skip undelivered original-font text or borrow endpoints from another index.
        Ok(MarkdownShapingTerminalGrapheme {
            owner: previous.owner.clone(),
            source: self.source.clone(),
            font: self.font.clone(),
            utf16: start.utf16..end.utf16,
            bytes: start.byte..end.byte,
            item_end: self.original.end,
            cancellation: self.cancellation.clone(),
        })
    }

    /// Register the actual child and unique return ticket before a fallback font run enters the
    /// original Store. Perform no shaping/Source reconstruction here; only its actual Ready job can
    /// advance the original frontier.
    pub(crate) fn lend_font_run(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        proof: &MarkdownShapingTerminalGrapheme,
        job: &mut MarkdownShapingJob,
    ) -> FrontierResult<MarkdownShapingFrontierContinuation> {
        self.guard(source, font)?;
        let selected_font = job.font_authority.clone();
        job.guard(source, &selected_font)?;
        if !Weak::ptr_eq(&proof.owner, &Arc::downgrade(&self.identity))
            || proof.source != self.source
            || !proof.font.same_payload(&self.font)
            || self.outstanding.is_some()
            || self.next != proof.utf16.start
            || job.source_utf16.start != self.next
            || job.source_utf16.end < proof.utf16.end
            || job.source_utf16.end > self.original.end
            || job.font_authority.same_payload(&self.font)
            || job.status() != MarkdownShapingStatus::Ready
        {
            return Err(MarkdownShapingFrontierError::ContinuationMismatch);
        } // Only actual new-font output qualifies; the same font cannot bypass the original plan's rejection.
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or(MarkdownShapingError::CoordinateOverflow)?;
        let child = job.index_identity();
        self.outstanding = Some(child.clone());
        Ok(MarkdownShapingFrontierContinuation {
            owner: Arc::downgrade(&self.identity),
            serial: self.serial,
            next: job.source_utf16.end,
            next_byte: job.source_bytes.end,
            child,
            font_splice: true,
        })
    }

    /// Called only by the coordinator after actual item/style validation, without exposing a public
    /// short-range authority upgrade.
    pub(crate) fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        original: Range<usize>,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> FrontierResult<Self> {
        Self::initialize(
            source,
            authority,
            font,
            font_authority,
            MarkdownShapingFrontierScope::Closed(original),
            options,
            cancellation,
        )
    }

    /// Certified scopes consume original endpoint coordinates. Closed scope without index tickets
    /// retains the existing synchronous lookup contract.
    pub(crate) fn new_with_scope(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        scope: MarkdownShapingFrontierScope<'_, '_>,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> FrontierResult<Self> {
        match scope {
            MarkdownShapingFrontierScope::Closed(original) => Self::new(
                source,
                authority,
                font,
                font_authority,
                original,
                options,
                cancellation,
            ),
            certified => Self::initialize(
                source,
                authority,
                font,
                font_authority,
                certified,
                options,
                cancellation,
            ),
        } // Keep original synchronous lookup explicit; the ticketed route does not reread text merely for code reuse.
    }

    /// Share original identity/options/plan assembly while retaining the two actual coordinate origins.
    fn initialize(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        scope: MarkdownShapingFrontierScope<'_, '_>,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> FrontierResult<Self> {
        source.health().map_err(MarkdownShapingError::from)?; // Original errno precedes cancellation or mismatched authority.
        if !source.permits(authority) {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        if !font.permits(font_authority) {
            return Err(MarkdownShapingError::FontMismatch.into());
        }
        let original = match &scope {
            MarkdownShapingFrontierScope::Closed(original)
            | MarkdownShapingFrontierScope::CertifiedCoordinates { original, .. }
            | MarkdownShapingFrontierScope::Open { original, .. } => original.clone(),
        };
        if original.start > original.end || original.end > source.utf16_len() {
            return Err(MarkdownShapingError::InvalidRange.into());
        }
        u32::try_from(original.end).map_err(|_| MarkdownShapingError::CoordinateOverflow)?;
        let original_bytes = match &scope {
            MarkdownShapingFrontierScope::Closed(_) => {
                source
                    .byte_offset_for_utf16(original.start)
                    .map_err(MarkdownShapingError::from)?
                    ..source
                        .byte_offset_for_utf16(original.end)
                        .map_err(MarkdownShapingError::from)?
            }
            MarkdownShapingFrontierScope::Open { endpoints, .. }
            | MarkdownShapingFrontierScope::CertifiedCoordinates { endpoints, .. } => {
                let start = endpoints[0].boundary(authority)?;
                let end = endpoints[1].boundary(authority)?;
                endpoints[0].require_pair(endpoints[1], authority)?;
                if start.utf16 != original.start
                    || end.utf16 != original.end
                    || start.byte >= end.byte
                    || end.byte > source.byte_len()
                {
                    return Err(MarkdownShapingError::InvalidRange.into());
                }
                start.byte..end.byte
            }
        };
        let (shaping_seams, partial_grapheme) = match &scope {
            MarkdownShapingFrontierScope::Closed(_) => ([None, None], [false; 2]),
            MarkdownShapingFrontierScope::Open { endpoints, .. }
            | MarkdownShapingFrontierScope::CertifiedCoordinates { endpoints, .. } => (
                std::array::from_fn(|index| endpoints[index].seam().cloned()),
                std::array::from_fn(|index| !endpoints[index].is_grapheme()),
            ),
        };
        let context_bounds = shaping_seams.iter().any(Option::is_some).then(|| {
            (if shaping_seams[0].is_some() {
                original_bytes.start
            } else {
                0
            })..(if shaping_seams[1].is_some() {
                original_bytes.end
            } else {
                source.byte_len()
            })
        }); // Retain actual seams after the Style request ends; finite byte windows do not imply paragraph boundaries.
        let options = ValidatedOptions::new(options, &font)?;
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        let (plan, answer) = match FontFrontierPlan::new(
            &font,
            font_authority,
            options.direction,
            options.script,
            options.language.as_ref(),
            &options.features,
        ) {
            Ok(plan) => (Some(plan), MarkdownShapingFrontierAnswer::Pending),
            Err(reason) => (
                None,
                MarkdownShapingFrontierAnswer::NeedsFrontier(MarkdownShapingFrontierReason::Plan(
                    reason,
                )),
            ),
        }; // Keep typed plan rejection; changing features or eagerly shaping the item would not establish this lazy proof.
        source.health().map_err(MarkdownShapingError::from)?;
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        let unicode = (original_bytes.len() != original.len()).then(|| {
            Box::new(unicode::UnicodeState::new(
                original_bytes.start,
                original.start,
            ))
        });
        let mut result = Self {
            identity: Arc::new(()),
            payload: Some(FrontierPayload {
                source,
                font,
                options,
                plan,
            }),
            source: authority.clone(),
            font: font_authority.clone(),
            cancellation,
            next: original.start,
            next_byte: original_bytes.start,
            original,
            original_bytes,
            serial: 0,
            scan: [0; MARKDOWN_SHAPING_FRONTIER_UNITS + 1],
            scanned: 0,
            candidate: None,
            child: None,
            outstanding: None,
            answer,
            word_candidate: false,
            word_source_checked: false,
            word_closure_checked: false,
            word_closure: None,
            advance_scan: None,
            nonnegative: None,
            word_end_cursor: None,
            local_source_checked: false,
            local_source_cursor: None,
            local_source_bounds: None,
            local_scan: None,
            local_capability: None,
            local_certificate: None,
            positioned_scan: None,
            positioned_proof: None,
            local_verified: 0,
            resources: None,
            unicode,
            open_scope: false,
            shaping_seams,
            partial_grapheme,
            context_bounds,
        };
        if let MarkdownShapingFrontierScope::Open { script, .. } = scope {
            result.bind_open_script(script)?;
        }
        Ok(result)
    }

    /// Only the original Script borrow can bind this owner to an open support domain; a public
    /// short range cannot upgrade it. The font still needs per-record SimpleGcb certification;
    /// ASCII local/kern paths do not acquire this route.
    fn bind_open_script(
        &mut self,
        prefix: &crate::markdown_scripts::MarkdownStableScriptPrefix<'_>,
    ) -> FrontierResult<()> {
        self.guard(&self.source, &self.font)?;
        let script_error = |error| match error {
            crate::MarkdownScriptError::Source(error) => MarkdownShapingError::Source(error),
            crate::MarkdownScriptError::Cancelled => MarkdownShapingError::Cancelled,
            _ => MarkdownShapingError::SourceMismatch,
        };
        let facts = prefix.facts().map_err(script_error)?;
        if prefix.source_authority().map_err(script_error)? != &self.source
            || prefix.kind().map_err(script_error)?
                != crate::markdown_scripts::MarkdownScriptPrefixKind::StableOpenPrefix
            || self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingError::Poisoned)?
                .options
                .script
                .tag()
                .to_be_bytes()
                != facts.script.iso15924_tag()
            || self.original.start < facts.start_utf16
            || self.original.end > facts.end_utf16
            || self.serial != 0
            || self.child.is_some()
            || self.open_scope
        {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        let mut unicode =
            unicode::UnicodeState::new(self.original_bytes.start, self.original.start);
        unicode.require_independent_route();
        self.unicode = Some(Box::new(unicode));
        self.open_scope = true;
        self.guard(&self.source, &self.font)
    }

    pub(crate) fn attach_composer_resources(
        &mut self,
        registration: crate::markdown_line_composer::worker_resources::FrontierResourceRegistration,
    ) -> FrontierResult<()> {
        self.guard(&self.source, &self.font)?;
        if self.resources.is_some()
            || self.next != self.original.start
            || self.serial != 0
            || self.child.is_some()
            || self.outstanding.is_some()
        {
            return Err(MarkdownShapingError::InvalidOptions.into());
        } // Install original parent scope before any child; a later scope cannot retroactively bless continuation work.
        self.resources = Some(
            registration
                .install()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?,
        );
        Ok(())
    }

    fn guard(
        &self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> FrontierResult<()> {
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        payload
            .source
            .health()
            .map_err(MarkdownShapingError::from)?;
        if source != &self.source || !payload.source.permits(source) {
            return Err(MarkdownShapingError::SourceMismatch.into());
        }
        if !payload.font.permits(font) || !payload.font.permits(&self.font) {
            return Err(MarkdownShapingError::FontMismatch.into());
        }
        if payload
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.permits_font(&payload.font))
        {
            return Err(MarkdownShapingError::FontMismatch.into());
        } // Recheck actual font-plan weak identity; an equivalent options key cannot replace its owner.
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        Ok(())
    }

    fn fail<T>(&mut self, error: MarkdownShapingFrontierError) -> FrontierResult<T> {
        let backing = self
            .payload
            .as_ref()
            .and_then(|payload| payload.source.original_backing_observer());
        drop(self.child.take());
        drop(self.advance_scan.take());
        self.word_closure = None; // Return a fixed bitset cursor with no file/mapping/strong Source claim.
        self.local_scan = None; // Return fixed font-ID state without rewriting actual glyph/Source receipts.
        self.local_source_cursor = None; // Return an unfinished original tree path on cancellation/Source failure too.
        self.local_source_bounds = None;
        self.positioned_scan = None;
        self.positioned_proof = None;
        self.local_certificate = None;
        drop(self.nonnegative.take());
        drop(self.payload.take()); // Return actual resources first; ended cursors/tickets cannot recapture current Source/style.
        if let Some(errno) = backing.and_then(|observer| observer.first_errno()) {
            return Err(MarkdownShapingError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )
            .into());
        } // Failure observed on final Source return also precedes cancellation, unsupported results, or old metadata errors.
        Err(error)
    }

    pub(crate) fn step(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownShapingBudget,
        metadata_budget: usize,
    ) -> FrontierResult<MarkdownShapingFrontierStep> {
        let result = self.advance(source, font, breaks, budget, metadata_budget);
        let result = match self.guard(source, font) {
            Err(MarkdownShapingFrontierError::Shaping(MarkdownShapingError::Source(error))) => {
                Err(MarkdownShapingError::Source(error).into())
            }
            guarded => result.and_then(|step| {
                guarded?;
                Ok(step)
            }),
        }; // Even zero-work NeedsFrontier/Pending exits must not conceal actual Source failure as successful progress.
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail(error),
        }
    }

    fn advance(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownShapingBudget,
        metadata_budget: usize,
    ) -> FrontierResult<MarkdownShapingFrontierStep> {
        self.guard(source, font)?;
        if budget.bytes == 0 || budget.scalars == 0 || metadata_budget == 0 {
            return Err(MarkdownShapingError::InvalidOptions.into());
        }
        let mut step = MarkdownShapingFrontierStep {
            answer: self.answer,
            source_bytes: 0,
            input_bytes: 0,
            scalars: 0,
            context_bytes: 0,
            shapes: 0,
            metadata_glyphs: 0,
            plan_records: 0,
            boundary_records: 0,
        };
        if self.outstanding.is_some() {
            step.answer = MarkdownShapingFrontierAnswer::PendingConsumer;
            return Ok(step); // Even a dropped child needs its original return ticket; external ranges cannot skip FIFO ownership.
        }
        if matches!(
            self.answer,
            MarkdownShapingFrontierAnswer::NeedsFrontier(_)
                | MarkdownShapingFrontierAnswer::Complete
                | MarkdownShapingFrontierAnswer::PartReady
        ) {
            return Ok(step);
        }
        if self.next == self.original.end {
            self.answer = MarkdownShapingFrontierAnswer::Complete;
            step.answer = self.answer;
            return Ok(step); // Only the actual item endpoint is Complete; a full finite buffer is not EOF.
        }
        if self.unicode.is_some() {
            return self.advance_unicode(source, font, breaks, budget, metadata_budget, step);
        }
        let defer_local = self
            .payload
            .as_ref()
            .and_then(|payload| payload.plan.as_ref().map(|plan| (plan, &payload.font)))
            .is_some_and(|(plan, font)| plan.simple_gcb_job(font).is_ok());
        if !defer_local
            && self.local_plan()
            && !self.prepare_local(source, font, budget, metadata_budget, &mut step)?
        {
            step.answer = self.answer;
            return Ok(step);
        } // A plan without independent-GCB capability keeps its prior rejection, without expanded scan authority.
        if self.candidate.is_none() {
            let needed = (self.original_bytes.end - self.next_byte).min(self.scan.len());
            let count = (needed - self.scanned)
                .min(budget.bytes)
                .min(budget.scalars);
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let begin = self.next_byte + self.scanned;
            let mut copied = 0;
            payload
                .source
                .visit_byte_range(begin..begin + count, |bytes| {
                    self.scan[self.scanned + copied..self.scanned + copied + bytes.len()]
                        .copy_from_slice(bytes);
                    copied += bytes.len();
                    Ok(())
                })
                .map_err(MarkdownShapingError::from)?;
            self.guard(source, font)?;
            self.scanned += copied; // Publish read facts after successful actual lease return, never an error recast as early success.
            step.source_bytes = copied;
            if self.scanned != needed {
                return Ok(step);
            }
            let ascii = self.scan[..needed]
                .iter()
                .position(|byte| !(0x20..=0x7e).contains(byte))
                .unwrap_or(needed);
            let eof = self.next_byte + needed == self.original_bytes.end;
            let length = if eof && ascii == needed && needed <= MARKDOWN_SHAPING_FRONTIER_UNITS {
                needed // A final short part ends at the actual item endpoint, without inventing a natural space break.
            } else {
                // End at a real nondecimal space seam with an already-read first-unconsumed ASCII
                // scalar. A later fraction slash can affect its decimal run, not cross this space
                // into the certified prefix.
                (1..ascii.min(needed))
                    .rev()
                    .find(|&end| self.scan[end - 1] == b' ')
                    .unwrap_or(0)
            };
            if length == 0 {
                if ascii < needed
                    && let Some(job) = payload
                        .plan
                        .as_ref()
                        .and_then(|plan| plan.simple_gcb_job(&payload.font).ok())
                {
                    // Switch only after the charged <=257-byte probe really finds non-ASCII with
                    // no safe space seam, using the same font/options' independent-GCB capability.
                    // Scratch is neither a GCB nor encoding proof; reread actual boundaries in later
                    // quanta and account those reads instead of guessing from spelling/font names.
                    self.unicode = Some(Box::new(unicode::UnicodeState::from_ascii_scan(
                        self.next_byte,
                        self.next,
                        job,
                    )));
                    self.scanned = 0;
                    return Ok(step);
                }
                if ascii == needed && needed > MARKDOWN_SHAPING_FRONTIER_UNITS {
                    self.word_candidate = true;
                    self.candidate = Some(self.next + MARKDOWN_SHAPING_FRONTIER_UNITS);
                } else {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(if ascii < needed {
                        MarkdownShapingFrontierReason::NonAscii
                    } else {
                        MarkdownShapingFrontierReason::WordBeyondBudget
                    });
                    step.answer = self.answer;
                    return Ok(step); // No invented BOT/EOT or default plain behavior for an unsupported long word.
                }
            } else {
                self.candidate = Some(self.next + length);
                // Preserve the same certified word proof on its final short part, including safe
                // whole-line GCB candidates across prefixes. Reaching real EOF does not widen this
                // child's actual Ready range.
                self.word_candidate = self.word_source_checked;
            }
        }
        if self.local_plan()
            && !self.prepare_local(source, font, budget, metadata_budget, &mut step)?
        {
            step.answer = self.answer;
            return Ok(step);
        } // The selected ASCII candidate still needs full-source/local-plan certificates before word proof or child creation.
        let end = self
            .candidate
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        for at in [self.next, end] {
            match breaks.grapheme_query(source, at, MarkdownGraphemeQuery::IsBoundary)? {
                MarkdownGraphemeAnswer::Pending => return Ok(step),
                MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == at => {}
                MarkdownGraphemeAnswer::Known(None)
                    if (at == self.original.start && self.partial_grapheme[0])
                        || (at == self.original.end && self.partial_grapheme[1]) => {}
                MarkdownGraphemeAnswer::Known(_) => {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::NotGraphemeBoundary,
                    );
                    step.answer = self.answer;
                    return Ok(step);
                }
            } // ASCII bytes alone do not prove that the original item starts outside an external grapheme.
        }
        if self.word_candidate
            && !self.prepare_word(source, font, breaks, metadata_budget, &mut step)?
        {
            step.answer = self.answer;
            return Ok(step);
        }
        if self.child.is_none() {
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let plan = payload
                .plan
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let capability = plan.capability();
            if capability.is_some_and(|capability| capability.requires_closed_decimal_seam)
                && !self.word_candidate
                && end < self.original.end
                && self.scan[end - self.next - 1].is_ascii_digit()
            {
                return Err(MarkdownShapingError::InvalidRange.into()); // The whole-item ASCII proof separately excludes U+2044.
            }
            let support = self.support_range(end)?;
            let support_bytes = (self.original_bytes.start + support.start - self.original.start)
                ..(self.original_bytes.start + support.end - self.original.start);
            let mut options = payload.options.clone();
            options.beginning_of_text &= support.start == self.original.start;
            options.end_of_text &= support.end == self.original.end;
            let prepared = self
                .context_bounds
                .clone()
                .map(|bounds| {
                    input::prepare_context(&payload.source, support_bytes.clone(), Some(bounds))
                })
                .transpose()?;
            let mut child = MarkdownShapingJob::assemble_with_context(
                payload.source.clone(),
                source,
                payload.font.clone(),
                font,
                (support, support_bytes, prepared),
                options,
                self.cancellation.clone(),
            )?;
            if let Some(resources) = &self.resources {
                child.attach_composer_resources(resources)?;
            } // Attach the original claim before glyph storage, ingestion, or shaping can fail.
            // Report the constructor's fixed five-scalar adjacent context separately from support
            // ingestion. It is not free work within the input budget, and a 256-byte center does
            // not describe all actual Source access or main-thread elapsed time.
            step.context_bytes = child.profile()?.source_bytes_read;
            child
                .payload
                .as_mut()
                .ok_or(MarkdownShapingError::Poisoned)?
                .frontier_plan = Some(plan.plan());
            self.child = Some(child); // Acquire actual glyph storage only after Source/GCB/original-plan certification.
        }
        let child = self
            .child
            .as_mut()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        if child.status() == MarkdownShapingStatus::Ingesting {
            let bytes = budget.bytes - step.source_bytes;
            let scalars = budget.scalars - step.source_bytes;
            if bytes == 0 || scalars == 0 {
                return Ok(step);
            }
            let input = child.step(source, font, MarkdownShapingBudget { bytes, scalars })?;
            step.input_bytes = input.bytes;
            step.scalars = input.scalars;
        }
        if child.status() == MarkdownShapingStatus::AwaitingShape {
            child.shape(source, font)?;
            step.shapes = 1;
        }
        self.guard(source, font)?;
        if self
            .child
            .as_ref()
            .is_some_and(|child| child.status() == MarkdownShapingStatus::Ready)
            && self.seal_center(source, font, end, metadata_budget, &mut step)?
        {
            if self.word_candidate {
                let child = self
                    .child
                    .as_mut()
                    .ok_or(MarkdownShapingFrontierError::Poisoned)?;
                let proof = word::WordPrefixProof::new(
                    &self.identity,
                    child,
                    self.original.clone(),
                    &self.cancellation,
                )?;
                child
                    .payload
                    .as_mut()
                    .ok_or(MarkdownShapingError::Poisoned)?
                    .frontier_word = Some(proof);
            } // The actual center view must complete first; support cannot become a presigned public word-prefix range.
            self.answer = MarkdownShapingFrontierAnswer::PartReady;
        }
        step.answer = self.answer;
        Ok(step)
    }

    pub(crate) fn take_part(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> FrontierResult<MarkdownShapingFrontierPart> {
        let result = (|| {
            self.guard(source, font)?;
            if self.answer != MarkdownShapingFrontierAnswer::PartReady || self.outstanding.is_some()
            {
                return Err(MarkdownShapingError::Pending.into());
            }
            let job = self
                .child
                .take()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let range = job.source_range();
            let next_byte = self
                .unicode
                .as_ref()
                .map_or_else(|| self.next_byte + range.len(), |state| state.center_byte());
            self.serial = self
                .serial
                .checked_add(1)
                .ok_or(MarkdownShapingError::CoordinateOverflow)?;
            let child = Arc::downgrade(&job.identity);
            self.outstanding = Some(child.clone());
            Ok(MarkdownShapingFrontierPart {
                shaping_seams: self.shaping_seams.clone(), // Actual context support may reach the seam, so audit retains its original proof.
                scope_complete: range.end == self.original.end
                    || (self.open_scope
                        && self
                            .unicode
                            .as_ref()
                            .is_some_and(|state| state.support_end() == self.original.end)),
                job,
                original: self.original.clone(),
                continuation: MarkdownShapingFrontierContinuation {
                    owner: Arc::downgrade(&self.identity),
                    serial: self.serial,
                    next: range.end,
                    next_byte,
                    child,
                    font_splice: false,
                },
            })
        })();
        match result {
            Ok(part) => Ok(part),
            Err(error) => self.fail(error),
        }
    }

    pub(crate) fn resume(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        continuation: MarkdownShapingFrontierContinuation,
    ) -> FrontierResult<()> {
        let result = (|| {
            self.guard(source, font)?;
            if !Weak::ptr_eq(&continuation.owner, &Arc::downgrade(&self.identity))
                || continuation.serial != self.serial
                || self
                    .outstanding
                    .as_ref()
                    .is_none_or(|child| !Weak::ptr_eq(child, &continuation.child))
            {
                return Err(MarkdownShapingFrontierError::ContinuationMismatch);
            }
            if continuation.child.strong_count() != 0 {
                return Err(MarkdownShapingError::Pending.into());
            }
            self.next = continuation.next;
            self.next_byte = continuation.next_byte;
            self.scanned = 0;
            self.candidate = None;
            self.word_candidate = false;
            self.local_verified = 0;
            if let Some(unicode) = &mut self.unicode {
                if continuation.font_splice {
                    unicode.reset_after_font_run(self.next_byte, self.next);
                } else {
                    unicode.reset(self.next_byte, self.next);
                } // Foreign font runs cannot infer GCB counts from old center length; original-font parts keep their known ordinal.
            }
            self.outstanding = None;
            self.answer = MarkdownShapingFrontierAnswer::Pending;
            Ok(()) // Clear scratch only after the actual child returns; unread tail remains private cursor state.
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.fail(error),
        }
    }
}

#[cfg(test)]
#[path = "frontier/tests.rs"]
mod tests;
