//! Continue emergency lines from the same original Ready glyph array. Share the original
//! selector instead of repeatedly measuring or shaping the entire remaining source.
use super::*;
use crate::{MarkdownSafeGlyphSlice, MarkdownSafeGlyphSliceError};

/// Facts for the original glyph slice and actual source range only, without a fabricated tail
/// flags union or any claim that Native layout is Ready.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownEmergencyGlyphSliceBoundary {
    pub source_utf16: std::ops::Range<usize>,
    pub remainder_utf16: std::ops::Range<usize>,
    pub kind: MarkdownSoftLineResolvedKind,
    pub x_advance_26_6: i64,
    pub glyphs: usize,
}
/// Describe only the actually shaped prefix. Remainder contains unconsumed source coordinates,
/// with no promise of tail glyph count, flags, or Ready shaping output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownEmergencyShapedPrefixBoundary {
    pub source_utf16: std::ops::Range<usize>,
    pub remainder_utf16: std::ops::Range<usize>,
    pub kind: MarkdownSoftLineResolvedKind,
    pub x_advance_26_6: i64,
    pub glyphs: usize,
    pub prefix_flags_union: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownEmergencyGlyphSliceAnswer {
    Yielded,
    PendingDependencies,
    Pending(MarkdownSoftLineResolvePending),
    ReusableLine(MarkdownEmergencyGlyphSliceBoundary),
    Reshaped(MarkdownSoftLineResolvedBoundary),
    ShapedPrefix(MarkdownEmergencyShapedPrefixBoundary),
    PendingPrefixConsumer,
    PrefixComplete {
        source_utf16_end: usize,
        source_eof: bool,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownEmergencyGlyphSliceStep {
    pub input_bytes: usize,
    /// Actual scalars consumed by original reshape work; safe reuse stays zero rather than
    /// converting glyph counts into fabricated scalar consumption.
    pub input_scalars: usize,
    /// Actual adjacent-context reads of newly created jobs, forwarded from the original resolver
    /// turn separately from its text input.
    pub context_bytes: usize,
    pub glyphs: usize,
    pub candidates: usize,
    pub shapes: usize,
    pub answer: MarkdownEmergencyGlyphSliceAnswer,
}
/// Count original-item shaping separately from subsequent actual input and shaping. Metadata
/// candidate work remains visible rather than being described as zero work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownEmergencyGlyphSliceWork {
    pub original_input_scalars: usize,
    pub original_glyphs: usize,
    pub new_shape_input_bytes: usize,
    pub shape_calls: usize,
    pub candidates: usize,
    pub lines: usize,
}
impl MarkdownEmergencyGlyphSliceWork {
    fn add(&mut self, step: &MarkdownSoftLineResolveStep) -> Result<()> {
        // Preserve checked cumulative work from the same resolver turns. Overflow is an error,
        // not a wrapped low count that could falsely imply bounded processing.
        self.new_shape_input_bytes = self
            .new_shape_input_bytes
            .checked_add(step.input_bytes)
            .ok_or(MarkdownSoftLineResolveError::Overflow)?;
        self.shape_calls = self
            .shape_calls
            .checked_add(step.shapes)
            .ok_or(MarkdownSoftLineResolveError::Overflow)?;
        self.candidates = self
            .candidates
            .checked_add(step.candidates)
            .ok_or(MarkdownSoftLineResolveError::Overflow)?;
        Ok(())
    }
}
/// Not Clone; consumption transfers the actual safe slice. Public boundary copies cannot
/// authorize a successor start position.
pub struct MarkdownEmergencyGlyphSliceLine {
    boundary: MarkdownEmergencyGlyphSliceBoundary,
    slice: MarkdownSafeGlyphSlice,
}
impl MarkdownEmergencyGlyphSliceLine {
    #[must_use]
    pub fn boundary(&self) -> MarkdownEmergencyGlyphSliceBoundary {
        self.boundary.clone()
    }
    #[must_use]
    pub fn into_parts(self) -> (MarkdownEmergencyGlyphSliceBoundary, MarkdownSafeGlyphSlice) {
        (self.boundary, self.slice)
    }
}

/// Worker-local continuation of one item, retaining one pending slice and finite original
/// selector state without creating another glyph index.
pub struct MarkdownEmergencyGlyphSliceJob {
    inner: MarkdownSoftLineResolveJob,
    line: Option<MarkdownEmergencyGlyphSliceLine>,
    complete: Option<(usize, bool)>,
    prefix_outstanding: bool,
    work: MarkdownEmergencyGlyphSliceWork,
}
impl MarkdownEmergencyGlyphSliceJob {
    /// The original Measure must actually select emergency resolution. Natural breaks use their
    /// existing safe factory, rather than an arbitrary Range admitted by this constructor.
    pub fn new(
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<Self> {
        let mut inner = MarkdownSoftLineResolveJob::new(measure, original)?;
        let payload = inner
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let emergency = matches!(
            payload.seed.choice,
            MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(_)
        );
        #[cfg(feature = "paged-shaping-frontier")]
        let emergency = emergency
            || matches!(
                payload.seed.choice,
                MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix { .. }
            );
        if !emergency {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        payload.reuse_safe = true;
        let profile = original.profile()?;
        inner.guard(measure, original)?;
        Ok(Self {
            inner,
            line: None,
            complete: None,
            prefix_outstanding: false,
            work: MarkdownEmergencyGlyphSliceWork {
                original_input_scalars: profile.input_scalars,
                original_glyphs: profile.glyph_count,
                new_shape_input_bytes: 0,
                shape_calls: 0,
                candidates: 0,
                lines: 0,
            },
        })
    }
    /// Explicitly deliver actual line prefixes only. The separate complete-prefix/tail contract
    /// of new remains intact; selecting this mode does not silently alter another caller's result.
    pub fn new_prefix_only(
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<Self> {
        let mut job = Self::new(measure, original)?;
        job.inner
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?
            .defer_tail = true;
        Ok(job)
    }
    #[must_use]
    pub fn cancellation(&self) -> MarkdownSoftLineResolveCancellation {
        self.inner.cancellation()
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownSoftLineResolveResourceObserver {
        self.inner.resource_observer()
    }
    pub fn work(
        &self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<MarkdownEmergencyGlyphSliceWork> {
        // Work observations require the same original measure/job authority as actual processing.
        self.inner.guard(measure, original)?;
        Ok(self.work)
    }
    fn fail<T>(&mut self, error: MarkdownSoftLineResolveError) -> Result<T> {
        // Revoke and drop this owner's pending slice and real selector payload first. A backing
        // failure revealed during that cleanup retains precedence over the outer error.
        drop(self.line.take());
        drop(self.inner.payload.take());
        if let Some(errno) = self
            .inner
            .backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        Err(error)
    }
    pub fn step(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownEmergencyGlyphSliceStep> {
        // Every failed bounded turn retires this same operation, without keeping a reusable
        // shell that could later borrow another measure's authority.
        let result = self.advance(measure, original, clusters, breaks, budget);
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail(error),
        }
    }
    fn advance(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownEmergencyGlyphSliceStep> {
        self.inner.guard(measure, original)?;
        // A zero budget is an invalid turn, not an excuse to return an unchanged successful
        // answer or bypass the original-owner guard.
        if budget.input_bytes == 0
            || budget.input_scalars == 0
            || budget.glyphs == 0
            || budget.candidates == 0
        {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        if self.prefix_outstanding {
            if self.inner.resources.snapshot().active_jobs != 0 {
                return Ok(empty_step(
                    MarkdownEmergencyGlyphSliceAnswer::PendingPrefixConsumer,
                ));
            }
            self.prefix_outstanding = false;
        } // Start the next line only after actual child destruction; handoff alone neither frees resources nor permits retaining many jobs.
        if let Some(line) = self.line.as_mut() {
            line.slice.require(original)?;
            return Ok(empty_step(MarkdownEmergencyGlyphSliceAnswer::ReusableLine(
                line.boundary.clone(),
            )));
        } // Reobserving one line issues no new slice or work count; only take_line advances the private source start.
        if let Some((end, eof)) = self.complete {
            // Ending this selected prefix does not manufacture source EOF; the stored scalar
            // records whether its actual endpoint equals the original Source's complete length.
            return Ok(empty_step(
                MarkdownEmergencyGlyphSliceAnswer::PrefixComplete {
                    source_utf16_end: end,
                    source_eof: eof,
                },
            ));
        }
        let step = self
            .inner
            .step(measure, original, clusters, breaks, budget)?;
        self.work.add(&step)?;
        let payload = self
            .inner
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let answer = if matches!(payload.stage, Stage::ShapedPrefix) {
            // This branch has one actual Ready child and only coordinates for unresolved remainder.
            MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(prefix_boundary(payload)?)
        } else if matches!(payload.stage, Stage::Reusable) {
            // Safe reuse still requires the original selector choice, GCB, cluster, Source/Font,
            // and actual glyph array. Dependency Pending cannot be relabeled as a safe slice.
            let choice = MarkdownEmergencyResolvedChoice::new(payload)?;
            match MarkdownSafeGlyphSlice::from_emergency_selection(
                &choice, measure, original, breaks, clusters,
            ) {
                Err(MarkdownSafeGlyphSliceError::PendingDependencies) => {
                    MarkdownEmergencyGlyphSliceAnswer::PendingDependencies
                }
                Err(error) => return Err(error.into()),
                Ok(slice) => {
                    let facts = slice.facts();
                    let boundary = MarkdownEmergencyGlyphSliceBoundary {
                        source_utf16: facts.source_utf16,
                        remainder_utf16: payload.selected_end..payload.seed.item.end,
                        kind: payload.kind,
                        x_advance_26_6: facts.x_advance_26_6,
                        glyphs: facts.visual_glyphs.len(),
                    };
                    self.line = Some(MarkdownEmergencyGlyphSliceLine {
                        boundary: boundary.clone(),
                        slice,
                    });
                    MarkdownEmergencyGlyphSliceAnswer::ReusableLine(boundary)
                }
            }
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
                MarkdownSoftLineResolveAnswer::Resolved(boundary) => {
                    MarkdownEmergencyGlyphSliceAnswer::Reshaped(boundary)
                }
                MarkdownSoftLineResolveAnswer::EmptySource => {
                    return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
                }
            } // Unsafe/interior-cluster candidates keep actual reshape results; they are not safe reuse or universal linear-work evidence.
        };
        self.inner.guard(measure, original)?;
        Ok(MarkdownEmergencyGlyphSliceStep {
            input_bytes: step.input_bytes,
            input_scalars: step.input_scalars,
            context_bytes: step.context_bytes,
            glyphs: step.glyphs,
            candidates: step.candidates,
            shapes: step.shapes,
            answer,
        })
    }
    /// Consume the current GCB-verified line in FIFO order. Update the next start privately;
    /// no public Range or externally constructed Start ticket can replace this transition.
    pub fn take_line(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<MarkdownEmergencyGlyphSliceLine> {
        let result = (|| {
            self.inner.guard(measure, original)?;
            let line = self
                .line
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::InvalidRequest)?;
            line.slice.require(original)?;
            let payload = self
                .inner
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            let end = line.boundary.source_utf16.end;
            if !matches!(payload.stage, Stage::Reusable)
                || end != payload.selected_end
                || self.inner.resources.snapshot().active_jobs != 0
            {
                return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
            }
            self.work.lines = self
                .work
                .lines
                .checked_add(1)
                .ok_or(MarkdownSoftLineResolveError::Overflow)?;
            self.complete = advance_source_start(payload, end)?;
            self.inner.guard(measure, original)?;
            self.line
                .take()
                .ok_or(MarkdownSoftLineResolveError::InvalidRequest)
        })();
        match result {
            Ok(line) => Ok(line),
            Err(error) => self.fail(error),
        }
    }
    /// Transfer the actually Ready prefix job. Consume and release it before the next turn;
    /// unresolved remainder still advances only through this selector's private cursor.
    pub fn take_shaped_prefix(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<(
        MarkdownEmergencyShapedPrefixBoundary,
        MarkdownSoftLineResolvedShape,
    )> {
        let result = (|| {
            self.inner.guard(measure, original)?;
            if self.prefix_outstanding || self.line.is_some() {
                return Err(MarkdownSoftLineResolveError::InvalidRequest);
            }
            let payload = self
                .inner
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            let boundary = prefix_boundary(payload)?;
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
            self.work.lines = self
                .work
                .lines
                .checked_add(1)
                .ok_or(MarkdownSoftLineResolveError::Overflow)?;
            self.complete = advance_source_start(payload, boundary.source_utf16.end)?;
            self.prefix_outstanding = true;
            self.inner.guard(measure, original)?;
            Ok((boundary, line))
        })();
        match result {
            Ok(line) => Ok(line),
            Err(error) => self.fail(error),
        }
    }
    /// Only an actual Reshaped answer can transfer complete parts. Other phases cannot create
    /// replacement jobs from copied coordinates.
    pub fn take_reshaped_parts(
        self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<MarkdownSoftLineResolvedParts> {
        self.inner.take_parts(measure, original)
    }
    pub fn close(self) -> Result<MarkdownSoftLineResolveResources> {
        // Drop the pending safe slice before closing the actual resolver; transferred children
        // remain their consumers' real ownership and cannot be counted as already destroyed.
        drop(self.line);
        self.inner.close()
    }
}
fn empty_step(answer: MarkdownEmergencyGlyphSliceAnswer) -> MarkdownEmergencyGlyphSliceStep {
    // Observation and consumer backpressure perform no new shaping work, while retaining their
    // distinct actual answer instead of pretending that Pending is Ready or source EOF.
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

/// Closed FIFO start for the same original Job, derived from its selected actual GCB endpoint,
/// never reconstructed from a public Range or viewport edge.
pub(super) fn advance_source_start(
    payload: &mut Payload,
    end: usize,
) -> Result<Option<(usize, bool)>> {
    // Require genuine forward progress to the currently selected endpoint inside this original
    // limit. Reaching the limit completes this scope; only actual Source end establishes EOF.
    if end <= payload.seed.start || end > payload.limit || end != payload.selected_end {
        return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
    }
    if end == payload.limit {
        payload.stage = Stage::Finished;
        return Ok(Some((end, end == payload.seed.source.utf16_len())));
    }
    payload.seed.start = end;
    payload.next = end;
    payload.candidate = end;
    payload.selected_end = end;
    payload.expected_width = None;
    payload.stage = Stage::Choose;
    payload.kind = MarkdownSoftLineResolvedKind::Natural;
    payload.scan = GlyphFacts::default();
    payload.line_facts = GlyphFacts::default();
    // Preserve emergency mode and original item/font/features/cancellation/nonce. Existing
    // reshape_subrange still reads adjacent context from the original Source. Soft lines create
    // no synthetic BOT/EOT, and unconsumed tail creates neither Ready nor a substitute GlyphProvider.
    Ok(None)
}
pub(super) fn prefix_boundary(payload: &Payload) -> Result<MarkdownEmergencyShapedPrefixBoundary> {
    // Only the actual prefix-only stage with one Ready line and no other trial/tail can report
    // prefix glyph facts. Its remainder coordinates carry no glyph/flags promise.
    if !payload.defer_tail
        || !matches!(payload.stage, Stage::ShapedPrefix)
        || payload.line.is_none()
        || payload.current.is_some()
        || payload.best.is_some()
        || payload.tail.is_some()
        || payload.selected_end <= payload.seed.start
        || payload.selected_end > payload.limit
    {
        return Err(MarkdownSoftLineResolveError::InvalidRequest);
    }
    Ok(MarkdownEmergencyShapedPrefixBoundary {
        source_utf16: payload.seed.start..payload.selected_end,
        remainder_utf16: payload.selected_end..payload.seed.item.end,
        kind: payload.kind,
        x_advance_26_6: payload.line_facts.width,
        glyphs: payload.line_facts.count,
        prefix_flags_union: payload.line_facts.flags,
    })
}
