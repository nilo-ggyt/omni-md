//! Reshape actual prefixes from the admitted Store directory. Reuse the original selector;
//! do not manufacture a Job or Measure to impersonate stored output.
use super::*;
use crate::markdown_soft_line_measure::provider::{
    GlyphProviderOrigin, MeasuredLineScope, ResolutionMeasurePermit,
};
use std::sync::Weak;

/// Facts copied from the admitted packet only. Store line publication must still borrow the same
/// signed packet, rather than accept a Range paired with an arbitrary glyph array.
pub(crate) struct MarkdownStoredResolvedPrefixFacts {
    pub boundary: MarkdownEmergencyShapedPrefixBoundary,
    pub source: crate::MarkdownLayoutTextAuthority,
    pub font: crate::MarkdownLayoutFontAuthority,
    pub origin: GlyphProviderOrigin,
    pub line: MeasuredLineScope,
}

/// Constructed only by handing off the original stored selector's actually selected child, with
/// no public fields, Clone, or Range factory. Retain the original whole-line weak scope and one
/// concrete child, not the original item Job, Store file, or a history of receipts.
pub(crate) struct MarkdownStoredResolvedPrefix {
    boundary: MarkdownEmergencyShapedPrefixBoundary,
    source: crate::MarkdownLayoutTextAuthority,
    font: crate::MarkdownLayoutFontAuthority,
    origin: GlyphProviderOrigin,
    line: MeasuredLineScope,
    child_identity: Weak<()>,
    child: Option<MarkdownSoftLineResolvedShape>,
    backing: Option<MarkdownOriginalBackingObserver>,
}
impl MarkdownStoredResolvedPrefix {
    fn check(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<()> {
        // Backing health takes precedence. This actual child must accept the packet's original
        // Source/Font, and the fresh borrow must still describe its original measure/provider scope.
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let child = self
            .child
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let profile = child.require_authorities(&self.source, &self.font)?;
        let current = permit.facts(provider)?;
        if self.source != current.source
            || self.origin != current.origin
            || !Weak::ptr_eq(&self.line.owner, &current.line.owner)
            || !Arc::ptr_eq(&self.line.alive, &current.line.alive)
            || !self
                .line
                .cancellation
                .same_request(&current.line.cancellation)
            || !Weak::ptr_eq(&self.child_identity, &child.instance_identity())
        {
            return Err(MarkdownSoftLineMeasureError::JobMismatch.into());
        } // Equal coordinates/glyph counts, a same-named font, new Measure, or another Store directory cannot authorize this prefix.
        if !self.line.alive.load(Ordering::Acquire)
            || self.line.owner.strong_count() == 0
            || self.line.cancellation.is_cancelled()
        {
            return Err(MarkdownSoftLineMeasureError::Cancelled.into());
        } // Cancellation or original-line destruction revokes authority only; finish below actually returns this packet's child.
        child.require_authorities(&current.source, &current.font)?;
        if profile.glyph_count != self.boundary.glyphs
            || child.source_range()? != self.boundary.source_utf16
        {
            return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
        }
        Ok(()) // Glyph width/flags came from the prior complete bounded scan of this same irreplaceable Ready child.
    }
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            drop(self.child.take());
        } // Identity/I/O/cancellation failure permanently ends the child; another request cannot retry through this old packet.
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        result
    }
    pub(crate) fn require(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<()> {
        // Both successful access and rejection pass through the same original-child retirement path.
        let result = self.check(permit, provider);
        self.finish(result)
    }
    fn with_child<T>(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        visit: impl FnOnce(&mut MarkdownSoftLineResolvedShape) -> Result<T>,
    ) -> Result<T> {
        let result = (|| {
            self.check(permit, provider)?;
            let value = visit(
                self.child
                    .as_mut()
                    .ok_or(MarkdownSoftLineResolveError::Poisoned)?,
            )?;
            self.check(permit, provider)?;
            Ok(value)
        })();
        self.finish(result) // Every actual getter rechecks the same original ticket before/after; first admission is not permanent cache authority.
    }
    pub(crate) fn facts(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<MarkdownStoredResolvedPrefixFacts> {
        // Copy only certified facts. These fields cannot independently recreate the signed owner
        // required by Store publication or replace its concrete child.
        self.require(permit, provider)?;
        Ok(MarkdownStoredResolvedPrefixFacts {
            boundary: self.boundary.clone(),
            source: self.source.clone(),
            font: self.font.clone(),
            origin: self.origin.clone(),
            line: self.line.clone(),
        })
    }
    pub(crate) fn glyph(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        ordinal: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        // Return the original child's actual glyph only while the same measure/provider scope survives.
        self.with_child(permit, provider, |child| child.glyph(ordinal))
    }
    pub(crate) fn cluster_index(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<MarkdownGlyphClusterIndex> {
        // Bind a real cluster index to this concrete resolved child, not the original stored item.
        self.with_child(
            permit,
            provider,
            MarkdownSoftLineResolvedShape::cluster_index,
        )
    }
    pub(crate) fn step_clusters(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        index: &mut MarkdownGlyphClusterIndex,
        budget: usize,
    ) -> Result<crate::MarkdownGlyphClusterStep> {
        // Keep both packet authority and the index's actual child nonce current for every bounded turn.
        self.with_child(permit, provider, |child| child.step_clusters(index, budget))
    }
    pub(crate) fn containing_cluster(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        index: &mut MarkdownGlyphClusterIndex,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        // A containing-cluster answer must come from this child-bound index under the same original scope.
        self.with_child(permit, provider, |child| {
            child.containing_cluster(index, offset)
        })
    }
    pub(crate) fn close(mut self) -> Result<()> {
        let result = self
            .child
            .take()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)
            .and_then(MarkdownSoftLineResolvedShape::close);
        self.finish(result) // Actual child.close settles mappings/descriptors; this packet fabricates no zero-resource receipt.
    }
}

/// Retain the original selector and observation of one outstanding actual child. Unresolved
/// remainder carries private FIFO coordinates only. The caller still owns the original Measure;
/// each operation reborrows its permit and the same directory rather than restoring authority from disk.
pub(crate) struct MarkdownStoredPrefixResolveJob {
    inner: MarkdownSoftLineResolveJob,
    complete: Option<(usize, bool)>,
    prefix_outstanding: bool,
}
impl MarkdownStoredPrefixResolveJob {
    pub(crate) fn new(
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<Self> {
        // Capture the selected original seed and actual provider cancellation once. Defer the
        // unconsumed tail explicitly, without constructing a pretend Ready remainder provider.
        let seed = permit.seed(provider)?;
        let mut inner =
            MarkdownSoftLineResolveJob::from_seed(seed, provider.shaping_cancellation())?;
        inner
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?
            .defer_tail = true;
        inner.guard_provider(permit.measure(), provider)?;
        Ok(Self {
            inner,
            complete: None,
            prefix_outstanding: false,
        }) // A cross-item original selection remains NextItem; current font/directory cannot renew historical selection authority.
    }

    pub(crate) fn cancellation(&self) -> MarkdownSoftLineResolveCancellation {
        self.inner.cancellation()
    }
    pub(crate) fn resource_observer(&self) -> MarkdownSoftLineResolveResourceObserver {
        self.inner.resource_observer()
    }
    fn guard(
        &self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<()> {
        // The permit checks the actual selected stage; inner separately checks its construction-time
        // Measure nonce. Even same-source/same-range new measurement cannot substitute. Both guards
        // prioritize original Source health rather than hiding I/O failure behind cached directory length.
        permit.require(provider)?;
        self.inner.guard_provider(permit.measure(), provider)
    }
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        // Failure retires the selector's actual payload before examining backing cleanup facts;
        // the cleared shell cannot recapture authority on a later call.
        if result.is_err() {
            drop(self.inner.payload.take());
        }
        if let Some(errno) = self
            .inner
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        } // Actual backing failure during retirement still outranks outer control/cancellation errors.
        result
    }

    pub(crate) fn step(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownEmergencyGlyphSliceStep> {
        // Advance only through the existing selector, then settle rejection through this owner.
        let result = self.advance(permit, provider, breaks, budget);
        self.finish(result)
    }
    fn advance(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownEmergencyGlyphSliceStep> {
        self.guard(permit, provider)?;
        // Reject an empty work turn only after checking original authority; zero budgets are
        // invalid requests rather than successful no-op resolution or permission to skip guards.
        if budget.input_bytes == 0
            || budget.input_scalars == 0
            || budget.glyphs == 0
            || budget.candidates == 0
        {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        if self.prefix_outstanding {
            if self.inner.resources.snapshot().active_jobs != 0 {
                return Ok(empty(
                    MarkdownEmergencyGlyphSliceAnswer::PendingPrefixConsumer,
                ));
            }
            self.prefix_outstanding = false;
        } // The previous handed-off child must actually drop; retaining many children cannot count as bounded continuation resources.
        if let Some((end, eof)) = self.complete {
            // Completion certifies this selected prefix scope; source_eof is separate and may be false.
            return Ok(empty(MarkdownEmergencyGlyphSliceAnswer::PrefixComplete {
                source_utf16_end: end,
                source_eof: eof,
            }));
        }
        let step = self
            .inner
            .step_provider(permit.measure(), provider, breaks, budget)?;
        let payload = self
            .inner
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let answer = if matches!(payload.stage, Stage::ShapedPrefix) {
            MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(emergency::prefix_boundary(payload)?)
        } else {
            match step.answer {
                MarkdownSoftLineResolveAnswer::Yielded => {
                    MarkdownEmergencyGlyphSliceAnswer::Yielded
                }
                MarkdownSoftLineResolveAnswer::PendingDependencies => {
                    MarkdownEmergencyGlyphSliceAnswer::PendingDependencies
                }
                MarkdownSoftLineResolveAnswer::Pending(reason) => {
                    MarkdownEmergencyGlyphSliceAnswer::Pending(reason)
                }
                MarkdownSoftLineResolveAnswer::EmptySource
                | MarkdownSoftLineResolveAnswer::Resolved(_) => {
                    return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
                } // This entry delivers actual nonempty prefixes only; a full tail or empty document is not successful unsafe-prefix resolution.
            }
        };
        self.guard(permit, provider)?;
        Ok(MarkdownEmergencyGlyphSliceStep {
            input_bytes: step.input_bytes,
            // Store forwards actual work from the original Job; directory handoff grants no new input budget.
            input_scalars: step.input_scalars,
            context_bytes: step.context_bytes,
            glyphs: step.glyphs,
            candidates: step.candidates,
            shapes: step.shapes,
            answer,
        })
    }

    pub(crate) fn take_shaped_prefix(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<(
        MarkdownEmergencyShapedPrefixBoundary,
        MarkdownSoftLineResolvedShape,
    )> {
        // Remove the one actually Ready child only after verifying its current selector phase.
        // Advancing the private source cursor does not resolve or shape the unconsumed remainder.
        let result = (|| {
            self.guard(permit, provider)?;
            if self.prefix_outstanding {
                return Err(MarkdownSoftLineResolveError::InvalidRequest);
            }
            let payload = self
                .inner
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            let boundary = emergency::prefix_boundary(payload)?;
            let line = payload
                .line
                .take()
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
            let line = MarkdownSoftLineResolvedShape::new(
                line,
                payload.seed.source.clone(),
                payload.seed.authority.clone(),
                payload.seed.font.clone(),
                false,
            )?;
            self.complete = emergency::advance_source_start(payload, boundary.source_utf16.end)?;
            self.prefix_outstanding = true;
            self.guard(permit, provider)?;
            Ok((boundary, line))
        })();
        self.finish(result)
    }
    /// Store accepts this closed packet for line publication. Public boundary facts plus an
    /// arbitrary reshaped value cannot replace the original selection's concrete child.
    pub(crate) fn take_signed_prefix(
        &mut self,
        permit: &ResolutionMeasurePermit<'_>,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<MarkdownStoredResolvedPrefix> {
        let result = (|| {
            self.guard(permit, provider)?;
            let facts = permit.facts(provider)?;
            let (boundary, child) = self.take_shaped_prefix(permit, provider)?;
            let mut prefix = MarkdownStoredResolvedPrefix {
                boundary,
                source: facts.source,
                font: facts.font,
                origin: facts.origin,
                line: facts.line,
                child_identity: child.instance_identity(),
                child: Some(child),
                backing: self.inner.backing.clone(),
            };
            prefix.require(permit, provider)?;
            Ok(prefix)
        })();
        self.finish(result) // Admission failure after selector progress retires the whole operation; no failed prefix may be skipped for a later line.
    }
    pub(crate) fn close(self) -> Result<MarkdownSoftLineResolveResources> {
        // The original resolver owns actual selector/child resource accounting and final cleanup.
        self.inner.close()
    }
}
fn empty(answer: MarkdownEmergencyGlyphSliceAnswer) -> MarkdownEmergencyGlyphSliceStep {
    // Observation/backpressure carries no new work. Its actual answer still distinguishes
    // pending consumption from completion and from unavailable dependencies.
    MarkdownEmergencyGlyphSliceStep {
        input_bytes: 0,
        input_scalars: 0,
        context_bytes: 0,
        glyphs: 0,
        candidates: 0,
        shapes: 0,
        answer,
    }
}
