//! Borrow only an actual glyph producer for measurement; certified reshaping is a separate capability, and a stored directory does not impersonate a Job.
use super::*;

/// External crates cannot implement this contract. Internal adapters represent an original Job or a directory certified by the actual Store.
pub(crate) mod sealed {
    pub trait Sealed {}
}
#[derive(Clone)]
pub(crate) enum GlyphProviderOrigin {
    Job(Weak<()>),
    #[cfg(feature = "paged-line-compose")]
    Stored {
        store: Weak<()>,
        origin: u64,
    },
}
impl PartialEq for GlyphProviderOrigin {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Job(left), Self::Job(right)) => Weak::ptr_eq(left, right),
            #[cfg(feature = "paged-line-compose")]
            (
                Self::Stored {
                    store: left,
                    origin: a,
                },
                Self::Stored {
                    store: right,
                    origin: b,
                },
            ) => *a == *b && Weak::ptr_eq(left, right),
            #[cfg(feature = "paged-line-compose")]
            _ => false, // Equal text/ranges cannot substitute a Job identity for an entry signed into a Store.
        }
    }
}
impl Eq for GlyphProviderOrigin {}

/// Facts returned after successful authorization; copying these fields does not replace the Source/Font/actual completion checks.
pub(crate) struct GlyphProviderFacts {
    pub origin: GlyphProviderOrigin,
    pub source_utf16: Range<usize>,
    pub glyphs: usize,
    pub generation: u64,
    pub direction: MarkdownShapingDirection,
}
pub(crate) trait GlyphProvider: sealed::Sealed {
    /// Read only the identity bound at construction, without I/O or accessing another Font; Measure checks its original Source error first.
    fn origin(&self) -> GlyphProviderOrigin;
    /// Recheck original Source health, Font authority, cancellation, and the published range's actual completion witness on every call.
    /// Return finite facts rather than a background borrow or a strong producer owner.
    fn require(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<GlyphProviderFacts>;
    /// Read an original logical cluster; unresolved dependencies remain Pending, while a hole inside complete coverage is an error.
    fn cluster(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer>;
    /// Only a fold control signed by the original Store authorizes skipping hidden text; a Job cannot derive that permission from a public range.
    #[cfg(feature = "paged-line-compose")]
    fn folded_section(&self) -> Option<Range<usize>> {
        None
    }
    /// An actual paragraph control signed into the Store has no following glyph flags; an ordinary shaping Job does not gain that terminal capability.
    #[cfg(feature = "paged-line-compose")]
    fn ends_paragraph_control(&self) -> bool {
        false
    }
    /// Only the actual Store adapter can read its latest complete LineEnd; a Job or a range alone has no committed-line authority.
    #[cfg(feature = "paged-line-compose")]
    fn committed_line_start(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        _selected: &SelectedMeasureSelection,
    ) -> Result<CommittedLineStart> {
        self.require(source, font)?;
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    }
    /// Recheck that same latest LineEnd before consuming the next-line ticket; an old ticket cannot reopen an earlier line or another Store.
    #[cfg(feature = "paged-line-compose")]
    fn require_committed_line_start(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        _committed: &CommittedLineStart,
    ) -> Result<()> {
        self.require(source, font)?;
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    }
    /// Only an actual frontier child or a same-origin signed Store directory can supply this word end; it does not extend current glyph coverage.
    #[cfg(feature = "paged-shaping-frontier")]
    fn frontier_word_end(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Option<usize>> {
        self.require(source, font)?;
        Ok(None) // A producer without the original frontier certificate cannot manufacture that capability from public coordinates.
    }
    /// An ordinary Job has no original Bidi segment ticket; only a directory verified by the Store/Proof can supply an internal shaping end.
    /// This permits continued accumulation, not a grapheme, soft-line, or emergency break.
    #[cfg(feature = "paged-shaping-frontier")]
    fn bidi_shaping_end(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Option<usize>> {
        self.require(source, font)?;
        Ok(None)
    }
}

/// Only an actual original Job or certified Store directory supplies reshaping; a public Range/Choice cannot implement that authority.
/// The original selector chooses subranges, retaining language, features, Font, and globally adjacent context.
/// BOT/EOT is inherited only at corresponding original item endpoints; a soft line does not create a physical paragraph boundary.
#[cfg(feature = "paged-soft-line-resolve")]
pub(crate) trait ResolutionProvider: GlyphProvider {
    fn shaping_cancellation(&self) -> crate::MarkdownShapingCancellation;
    /// The same Store can supply certified first-cluster flags from its next item; a Job without that witness cannot treat its item end as EOF.
    fn following_item_flags(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> std::result::Result<Option<u32>, crate::MarkdownSoftLineResolveError> {
        self.require(source, font)?;
        Ok(None)
    }
    fn reshape_subrange(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
    ) -> std::result::Result<MarkdownShapingJob, crate::MarkdownSoftLineResolveError>;
}

/// The public Job-backed entry points borrow the original Job and existing index; construction neither duplicates an index nor copies glyph data.
pub(crate) struct JobGlyphProvider<'a> {
    job: &'a mut MarkdownShapingJob,
    clusters: Option<&'a mut MarkdownGlyphClusterIndex>,
}
impl<'a> JobGlyphProvider<'a> {
    pub(crate) fn new(
        job: &'a mut MarkdownShapingJob,
        clusters: Option<&'a mut MarkdownGlyphClusterIndex>,
    ) -> Self {
        Self { job, clusters }
    }
}
impl sealed::Sealed for JobGlyphProvider<'_> {}
impl GlyphProvider for JobGlyphProvider<'_> {
    fn origin(&self) -> GlyphProviderOrigin {
        GlyphProviderOrigin::Job(self.job.index_identity())
    }
    fn require(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<GlyphProviderFacts> {
        // The out-of-range glyph read verifies the original Job's actual terminal state and authorities; the profile supplies only finite metadata.
        let profile = self.job.profile()?;
        if self.job.glyph(source, font, profile.glyph_count)?.is_some() {
            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
        } // The actual EOF getter checks Ready and both original authorities; a profile length cannot certify a partially filled Buffer.
        Ok(GlyphProviderFacts {
            origin: self.origin(),
            source_utf16: self.job.source_range(),
            glyphs: profile.glyph_count,
            generation: profile.source_generation,
            direction: self.job.direction(),
        })
    }
    fn cluster(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        self.require(source, font)?;
        // Construction may omit the cluster index for operations that need only facts; actual cluster reads require the caller's original index.
        let value = self
            .clusters
            .as_mut()
            .ok_or(MarkdownSoftLineMeasureError::InvalidRequest)?
            .containing(self.job, offset)?;
        self.require(source, font)?;
        Ok(value) // Recheck the same Job after index I/O; failure prevents publication of a Copy record obtained from an old page.
    }
    #[cfg(feature = "paged-shaping-frontier")]
    fn frontier_word_end(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Option<usize>> {
        Ok(self.job.frontier_word_end(source, font)?)
    }
}

#[cfg(feature = "paged-soft-line-resolve")]
impl ResolutionProvider for JobGlyphProvider<'_> {
    fn shaping_cancellation(&self) -> crate::MarkdownShapingCancellation {
        self.job.cancellation()
    }
    fn reshape_subrange(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
    ) -> std::result::Result<MarkdownShapingJob, crate::MarkdownSoftLineResolveError> {
        Ok(self.job.reshape_subrange(source, font, range)?)
    } // The original Job retains its validated style; no new output identity is invented and no unshaped tail is read early.
}

impl GlyphProviderOrigin {
    pub(super) fn same_provider_family(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Job(_), Self::Job(_)) => true,
            #[cfg(feature = "paged-line-compose")]
            (Self::Stored { store: left, .. }, Self::Stored { store: right, .. }) => {
                Weak::ptr_eq(left, right)
            }
            #[cfg(feature = "paged-line-compose")]
            _ => false,
        } // Item origins may change within one Store; a second same-Source file or a Job cannot replace that Store family.
    }
}

/// Exclusive access to the original Measure is the handoff ticket; it does not retain the previous provider or overlap mutable Store borrows.
/// Dropping an unused ticket ends only the borrow: NeedNextItem remains unchanged, with no implicit advancement, retry, or success receipt.
pub(crate) struct PreparedMeasureContinuation<'a> {
    measure: &'a mut MarkdownSoftLineMeasureJob,
}
impl<'a> PreparedMeasureContinuation<'a> {
    pub(super) fn new(measure: &'a mut MarkdownSoftLineMeasureJob) -> Self {
        Self { measure }
    }
    pub(crate) fn continue_with(
        self,
        next: &mut dyn GlyphProvider,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
    ) -> Result<()> {
        self.measure.continue_prepared(next, font, breaks)
    }
}

/// Carry only the line's weak identity and revocation/cancellation scalars, with no strong Source, Font, file, or Job edge.
#[cfg(feature = "paged-safe-glyph-slices")]
#[derive(Clone)]
pub(crate) struct MeasuredLineScope {
    pub owner: Weak<()>,
    pub alive: Arc<AtomicBool>,
    pub cancellation: MarkdownSoftLineMeasureCancellation,
}

/// Fixed facts for the producer's current admission; the unforgeable permit and actual provider checks remain the authority.
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) struct MeasuredItemFacts {
    pub source: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub origin: GlyphProviderOrigin,
    pub source_utf16: Range<usize>,
    pub measured_utf16: Range<usize>,
    pub glyphs: usize,
    pub generation: u64,
    pub direction: MarkdownShapingDirection,
    pub line: MeasuredLineScope,
}

/// Temporarily borrow the original Measure without cloning or persistent reconstruction; the same Store can be reborrowed after its previous item borrow ends.
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) struct MeasuredItemPermit<'a> {
    measure: &'a MarkdownSoftLineMeasureJob,
}
#[cfg(feature = "paged-safe-glyph-slices")]
impl<'a> MeasuredItemPermit<'a> {
    pub(super) fn new(measure: &'a MarkdownSoftLineMeasureJob) -> Self {
        Self { measure }
    }
    pub(crate) fn require(&self, provider: &mut dyn GlyphProvider) -> Result<MeasuredItemFacts> {
        self.measure.guard_provider(provider)?;
        self.measure.require_next_item()?;
        Ok(MeasuredItemFacts {
            source: self.measure.source.clone(),
            font: self.measure.font.clone(),
            origin: self.measure.origin.clone(),
            source_utf16: self.measure.item.clone(),
            measured_utf16: self.measure.start.max(self.measure.item.start)..self.measure.item.end,
            glyphs: self.measure.glyph_count,
            generation: self.measure.generation,
            direction: self.measure.direction,
            line: MeasuredLineScope {
                owner: Arc::downgrade(&self.measure.identity),
                alive: Arc::clone(&self.measure.line_alive),
                cancellation: self.measure.cancellation.clone(),
            },
        }) // These facts describe actual item consumption, not a final line end, GCB safety, or Native drawing readiness.
    }
}

/// The current provider anchors the original selection's authority; selection may lie in an earlier item, so current coverage is not whole-line coverage.
#[cfg(feature = "paged-line-compose")]
pub(crate) struct SelectedMeasureFacts {
    pub source: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub current: GlyphProviderFacts,
    pub selected: SelectedMeasureSelection,
    pub line: MeasuredLineScope,
}

/// Preserve whether the actual selection is a natural opportunity or an emergency GCB; an emergency slice never becomes Allowed/Mandatory.
#[cfg(feature = "paged-line-compose")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SelectedMeasureKind {
    Natural(MarkdownLineBreakBoundary),
    #[cfg(feature = "paged-shaping-frontier")]
    Emergency {
        overflowing_grapheme: bool,
    },
}

/// Only the original Measure's internal selection produces these facts; Store must still verify the original line ticket and actual historical glyph coverage.
#[cfg(feature = "paged-line-compose")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedMeasureSelection {
    pub source_utf16: Range<usize>,
    pub x_advance_26_6: i64,
    pub boundary_flags: u32,
    pub kind: SelectedMeasureKind,
}

/// Fixed commit facts read independently by the original Store; no file/text owner is retained, and that Store must recheck the actual record.
#[cfg(feature = "paged-line-compose")]
#[derive(Clone)]
pub(crate) struct CommittedLineStart {
    pub source_utf16: Range<usize>,
    pub generation: u64,
    pub record: usize,
    pub ordinal: usize,
    pub kind: SelectedMeasureKind,
    pub x_advance_26_6: i64,
}

/// A non-Clone, single-use ticket may outlive the old Measure with fixed facts, but retains no Source, Font, Job, or FD owner.
#[cfg(feature = "paged-line-compose")]
pub(crate) struct SelectedLineStart {
    source: MarkdownLayoutTextAuthority,
    family: GlyphProviderOrigin,
    cancellation: MarkdownSoftLineMeasureCancellation,
    committed: CommittedLineStart,
}

#[cfg(feature = "paged-line-compose")]
impl SelectedLineStart {
    pub(super) fn require(
        &self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        provider: &mut dyn GlyphProvider,
        start: usize,
        cancellation: &MarkdownSoftLineMeasureCancellation,
    ) -> Result<()> {
        source.health()?; // Report the actual Source's first errno before an expired ticket, foreign Store, or cancellation.
        // Bind the single ticket to this Source generation, exact committed end, and original long-lived cancellation request.
        if authority != &self.source
            || !source.permits(&self.source)
            || source.generation() != self.committed.generation
            || start != self.committed.source_utf16.end
            || !self.cancellation.same_request(cancellation)
        {
            return Err(MarkdownSoftLineMeasureError::SourceMismatch);
        }
        if self.cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        let facts = provider.require(authority, font)?;
        if !self.family.same_provider_family(&facts.origin)
            || facts.generation != self.committed.generation
        {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        }
        // Store must read the latest LineEnd's original record, kind, range, and ordinal.
        // Equal Source/record coordinates are insufficient; a later commit prevents an old ticket from reopening an earlier line.
        let result = provider.require_committed_line_start(authority, font, &self.committed);
        source.health()?;
        result?;
        if self.cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        Ok(())
    }
}

/// This non-Clone permit lives only within the original Measure borrow. The producer must prove coverage and Font from that Store's actual history,
/// then verify the actual GCB, cluster boundary, and L1/L2; public ranges do not authorize arbitrary line assembly.
#[cfg(feature = "paged-line-compose")]
pub(crate) struct SelectedMeasurePermit<'a> {
    measure: &'a MarkdownSoftLineMeasureJob,
}
#[cfg(feature = "paged-line-compose")]
impl<'a> SelectedMeasurePermit<'a> {
    pub(super) fn new(measure: &'a MarkdownSoftLineMeasureJob) -> Self {
        Self { measure }
    }
    pub(crate) fn require(&self, provider: &mut dyn GlyphProvider) -> Result<SelectedMeasureFacts> {
        self.measure.guard_provider(provider)?;
        let selected = self.measure.require_selected()?;
        Ok(SelectedMeasureFacts {
            source: self.measure.source.clone(),
            font: self.measure.font.clone(),
            current: GlyphProviderFacts {
                origin: self.measure.origin.clone(),
                source_utf16: self.measure.item.clone(),
                glyphs: self.measure.glyph_count,
                generation: self.measure.generation,
                direction: self.measure.direction,
            },
            selected,
            line: MeasuredLineScope {
                owner: Arc::downgrade(&self.measure.identity),
                alive: Arc::clone(&self.measure.line_alive),
                cancellation: self.measure.cancellation.clone(),
            },
        }) // Copy only fixed metadata; historical glyph access still borrows the single Store and does not keep an old Job or Font alive.
    }
    /// Call only after Store completes its actual LineEnd and finally verifies this permit; one selection issues at most one next-line ticket.
    pub(crate) fn next_start(&self, provider: &mut dyn GlyphProvider) -> Result<SelectedLineStart> {
        let selected = self.require(provider)?;
        // Only one actual stored selection may issue a successor start; a Job's measured Choice alone cannot certify a committed LineEnd.
        if self.measure.start_issued.get()
            || !matches!(selected.current.origin, GlyphProviderOrigin::Stored { .. })
        {
            return Err(MarkdownSoftLineMeasureError::InvalidRequest);
        }
        let committed =
            provider.committed_line_start(&selected.source, &selected.font, &selected.selected);
        // Reading the Store may expose a Source failure or cancellation; check the original line before consuming the returned commit facts.
        self.measure.require_live_line()?;
        let committed = committed?;
        // Match the actual committed line against this exact selection before marking the single-use ticket as issued.
        if committed.source_utf16 != selected.selected.source_utf16
            || committed.generation != selected.current.generation
            || committed.kind != selected.selected.kind
            || committed.x_advance_26_6 != selected.selected.x_advance_26_6
        {
            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
        }
        self.measure.start_issued.set(true);
        Ok(SelectedLineStart {
            source: selected.source,
            family: selected.current.origin,
            cancellation: self.measure.cancellation.clone(),
            committed,
        }) // Incomplete/failed writes issue no ticket; dropping it releases fixed weak metadata without changing immutable glyphs.
    }
}

/// Temporarily borrow the original Measure that actually selected an unsafe/emergency boundary; a copied public Choice has no construction authority.
/// Reborrowed directories must match the original origin; equal Source does not reassign a cross-item candidate to the current directory.
#[cfg(feature = "paged-line-compose")]
pub(crate) struct ResolutionMeasurePermit<'a> {
    measure: &'a MarkdownSoftLineMeasureJob,
}
/// Fixed facts for that same original selection retain no strong Source/Font; authority is still rechecked through the original permit.
#[cfg(feature = "paged-line-compose")]
pub(crate) struct ResolutionMeasureFacts {
    pub source: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub origin: GlyphProviderOrigin,
    pub line: MeasuredLineScope,
}
#[cfg(feature = "paged-line-compose")]
impl<'a> ResolutionMeasurePermit<'a> {
    pub(super) fn new(measure: &'a MarkdownSoftLineMeasureJob) -> Self {
        Self { measure }
    }
    pub(crate) fn require(&self, provider: &mut dyn GlyphProvider) -> Result<()> {
        self.measure.guard_provider(provider)?;
        self.measure.require_resolution()?;
        self.measure.require_live_line()
    }
    pub(crate) fn seed(
        &self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<MarkdownSoftLineResolveSeed> {
        self.require(provider)?;
        self.measure.resolve_seed_provider(provider)
    }
    pub(crate) fn facts(&self, provider: &mut dyn GlyphProvider) -> Result<ResolutionMeasureFacts> {
        self.require(provider)?;
        Ok(ResolutionMeasureFacts {
            source: self.measure.source.clone(),
            font: self.measure.font.clone(),
            origin: self.measure.origin.clone(),
            line: MeasuredLineScope {
                owner: Arc::downgrade(&self.measure.identity),
                alive: Arc::clone(&self.measure.line_alive),
                cancellation: self.measure.cancellation.clone(),
            },
        }) // One line scope does not accumulate Weak sets as item count grows, and cannot reconstruct another measurement request.
    }
    pub(crate) fn measure(&self) -> &MarkdownSoftLineMeasureJob {
        self.measure
    } // The shared selector may recheck the original nonce; this does not authorize constructing another Measure or changing its start.
}
