//! Real font runs after a naturally closed item or a rejected initial plan. Adjacent
//! graphemes with the same payload are reshaped together, never sliced from old glyphs.

use super::*;

/// Accepts only an original closed item or a rejected initial/cross-input plan.
/// Retains at most the current run and one successor, independently of document size.
pub(in crate::markdown_line_composer::item) struct FontRuns {
    source: MarkdownLayoutTextAuthority,
    scope: Arc<()>,
    cancellation: MarkdownShapingCancellation,
    original: MarkdownLayoutFont,
    registration: Option<MarkdownLineFontRegistration>,
    style: MarkdownLineComposeStyle,
    facts: MarkdownLineItemFacts,
    phase: Phase,
    probe: Option<Probe>,
    run: Option<Run>,
    emit: Option<Run>,
    endpoints: [Option<CertifiedShapingEndpoint>; 2],
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    awaiting: Option<(Range<usize>, MarkdownLayoutFontAuthority)>,
    child: Option<std::sync::Weak<()>>,
    // After an initial-plan rejection, only a real font change can close this head.
    // Neither an open Script_Extensions edge nor an exhausted budget is a right edge.
    frontier_head: bool,
    audit: Option<FontAudit>,
}

/// Audit only an original natural item or a sealed safe part. A safe part's end does
/// not establish a natural boundary for a newly selected font.
enum FontAuditOrigin {
    Natural,
    Frontier,
}
struct FontAudit {
    // The palette remains the registration's sole strong owner. An in-flight audit
    // neither prolongs the native font nor creates another font catalog.
    preferred: MarkdownLineFontRegistration,
    origin: FontAuditOrigin,
    range: Range<usize>,
}
impl FontAudit {
    fn new(
        preferred: Option<MarkdownLineFontRegistration>,
        selected: &MarkdownLayoutFont,
        origin: FontAuditOrigin,
        range: Range<usize>,
    ) -> Result<Option<Self>> {
        let Some(preferred) = preferred else {
            return Ok(None);
        };
        let primary = preferred.lease()?;
        if selected.permits(&primary.font().authority()) {
            // The original base was actually selected; the window needs no repeated
            // audit of that same choice. Otherwise retain only its weak registration.
            return Ok(None);
        }
        Ok(Some(Self {
            preferred,
            origin,
            range,
        }))
    }
}

/// Glyph inspection, grapheme trials, and run delivery for one original item.
/// Scheduling and Source reacquisition remain outside this state machine.
enum Phase {
    Scan {
        ordinal: usize,
        count: usize,
        missing: Option<usize>,
    },
    Boundary(usize),
    Previous(usize),
    Probe,
    Emit,
    AwaitChild,
    ReuseOriginal,
    UncertifiedSupport,
}

/// The next original grapheme and two endpoint proofs establish this range. A font
/// request cannot be issued while either end remains uncertified.
struct Probe {
    range: Range<usize>,
    endpoints: [Option<CertifiedShapingEndpoint>; 2],
}

/// Advances only through real grapheme responses. A payload change or natural end
/// closes the run; font names never reconstruct payload identity.
struct Run {
    range: Range<usize>,
    font: MarkdownLayoutFont,
    registration: Option<MarkdownLineFontRegistration>,
}

impl FontRuns {
    /// Keep the original font owner until its normal post-child step; a selection
    /// change need not reconstruct it. The retained authority must cover the whole
    /// natural item and bounded successor, not merely the last published soft line.
    /// All ranges came from accepted responses; this checks them without new
    /// grapheme queries, text reads, or replacement font proofs.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(in crate::markdown_line_composer::item) fn retains_selection_styles(
        &self,
        source: &MarkdownLayoutTextAuthority,
        proven_end: usize,
    ) -> Result<bool> {
        if &self.source != source {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if self.cancellation.requested() {
            return Err(MarkdownLineComposeError::Cancelled);
        }
        // Reuse requires actual child retirement and every retained range to remain
        // inside the newly proven selection coverage; otherwise the caller restarts.
        let inside = |range: &Range<usize>| range.start <= range.end && range.end <= proven_end;
        if !matches!(self.phase, Phase::AwaitChild)
            || self
                .child
                .as_ref()
                .is_none_or(|child| child.strong_count() != 0)
            || !inside(&self.facts.source_utf16)
            || self.run.as_ref().is_some_and(|run| !inside(&run.range))
            || self.emit.as_ref().is_some_and(|run| !inside(&run.range))
            || self
                .probe
                .as_ref()
                .is_some_and(|probe| !inside(&probe.range))
            || self
                .audit
                .as_ref()
                .is_some_and(|audit| !inside(&audit.range))
            || self
                .awaiting
                .as_ref()
                .is_none_or(|(range, _)| !inside(range))
        {
            return Ok(false);
        }
        // Lease still-live original registrations to compare actual payloads. A
        // same-named replacement cannot renew an expired registration generation.
        if let Some(registration) = &self.registration
            && !self.original.permits(&registration.authority()?)
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        for run in [&self.run, &self.emit].into_iter().flatten() {
            if let Some(registration) = &run.registration
                && !run.font.permits(&registration.authority()?)
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
        }
        if let Some(audit) = &self.audit {
            audit.preferred.lease()?;
        }
        Ok(true)
    }

    /// Begin with the existing complete shaped item. Its glyphs are inspected only
    /// after Ready; a preferred-font audit may instead examine the same natural range.
    pub(in crate::markdown_line_composer::item) fn new(
        closed: &ClosedItem,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        preferred: Option<MarkdownLineFontRegistration>,
    ) -> Result<Self> {
        let audit = FontAudit::new(
            preferred,
            &original,
            FontAuditOrigin::Natural,
            facts.source_utf16.clone(),
        )?;
        Ok(Self {
            source: closed.source.clone(),
            scope: Arc::new(()),
            cancellation: closed.cancellation.clone(),
            original,
            registration,
            style,
            facts,
            phase: Phase::Scan {
                ordinal: 0,
                count: 0,
                missing: None,
            },
            probe: None,
            run: None,
            emit: None,
            endpoints: [None, None],
            shaping_seams: std::array::from_fn(|index| closed.endpoints[index].seam().cloned()),
            awaiting: None,
            child: None,
            frontier_head: false,
            audit,
        })
    }

    /// Derive audit coverage from the actual sealed part, never caller coordinates.
    /// Its safe end cannot be reused as a natural end for a newly selected font.
    pub(in crate::markdown_line_composer::item) fn for_frontier_part(
        part: &crate::markdown_shaping::frontier::MarkdownShapingFrontierPart,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        mut facts: MarkdownLineItemFacts,
        preferred: Option<MarkdownLineFontRegistration>,
    ) -> Result<Option<Self>> {
        let Some(audit) = FontAudit::new(
            preferred,
            &original,
            FontAuditOrigin::Frontier,
            part.shaping_range(),
        )?
        else {
            return Ok(None);
        };
        facts.source_utf16 = part.certified_range();
        // Audit the completed safe window, not the whole Script_Extensions/style
        // run. Even a multi-megabyte same-font body retains only this fixed part;
        // the parent item still owns its Job and cumulative Source/metadata budgets.
        let start = audit.range.start;
        Ok(Some(Self {
            source: part.source_authority().clone(),
            scope: Arc::new(()),
            cancellation: part.cancellation(),
            original,
            registration,
            style,
            facts,
            phase: Phase::Probe,
            probe: Some(Probe {
                range: start..start,
                endpoints: [None, None],
            }),
            run: None,
            emit: None,
            endpoints: [None, None],
            shaping_seams: part.shaping_seams().clone(),
            awaiting: None,
            child: None,
            frontier_head: false,
            audit: Some(audit),
        }))
    }

    /// Enter only after the original initial/cross-input plan is rejected. Share its
    /// grapheme and font authorities without constructing full input or glyph arrays.
    pub(in crate::markdown_line_composer::item) fn frontier_head(
        source: MarkdownLayoutTextAuthority,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        cancellation: MarkdownShapingCancellation,
        shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    ) -> Self {
        let start = facts.source_utf16.start;
        Self {
            source,
            scope: Arc::new(()),
            cancellation,
            original,
            registration,
            style,
            facts,
            phase: Phase::Probe,
            probe: Some(Probe {
                range: start..start,
                endpoints: [None, None],
            }),
            run: None,
            emit: None,
            endpoints: [None, None],
            shaping_seams,
            awaiting: None,
            child: None,
            frontier_head: true,
            audit: None,
        }
    }

    /// Enter only with adjacent semantic proofs and a complete right grapheme using
    /// a different real font. The head trial ruled out an earlier font edge in the
    /// short left item; its child must retire before requesting the unconsumed right.
    #[cfg(feature = "s2-table-window-prototype")]
    pub(in crate::markdown_line_composer::item) fn style_boundary(
        source: MarkdownLayoutTextAuthority,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        cancellation: MarkdownShapingCancellation,
        shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    ) -> Self {
        let emit = Run {
            range: facts.source_utf16.clone(),
            font: original.clone(),
            registration: registration.clone(),
        };
        let mut fonts = Self::frontier_head(
            source,
            original,
            registration,
            style,
            facts,
            cancellation,
            shaping_seams,
        );
        fonts.probe = None;
        fonts.emit = Some(emit);
        fonts.phase = Phase::Emit;
        fonts
    }

    /// Read the glyph count only from the original Ready Job. Run children do not
    /// recursively restart the same font-selection audit.
    fn inspect(&mut self, item: &mut MarkdownLineItem) -> Result<bool> {
        if let Phase::Scan { count, .. } = &mut self.phase {
            *count = item.job.profile()?.glyph_count;
            if let Some(audit) = &self.audit {
                let start = audit.range.start;
                self.probe = Some(Probe {
                    range: start..start,
                    endpoints: [None, None],
                });
                self.phase = Phase::Probe;
            } // Retain the one Ready Job; auditing a preference does not reshape it.
            return Ok(true);
        }
        Ok(false)
    }

    /// Emit at the natural end, otherwise probe only the adjacent complete grapheme.
    /// Exhausting a work budget cannot manufacture a font boundary.
    fn following(&mut self) {
        let run = self.run.as_ref().expect("原闭项仍持当前真实Font段");
        let end = self
            .audit
            .as_ref()
            .map_or(self.facts.source_utf16.end, |audit| audit.range.end);
        if run.range.end == end && !self.frontier_head {
            if self.audit.is_some() {
                self.phase = Phase::ReuseOriginal;
                return; // Identical choices preserve the original glyphs/continuation.
            }
            self.emit = self.run.take();
            self.phase = Phase::Emit;
        } else {
            self.probe = Some(Probe {
                range: run.range.end..run.range.end,
                endpoints: [None, None],
            });
            self.phase = Phase::Probe;
        }
    }

    /// Return only original UBA seam proofs at their exact coordinates. A middle
    /// font/grapheme coordinate cannot derive another seam from a containing range.
    fn seam_at(&self, offset: usize) -> Result<Option<ResolvedBidiShapingSeam>> {
        seam_at(&self.shaping_seams, &self.source, offset)
    }

    fn seams_for(&self, range: &Range<usize>) -> Result<[Option<ResolvedBidiShapingSeam>; 2]> {
        Ok([self.seam_at(range.start)?, self.seam_at(range.end)?])
    }
}

fn seam_at(
    seams: &[Option<ResolvedBidiShapingSeam>; 2],
    source: &MarkdownLayoutTextAuthority,
    offset: usize,
) -> Result<Option<ResolvedBidiShapingSeam>> {
    // Resolve the existing seam against the same Source before comparing its offset;
    // absent proof is ordinary absence, while stale authority remains an error.
    for seam in seams.iter().flatten() {
        if seam.boundary(source)?.1 == offset {
            return Ok(Some(seam.clone()));
        }
    }
    Ok(None)
}

pub(in crate::markdown_line_composer::item) fn inspect_shaped(
    payload: &mut Payload,
) -> Result<bool> {
    match (&mut payload.closed_fonts, &mut payload.item) {
        (Some(fonts), Some(item)) => fonts.inspect(item),
        _ => Ok(false), // Unrelated complete Jobs/frontier children keep their contract.
    }
}

/// Record a child only when the coordinator's Ready item is actually consumed. A
/// cursor value cannot prove that the paged glyph allocation has been released.
pub(in crate::markdown_line_composer::item) fn lend_font_item(
    payload: &mut Payload,
    item: &MarkdownLineItem,
) -> Result<()> {
    let Some(fonts) = payload.closed_fonts.as_mut() else {
        return Ok(());
    };
    let (range, font) = fonts
        .awaiting
        .as_ref()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    // Link the actual Job identity only after its complete range and font match the
    // pending transfer; later continuation waits for this weak identity to expire.
    if !matches!(fonts.phase, Phase::AwaitChild)
        || item.facts.source_utf16 != *range
        || !item.font.same_payload(font)
    {
        return Err(MarkdownLineComposeError::SourceMismatch);
    }
    fonts.child = Some(item.job.index_identity());
    Ok(())
}

/// Each response must retain the original style and complete grapheme. Only the
/// selected real font payload may change during this trial.
pub(in crate::markdown_line_composer::item) fn provide_font_style(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    response: MarkdownLineStyleResponse,
) -> Result<()> {
    let fonts = payload.closed_fonts.as_mut().expect("原闭项持有字体试次");
    let probe = fonts.probe.as_ref().expect("原NeedStyle来自一个完整GCB");
    // Font fallback cannot simultaneously replace Script, UBA, paragraph, or style
    // authority. Validate the response before consuming its pending grapheme proof.
    if fonts.source != *authority
        || response.coverage != probe.range
        || response.style.presentation != fonts.style.presentation
        || response.style.points.to_bits() != fonts.style.points.to_bits()
        || response.style.language != fonts.style.language
        || response.style.features != fonts.style.features
    {
        return Err(MarkdownLineComposeError::InvalidRequest);
    }
    if let Some(registration) = &response.registration {
        // A supplied registration must still lease this exact selected payload.
        let actual = registration.lease()?;
        if !response.font.permits(&actual.font().authority()) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
    }
    if fonts.audit.is_some() {
        // Preference auditing differs from missing-glyph repair: selecting the same
        // first font is valid and cannot skip a later return to the preferred base.
        return provide_audit_style(payload, response);
    }
    let first = fonts.run.is_none();
    if first && !fonts.frontier_head && response.font.permits(&fonts.original.authority()) {
        // Selecting the same payload did not repair the actual glyph zero. Keep the
        // complete result for Measure to report it; neither invent success nor retry
        // the same trial forever. The existing Core oracle still sees that result.
        payload.closed_fonts = None;
        payload.stage = Stage::Ready;
    } else {
        let range = fonts.probe.take().expect("原完整字素仍在").range;
        if first {
            // Retire the complete Job only after another real font was selected.
            // Its prefix must be reshaped, never sliced from the old glyph array.
            if !fonts.frontier_head {
                drop(payload.item.take());
            } // Only a natural closed item had an earlier complete Job to release.
            if !fonts.frontier_head && fonts.facts.source_utf16.start < range.start {
                fonts.emit = Some(Run {
                    range: fonts.facts.source_utf16.start..range.start,
                    font: fonts.original.clone(),
                    registration: fonts.registration.clone(),
                });
                fonts.phase = Phase::Emit;
            }
            fonts.run = Some(Run {
                range,
                font: response.font,
                registration: response.registration,
            });
            if fonts.emit.is_none() {
                fonts.following();
            }
        } else if response
            .font
            .permits(&fonts.run.as_ref().expect("当前Font段仍在").font.authority())
        {
            fonts.run.as_mut().expect("当前Font段仍在").range.end = range.end;
            fonts.following(); // Merge adjacent graphemes using the same real payload.
        } else {
            fonts.emit = fonts.run.take();
            if !fonts.frontier_head {
                fonts.run = Some(Run {
                    range,
                    font: response.font,
                    registration: response.registration,
                });
            } // A frontier head closes only its first run; the long suffix returns
            // to ItemStart rather than becoming an arbitrarily short styled item.
            fonts.phase = Phase::Emit; // Natural items retain one actual successor.
        }
        payload.stage = Stage::ClosedFonts;
    }
    payload.request.take();
    payload.font_probe_scope.take(); // Consuming the response expires its trial scope.
    Ok(())
}

/// Audit font policy for the original Ready window. Retain the current same-font
/// run only; the first real transition returns the unconsumed remainder.
fn provide_audit_style(payload: &mut Payload, response: MarkdownLineStyleResponse) -> Result<()> {
    let fonts = payload.closed_fonts.as_mut().expect("原字体审核owner仍活");
    let audit = fonts.audit.as_ref().expect("审核响应只进入原审核用途");
    let preferred = response
        .preferred_registration
        .as_ref()
        .ok_or(MarkdownLineComposeError::SourceMismatch)?;
    // Every trial belongs to the same original preferred registration. Identical
    // text or a same-named base cannot substitute another semantic font authority.
    if !preferred
        .authority()?
        .same_payload(&audit.preferred.authority()?)
    {
        return Err(MarkdownLineComposeError::SourceMismatch);
    }
    if fonts.run.is_none()
        && !response.font.permits(&fonts.original.authority())
        && matches!(audit.origin, FontAuditOrigin::Frontier)
    {
        // A later safe part may immediately return to the base. The previous part
        // was consumed, so there is no old-font prefix to emit here. Release the
        // unpublished Job, then retire its parent/whole-part continuation and restart
        // at the same cursor. Keep original Source/Script/Breaks, never an empty run
        // or a fabricated claim that the whole part has been consumed.
        let at_head = fonts.probe.as_ref().expect("原审核仍持GCB").range.start == payload.cursor;
        drop(payload.item.take());
        if !at_head {
            // A change inside left context is not a certified font start at the
            // current cursor. Keep it uncertified instead of publishing the old Job.
            fonts.phase = Phase::UncertifiedSupport;
            payload.request.take();
            payload.font_probe_scope.take();
            payload.stage = Stage::ClosedFonts;
            return Ok(());
        }
        drop(payload.frontier.take());
        payload.closed_fonts = None;
        payload.request.take();
        payload.font_probe_scope.take();
        payload.stage = Stage::ItemStart;
        return Ok(());
    }
    let range = fonts.probe.take().expect("原完整GCB试次仍在").range;
    match &mut fonts.run {
        None => {
            // The initiating response already selected the first grapheme's font;
            // unchanged Source/semantics cannot silently rewrite that original fact.
            if !response.font.permits(&fonts.original.authority()) {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            fonts.run = Some(Run {
                range,
                font: response.font,
                registration: response.registration,
            });
            fonts.following();
        }
        Some(run) if response.font.permits(&run.font.authority()) => {
            run.range.end = range.end;
            fonts.following();
        }
        Some(_) => {
            if fonts.run.as_ref().expect("原连续Font段仍在").range.start != payload.cursor {
                // Left support belongs to the shaping plan and cannot simply be
                // cropped away. Without a same-plan center proof, keep it explicitly
                // uncertified; neither publish known wrong-font support nor turn a
                // budget edge into a natural boundary for the new font.
                drop(payload.item.take());
                fonts.phase = Phase::UncertifiedSupport;
                payload.request.take();
                payload.font_probe_scope.take();
                payload.stage = Stage::ClosedFonts;
                return Ok(());
            }
            fonts.emit = fonts.run.take();
            fonts.frontier_head = true;
            fonts.phase = Phase::Emit;
            // The original Job includes text now known to select another font.
            // Release it before shaping the natural first run, avoiding two primary
            // glyph graphs. The suffix obtains its own plan from the real boundary
            // only after this child retires; the old part end grants it no closure.
            drop(payload.item.take());
            // The old frontier child's weak identity now only proves old Job release;
            // its continuation still ends at the whole part. Never resume with it.
            // AwaitChild waits for this natural run, then retires the original parent.
        }
    }
    payload.request.take();
    payload.font_probe_scope.take();
    payload.stage = Stage::ClosedFonts;
    Ok(())
}

/// Charge glyph records and grapheme metadata to the caller's separate quanta.
/// Inspection never copies the complete item's glyph vector.
pub(in crate::markdown_line_composer::item) fn advance_fonts(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
) -> Result<MarkdownLineItemStep> {
    let mut step = MarkdownLineItemStep {
        bytes: 0,
        context_bytes: 0,
        scalars: 0,
        records: 0,
        metadata_units: 0,
        shapes: 0,
        answer: MarkdownLineItemAnswer::Yielded,
    };
    let fonts = payload
        .closed_fonts
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    let previous = matches!(fonts.phase, Phase::Previous(_));
    match &mut fonts.phase {
        Phase::Scan {
            ordinal,
            count,
            missing,
        } => {
            let item = payload.item.as_mut().expect("原完整Job尚未发布");
            while *ordinal < *count && step.records < budget.records {
                let glyph = item
                    .glyph(*ordinal)?
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                *ordinal += 1;
                step.records += 1;
                // RTL visual order is not logical source order. Track the earliest
                // missing cluster, then ask the original UAX29 owner for its bounds.
                if glyph.glyph_id == 0 {
                    *missing = Some(missing.map_or(glyph.source_utf16_cluster, |old| {
                        old.min(glyph.source_utf16_cluster)
                    }));
                }
            }
            payload.frontier_work.metadata_glyphs = payload
                .frontier_work
                .metadata_glyphs
                .checked_add(step.records)
                .ok_or(MarkdownLineComposeError::Overflow)?;
            if *ordinal == *count {
                if let Some(offset) = *missing {
                    fonts.phase = Phase::Boundary(offset);
                } else {
                    payload.closed_fonts = None;
                    payload.stage = Stage::Ready;
                    step.answer = MarkdownLineItemAnswer::ItemReady(item.facts.clone());
                } // No missing glyph: deliver the original ligatures/context/index.
            }
        }
        Phase::Boundary(offset) | Phase::Previous(offset) => {
            let offset = *offset;
            step.metadata_units = 1;
            let answer = payload.breaks.grapheme_query(
                authority,
                offset,
                if previous {
                    MarkdownGraphemeQuery::Previous
                } else {
                    MarkdownGraphemeQuery::IsBoundary
                },
            )?;
            match answer {
                MarkdownGraphemeAnswer::Pending => {} // The original Break Job resumes later.
                MarkdownGraphemeAnswer::Known(Some(boundary))
                    if boundary.utf16 <= offset
                        && boundary.utf16 >= fonts.facts.source_utf16.start =>
                {
                    fonts.probe = Some(Probe {
                        range: boundary.utf16..boundary.utf16,
                        endpoints: [None, None],
                    });
                    fonts.phase = Phase::Probe;
                }
                MarkdownGraphemeAnswer::Known(None) if !previous => {
                    fonts.phase = Phase::Previous(offset)
                }
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            } // A HarfBuzz cluster locates the miss; only original UAX29 supplies bounds.
        }
        Phase::Probe => return prepare_probe(payload, authority, budget, step),
        Phase::Emit => return prepare_run(payload, authority, budget, step),
        Phase::AwaitChild => {
            let child = fonts
                .child
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            if child.strong_count() != 0 {
                // The consumer still owns the actual glyph allocation. Cursor
                // advancement alone cannot authorize another retained child.
                step.answer = MarkdownLineItemAnswer::NeedsFrontier(
                    MarkdownLineFrontierReason::PendingConsumer,
                );
            } else {
                fonts.child = None;
                fonts.awaiting = None;
                if fonts.run.is_some() {
                    fonts.following();
                } else {
                    if fonts.frontier_head {
                        // Only actual child release ends the rejected parent plan
                        // and its strong font ownership for this frontier head.
                        drop(payload.frontier.take());
                    } else if payload.cursor != fonts.facts.source_utf16.end {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    }
                    payload.closed_fonts = None;
                    payload.stage = Stage::ItemStart;
                } // Retire font/style ownership after the final real child expires.
            }
        }
        Phase::ReuseOriginal => {
            let item = payload
                .item
                .as_mut()
                .expect("未变化审核仍持唯一原Ready Job");
            let audit = fonts.audit.as_ref().expect("复用只属于原字体审核");
            // Revalidate the same preferred registration and actual original Job
            // before publishing it unchanged; the glyph query performs its health gate.
            audit.preferred.lease()?;
            if item.facts.source_utf16 != fonts.facts.source_utf16
                || !fonts.original.permits(&item.font)
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            let count = item.job.profile()?.glyph_count;
            item.job.glyph(authority, &item.font, count)?;
            step.answer = MarkdownLineItemAnswer::ItemReady(item.facts.clone());
            payload.closed_fonts = None;
            payload.stage = Stage::Ready;
        } // Preserve frontier identity/continuation or the natural complete Job.
        Phase::UncertifiedSupport => {
            step.answer =
                MarkdownLineItemAnswer::NeedsFrontier(MarkdownLineFrontierReason::UncertifiedWord);
        }
    }
    Ok(step)
}

/// Expose NeedStyle only after the adjacent complete grapheme and both endpoint
/// proofs are available. Pending metadata never authorizes a range.
fn prepare_probe(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let fonts = payload.closed_fonts.as_mut().expect("原闭项字体owner仍活");
    let probe = fonts.probe.as_mut().expect("真实待探字素仍在");
    if fonts.frontier_head
        && (probe.range.start == fonts.facts.source_utf16.end
            || fonts
                .run
                .as_ref()
                .is_some_and(|run| run.range.len() > MAX_CLOSED_UTF16))
    {
        // The same-font probe found no natural right edge. Do not publish a budget
        // slice or obscure the original plan's more precise rejection. The frontier
        // has lent no child; retire only this trial and resume its same Source,
        // Script_Extensions, and word-domain continuation.
        payload
            .frontier
            .as_mut()
            .expect("首 Font 探测仍归原 Frontier")
            .decline_font_head();
        let retired = payload.closed_fonts.take();
        payload.stage = Stage::Frontier;
        drop(retired); // Expire these run/trial scopes; old requests cannot restart them.
        return Ok(step);
    }
    if probe.range.is_empty() {
        step.metadata_units += 1;
        match payload.breaks.grapheme_query(
            authority,
            probe.range.start,
            MarkdownGraphemeQuery::Next,
        )? {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(end)) if end.utf16 > probe.range.start => {
                let limit = fonts
                    .audit
                    .as_ref()
                    .map_or(fonts.facts.source_utf16.end, |audit| audit.range.end);
                if end.utf16 > limit
                    && seam_at(&fonts.shaping_seams, &fonts.source, limit)?.is_none()
                {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                // Only an original UBA seam may terminate this local shaping segment
                // before the grapheme end; an arbitrary range limit cannot do so.
                probe.range.end = end.utf16.min(limit);
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    for (index, offset) in [probe.range.start, probe.range.end].into_iter().enumerate() {
        // Endpoint preparation is resumable under the same metadata quantum. Reuse
        // completed proofs; a non-grapheme endpoint still needs an actual UBA seam.
        if probe.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, offset)?;
        match answer {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 == offset => {
                probe.endpoints[index] = Some(CertifiedShapingEndpoint::Grapheme {
                    proof: endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                    seam: seam_at(&fonts.shaping_seams, &fonts.source, offset)?,
                });
            }
            MarkdownGraphemeAnswer::Known(None) => {
                probe.endpoints[index] = Some(CertifiedShapingEndpoint::Bidi(
                    seam_at(&fonts.shaping_seams, &fonts.source, offset)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                ));
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    // Preserve the original paragraph/Script/bidi facts and exact byte interval.
    // A fresh nonce and weak probe scope bind this request to this one complete trial.
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        range: probe.range.clone(),
        scope: StyleScope::Text(TextStyleScope {
            paragraph: fonts.facts.paragraph_utf16.clone(),
            script: fonts.facts.script,
            level: fonts.facts.level,
        }),
        shaping_seams: [
            seam_at(&fonts.shaping_seams, &fonts.source, probe.range.start)?,
            seam_at(&fonts.shaping_seams, &fonts.source, probe.range.end)?,
        ],
        continuation_font: Some(fonts.original.authority()),
        continuation_bytes: Some(
            probe.endpoints[0]
                .as_ref()
                .expect("原闭项 GCB 起点已认证")
                .boundary(authority)?
                .byte
                ..probe.endpoints[1]
                    .as_ref()
                    .expect("原闭项 GCB 终点已认证")
                    .boundary(authority)?
                    .byte,
        ),
    };
    payload.request = Some(request.clone());
    payload.font_probe_scope = Some(Arc::new(()));
    payload.stage = Stage::Style;
    step.answer = MarkdownLineItemAnswer::NeedStyle(request);
    Ok(step)
}

/// Shape each complete font run through the original context preparation. Its ends
/// must be closed by the natural item or an observed real font selection change.
fn prepare_run(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let fonts = payload.closed_fonts.as_mut().expect("原Font分段仍活");
    let run = fonts.emit.as_ref().expect("原已封闭Font段仍在");
    for (index, offset) in [run.range.start, run.range.end].into_iter().enumerate() {
        // Reshape from original endpoint proofs, not slices of the old glyph array.
        // Yield while the same Break Job prepares either endpoint under its budget.
        if fonts.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, offset)?;
        match answer {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 == offset => {
                fonts.endpoints[index] = Some(CertifiedShapingEndpoint::Grapheme {
                    proof: endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                    seam: fonts.seam_at(offset)?,
                });
            }
            MarkdownGraphemeAnswer::Known(None) => {
                fonts.endpoints[index] = Some(CertifiedShapingEndpoint::Bidi(
                    fonts
                        .seam_at(offset)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                ));
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    let endpoints = fonts
        .endpoints
        .each_mut()
        .map(|value| value.take().expect("原Font段两张GCB票齐备"));
    let first = endpoints[0].boundary(authority)?;
    let last = endpoints[1].boundary(authority)?;
    endpoints[0].require_pair(&endpoints[1], authority)?;
    // Ordinary closed items retain the 256 UTF-16-unit bound. A sealed frontier part
    // already proves 256 scalars/1024 bytes; non-BMP text must not double that actual
    // work allowance merely through UTF-16 units. This exception stays inside that
    // real part and covers only the old-font prefix before an observed transition.
    // It grants no natural boundary at the safe part end to the newly selected font.
    if fonts.source != *authority
        || first.utf16 != payload.cursor
        || first.utf16 != run.range.start
        || last.utf16 != run.range.end
        || first.utf16 < fonts.facts.source_utf16.start
        || last.utf16
            > fonts
                .audit
                .as_ref()
                .map_or(fonts.facts.source_utf16.end, |audit| audit.range.end)
        || first.utf16 >= last.utf16
        || (last.utf16 - first.utf16 > MAX_CLOSED_UTF16
            && !fonts
                .audit
                .as_ref()
                .is_some_and(|audit| matches!(audit.origin, FontAuditOrigin::Frontier)))
        || last.byte - first.byte > MAX_CLOSED_BYTES
    {
        return Err(MarkdownLineComposeError::InconsistentCoverage);
    } // Original owner plus endpoint proofs grant closure; coordinates alone cannot.
    let run = fonts.emit.take().expect("原真实Font段尚未移交");
    let font = run.font.authority();
    if let Some(registration) = &run.registration {
        // Check the same live registration before transferring this run's font.
        let actual = registration.lease()?;
        if !run.font.permits(&actual.font().authority()) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
    }
    let closed = ClosedItem {
        source: authority.clone(),
        font: font.clone(),
        scope: Arc::downgrade(&fonts.scope),
        endpoints,
        cancellation: fonts.cancellation.clone(),
    };
    let mut facts = fonts.facts.clone();
    facts.source_utf16 = run.range.clone();
    let shaping_seams = fonts.seams_for(&run.range)?;
    facts.beginning_of_text = run.range.start == facts.paragraph_utf16.start;
    facts.end_of_text = run.range.end == facts.paragraph_utf16.end;
    let options = MarkdownShapingOptions {
        direction: facts.direction,
        script: facts.script,
        language: fonts.style.language.clone(),
        points: facts.points,
        beginning_of_text: facts.beginning_of_text,
        end_of_text: facts.end_of_text,
        features: fonts.style.features.clone(),
    };
    // Transfer source/font resources into the original pending shaping owner. The
    // run keeps only exact awaiting facts until the coordinator lends the real child.
    let shaping = crate::markdown_shaping::PendingClosedShaping::new(
        payload.source.clone(),
        authority,
        run.font,
        &font,
        closed,
        options,
    )?;
    fonts.awaiting = Some((run.range, font.clone()));
    fonts.phase = Phase::AwaitChild;
    payload.closed_input = Some(PendingItem {
        shaping,
        font,
        registration: run.registration,
        replay_style: fonts.style.clone(),
        facts,
        shaping_seams,
    });
    payload.stage = Stage::ClosedInput;
    // Construction transfers authority only. Adjacent context and center text are
    // actually read in later, separately charged byte/scalar preparation quanta.
    Ok(step)
}

#[cfg(test)]
mod initial_tests;
