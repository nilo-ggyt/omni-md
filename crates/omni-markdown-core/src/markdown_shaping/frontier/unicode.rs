//! Actual single-scalar grapheme domains for the same frontier. Source batches, metadata pages,
//! and original paragraph boundaries remain separate facts.
use super::*;
use crate::MarkdownGraphemeBoundary;

const EMPTY: MarkdownGraphemeBoundary = MarkdownGraphemeBoundary { byte: 0, utf16: 0 };
const LIMIT: usize = MARKDOWN_SHAPING_FRONTIER_UNITS;

/// Fixed coordinate metadata and one 4-KiB scratch window, without a complete body or another
/// grapheme/cluster index.
pub(super) struct UnicodeState {
    plan: Option<harfrust::SimpleGcbFrontierJob>,
    certified: bool,
    first_checked: bool,
    boundaries: [MarkdownGraphemeBoundary; LIMIT + 2],
    count: usize,
    ordinal: Option<usize>,
    locate: Option<(usize, usize)>,
    successor_ordinal: Option<usize>,
    total: Option<usize>,
    next_boundary: Option<MarkdownGraphemeBoundary>,
    bytes: [u8; 4096],
    from: usize,
    available: usize,
    byte: usize,
    utf16: usize,
    pending: [u8; 4],
    used: usize,
    needed: usize,
    scanned: bool,
    center_count: usize,
    verified: usize,
}

impl UnicodeState {
    /// Return only a completely read blocking GCB within the original natural SCX endpoint; a
    /// budget boundary cannot establish that endpoint.
    pub(super) fn terminal_grapheme(
        &self,
        original_end: usize,
    ) -> Option<(Range<usize>, Range<usize>)> {
        let end = self.next_boundary?;
        let start = self.boundaries[self.count];
        (self.certified
            && !self.scanned
            && self.count != 0
            && self.count < LIMIT
            && self.used == self.needed
            && self.used != 0
            && self.byte == end.byte
            && self.utf16 == start.utf16
            && end.utf16 <= original_end)
            .then_some((start.utf16..end.utf16, start.byte..end.byte))
    }

    /// This scalar was already accounted before suspension. Resume from the same fixed scratch
    /// without reading or counting it again.
    pub(super) fn resume_terminal_grapheme(&mut self, same_font: bool) -> usize {
        if same_font {
            let end = self
                .next_boundary
                .take()
                .expect("原末GCB仍在唯一Unicode游标内");
            self.count += 1;
            self.boundaries[self.count] = end;
            self.utf16 = end.utf16;
        } else {
            // A proven font change leaves the old font's complete remainder intact; the new font
            // consumes the blocking grapheme separately later.
            self.byte = self.boundaries[self.count].byte;
            self.next_boundary = None;
        }
        self.used = 0;
        self.needed = 0;
        self.scanned = true;
        self.center_count = self.count;
        self.boundaries[self.count].utf16
    }

    pub(super) fn new(byte: usize, utf16: usize) -> Self {
        Self {
            plan: None,
            certified: false,
            first_checked: false,
            boundaries: [EMPTY; LIMIT + 2],
            count: 0,
            ordinal: None,
            locate: None,
            successor_ordinal: None,
            total: None,
            next_boundary: None,
            bytes: [0; 4096],
            from: 0,
            available: 0,
            byte,
            utf16,
            pending: [0; 4],
            used: 0,
            needed: 0,
            scanned: false,
            center_count: 0,
            verified: 0,
        }
    }
    pub(super) fn center_byte(&self) -> usize {
        self.boundaries[self.center_count].byte
    }
    pub(super) fn support_end(&self) -> usize {
        self.boundaries[self.count].utf16
    }
    pub(super) fn require_independent_route(&mut self) {
        self.first_checked = true; // Original open-SCX authority forbids probing then switching to a contextual ASCII plan.
    }
    pub(super) fn from_ascii_scan(
        byte: usize,
        utf16: usize,
        plan: harfrust::SimpleGcbFrontierJob,
    ) -> Self {
        let mut state = Self::new(byte, utf16);
        state.first_checked = true;
        state.plan = Some(plan);
        state // Skip only the completed route probe; Source/GCB/normalizer/plan still need their actual certification.
    }
    pub(super) fn reset(&mut self, byte: usize, utf16: usize) {
        // Only an original FIFO ticket after actual child release can enter. Retain certified font
        // plan state, not previous body scratch.
        self.ordinal = self.ordinal.map(|start| start + self.center_count);
        self.boundaries[0] = MarkdownGraphemeBoundary { byte, utf16 };
        self.count = 0;
        self.from = 0;
        self.available = 0;
        self.byte = byte;
        self.utf16 = utf16;
        self.used = 0;
        self.needed = 0;
        self.next_boundary = None;
        self.successor_ordinal = None;
        self.scanned = false;
        self.center_count = 0;
        self.verified = 0;
    }

    /// A foreign-font run consumes a different number of actual GCBs; old center_count cannot infer
    /// its ending ordinal. Keep the original font plan and locate the actual return point in the
    /// same grapheme index through bounded subsequent steps.
    pub(super) fn reset_after_font_run(&mut self, byte: usize, utf16: usize) {
        self.reset(byte, utf16);
        self.ordinal = None;
        self.locate = None;
        self.total = None;
    }
    fn remaining(step: &MarkdownShapingFrontierStep, budget: usize) -> FrontierResult<usize> {
        budget
            .checked_sub(step.plan_records + step.metadata_glyphs + step.boundary_records)
            .ok_or_else(|| MarkdownShapingError::InvalidOptions.into())
    }
    fn boundary(
        breaks: &mut MarkdownLayoutBreakJob,
        source: &MarkdownLayoutTextAuthority,
        ordinal: usize,
    ) -> FrontierResult<Option<MarkdownGraphemeBoundary>> {
        let mut one = [EMPTY];
        if !breaks.read_grapheme_prefix(source, ordinal..ordinal + 1, &mut one)? {
            return Ok(None);
        }
        Ok(Some(one[0])) // The page borrow ended; retain only actual byte/UTF-16 coordinates, without index/Source ownership.
    }

    fn locate(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        source: &MarkdownLayoutTextAuthority,
        step: &mut MarkdownShapingFrontierStep,
        budget: usize,
        shaping_start: Option<MarkdownGraphemeBoundary>,
    ) -> FrontierResult<bool> {
        if self.ordinal.is_some() {
            return Ok(true);
        }
        let prefix = breaks.grapheme_prefix_facts(source)?;
        if self.total != Some(prefix.known_boundary_count) {
            self.total = Some(prefix.known_boundary_count);
            self.locate = Some((0, prefix.known_boundary_count));
        } // Search only the actually committed ordinal prefix; this count is not the complete boundary total.
        while Self::remaining(step, budget)? != 0 {
            let (low, high) = self.locate.ok_or(MarkdownShapingError::Pending)?;
            if low >= high {
                if let (Some(start), Some(next), Some(ordinal)) =
                    (shaping_start, self.next_boundary, self.successor_ordinal)
                    && start.utf16 == self.utf16
                    && start.byte == self.byte
                    && next.utf16 > start.utf16
                    && next.byte > start.byte
                {
                    // This local start is the original UBA seam, not a new source GCB. Successors
                    // still come from the original directory; only this shaper gets a GB1 fragment,
                    // while Store/LineEnd/caret retain actual global grapheme boundaries.
                    self.ordinal = Some(
                        ordinal
                            .checked_sub(1)
                            .ok_or(MarkdownShapingError::InvalidRange)?,
                    );
                    self.boundaries[0] = start;
                    self.locate = None;
                    return Ok(true);
                }
                if prefix.complete {
                    return Err(MarkdownShapingError::InvalidRange.into());
                }
                step.boundary_records += 1;
                Self::boundary(breaks, source, prefix.known_boundary_count)?;
                return Ok(false); // Keep the actual unresolved GCB demand instead of deriving an ordinal from GB2 coordinates.
            }
            let middle = low + (high - low) / 2;
            step.boundary_records += 1;
            let Some(value) = Self::boundary(breaks, source, middle)? else {
                return Ok(false);
            };
            if value.utf16 < self.utf16 {
                self.locate = Some((middle + 1, high));
            } else if value.utf16 > self.utf16 {
                self.next_boundary = Some(value);
                self.successor_ordinal = Some(middle);
                self.locate = Some((low, middle));
            } else {
                if value.byte != self.byte {
                    return Err(MarkdownShapingError::InvalidRange.into());
                }
                self.ordinal = Some(middle);
                self.boundaries[0] = value;
                self.next_boundary = None;
                self.successor_ordinal = None;
                self.locate = None;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl MarkdownShapingFrontierJob {
    /// Share the ASCII branch's original owner/plan/resource scope. Public external ranges cannot
    /// authorize this coordinate-certification path.
    pub(super) fn advance_unicode(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownShapingBudget,
        metadata_budget: usize,
        mut step: MarkdownShapingFrontierStep,
    ) -> FrontierResult<MarkdownShapingFrontierStep> {
        let state = self.unicode.as_mut().ok_or(MarkdownShapingError::Pending)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        let plan = payload
            .plan
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        if !state.first_checked {
            let mut first = None;
            payload
                .source
                .visit_byte_range(self.next_byte..self.next_byte + 1, |part| {
                    first = part.first().copied();
                    Ok(())
                })
                .map_err(MarkdownShapingError::from)?;
            step.source_bytes = 1;
            state.first_checked = true;
            let first = first.ok_or(MarkdownShapingError::InvalidUtf8)?;
            if first < 0x80 {
                if (first == b' ' || first.is_ascii_punctuation())
                    && let Ok(job) = plan.simple_gcb_job(&payload.font)
                {
                    // Common punctuation/space may belong to the same item as following Han.
                    // Keep this route only with the actual plan's narrow-job capability; actual
                    // GCBs, stages, and final glyphs still need individual verification.
                    state.plan = Some(job);
                } else {
                    // The same original Source ticket already read this first byte. Hand fixed
                    // scratch to ASCII rather than rereading it; account the actual read here,
                    // then recheck Source/font before later consumption. It proves no future byte.
                    self.scan[0] = first;
                    self.scanned = 1;
                    self.unicode = None;
                    return Ok(step); // Uncertified plans and letter/digit starts retain original ASCII/body/SHY rejection routes.
                }
            }
        }
        if !state.certified {
            if state.plan.is_none() {
                match plan.simple_gcb_job(&payload.font) {
                    Ok(job) => state.plan = Some(job),
                    Err(reason) => {
                        self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                            MarkdownShapingFrontierReason::Plan(reason),
                        );
                        step.answer = self.answer;
                        return Ok(step);
                    }
                }
            }
            let job = state.plan.as_mut().ok_or(MarkdownShapingError::Pending)?;
            let before = job.checked_records();
            let result = job.step(&plan.plan(), payload.font.font(), metadata_budget);
            step.plan_records = job.checked_records() - before;
            match result {
                Ok(Some(_)) => {
                    state.certified = true;
                    state.plan = None;
                }
                Ok(None) => return Ok(step),
                Err(reason) => {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::Plan(reason),
                    );
                    step.answer = self.answer;
                    return Ok(step);
                }
            }
        }
        if !state.scanned {
            let shaping_start = (self.partial_grapheme[0] && state.utf16 == self.original.start)
                .then_some(MarkdownGraphemeBoundary {
                    byte: self.original_bytes.start,
                    utf16: self.original.start,
                });
            if !state.locate(breaks, source, &mut step, metadata_budget, shaping_start)? {
                return Ok(step);
            }
            while state.count <= LIMIT && step.scalars < budget.scalars {
                if state.byte == self.original_bytes.end && state.used == 0 {
                    if state.utf16 != self.original.end {
                        return Err(MarkdownShapingError::InvalidUtf8.into());
                    }
                    state.scanned = true;
                    break;
                }
                if state.next_boundary.is_none() {
                    if UnicodeState::remaining(&step, metadata_budget)? == 0 {
                        break;
                    }
                    let ordinal =
                        state.ordinal.ok_or(MarkdownShapingError::Pending)? + state.count + 1;
                    step.boundary_records += 1;
                    let Some(mut next) = UnicodeState::boundary(breaks, source, ordinal)? else {
                        return Ok(step);
                    };
                    if next.utf16 > self.original.end && self.partial_grapheme[1] {
                        next = MarkdownGraphemeBoundary {
                            byte: self.original_bytes.end,
                            utf16: self.original.end,
                        };
                    } // Only the original typed UBA endpoint closes a local fragment; budget/viewport edges never cut a GCB.
                    let previous = state.boundaries[state.count];
                    if next.byte <= previous.byte
                        || next.utf16 <= previous.utf16
                        || next.utf16 > self.original.end
                    {
                        return Err(MarkdownShapingError::InvalidRange.into());
                    }
                    if next.byte - previous.byte > 4 {
                        self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                            MarkdownShapingFrontierReason::NonSimpleGrapheme,
                        );
                        step.answer = self.answer;
                        return Ok(step); // A complete large GCB stays unresolved; reading four bytes cannot make it splittable.
                    }
                    state.next_boundary = Some(next);
                }
                if (state.used == 0 || state.used != state.needed) && state.from == state.available
                {
                    if state.byte == self.original_bytes.end {
                        return Err(MarkdownShapingError::InvalidUtf8.into());
                    }
                    let available = budget.bytes - step.source_bytes;
                    let count = available
                        .min(budget.scalars - step.scalars)
                        .min(4096)
                        .min(self.original_bytes.end - state.byte)
                        .min(
                            state
                                .next_boundary
                                .ok_or(MarkdownShapingError::Pending)?
                                .byte
                                - state.byte,
                        );
                    // Read only the located GCB's remaining bytes; a three-byte witness permits no
                    // prefetch of an unknown 258th scalar.
                    if count == 0 {
                        break;
                    }
                    let mut copied = 0;
                    payload
                        .source
                        .visit_byte_range(state.byte..state.byte + count, |part| {
                            state.bytes[copied..copied + part.len()].copy_from_slice(part);
                            copied += part.len();
                            Ok(())
                        })
                        .map_err(MarkdownShapingError::from)?;
                    step.source_bytes += copied;
                    if copied != count {
                        return Err(MarkdownShapingError::InvalidUtf8.into());
                    }
                    state.from = 0;
                    state.available = count;
                }
                while (state.used == 0 || state.used != state.needed)
                    && state.from < state.available
                    && state.used < 4
                {
                    let byte = state.bytes[state.from];
                    if state.used == 0 {
                        state.needed = match byte {
                            0..=0x7f => 1,
                            0xc2..=0xdf => 2,
                            0xe0..=0xef => 3,
                            0xf0..=0xf4 => 4,
                            _ => return Err(MarkdownShapingError::InvalidUtf8.into()),
                        };
                    }
                    state.pending[state.used] = byte;
                    state.used += 1;
                    state.from += 1;
                    state.byte += 1;
                    if state.used == state.needed {
                        break;
                    }
                }
                if state.used != state.needed {
                    continue;
                } // One-byte quanta retain the same UTF-8 remainder rather than treating it as EOF.
                if UnicodeState::remaining(&step, metadata_budget)? == 0 {
                    break;
                }
                let scalar = std::str::from_utf8(&state.pending[..state.used])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .ok_or(MarkdownShapingError::InvalidUtf8)?;
                // Actual decoding already occurred. Later GCB/category rejection still accounts
                // that scalar work instead of reporting zero for a negative control.
                step.scalars += 1;
                let next = state.next_boundary.ok_or(MarkdownShapingError::Pending)?;
                if state.byte != next.byte || state.utf16 + scalar.len_utf16() != next.utf16 {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::NonSimpleGrapheme,
                    );
                    step.answer = self.answer;
                    return Ok(step); // A single-input font certificate cannot split a real grapheme containing multiple scalars.
                }
                step.metadata_glyphs += 1;
                match harfrust::simple_gcb_nominal(payload.font.font(), scalar) {
                    Ok(_) => {}
                    Err(reason) => {
                        self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                            MarkdownShapingFrontierReason::Plan(reason),
                        );
                        step.answer = self.answer;
                        return Ok(step);
                    }
                } // Nominal lookup certifies the normalizer route only; actual shaping must still prove nonmissing output per glyph.
                state.utf16 = next.utf16;
                state.used = 0;
                state.needed = 0;
                state.count += 1;
                state.boundaries[state.count] = next;
                state.next_boundary = None;
                if state.count == LIMIT + 1 {
                    state.scanned = true;
                    break;
                }
            }
            if !state.scanned {
                return Ok(step);
            }
            state.center_count = if self.open_scope && state.support_end() == self.original.end {
                state.count.saturating_sub(1).min(LIMIT)
            } else {
                state.count.min(LIMIT)
            };
            if state.center_count == 0 {
                return Err(MarkdownShapingError::InvalidRange.into());
            }
            // Except at an actual closed item end, retain one certified first-unread GCB as support.
            // Tracking affects its own original grapheme, and fallback mark scanning stops at this
            // actual nonmark successor; unknown future input cannot change center flags.
            self.candidate = Some(state.boundaries[state.center_count].utf16);
        }
        let end = self.candidate.ok_or(MarkdownShapingError::Pending)?;
        if self.child.is_none() {
            let support_end = state.boundaries[state.count];
            let mut options = payload.options.clone();
            options.beginning_of_text &= self.next == self.original.start;
            options.end_of_text &= support_end.utf16 == self.original.end;
            let prepared = self
                .context_bounds
                .clone()
                .map(|bounds| {
                    input::prepare_context(
                        &payload.source,
                        self.next_byte..support_end.byte,
                        Some(bounds),
                    )
                })
                .transpose()?;
            let mut child = MarkdownShapingJob::assemble_with_context(
                payload.source.clone(),
                source,
                payload.font.clone(),
                font,
                (
                    self.next..support_end.utf16,
                    self.next_byte..support_end.byte,
                    prepared,
                ),
                options,
                self.cancellation.clone(),
            )?;
            if let Some(resources) = &self.resources {
                child.attach_composer_resources(resources)?;
            }
            step.context_bytes = child.profile()?.source_bytes_read;
            child
                .payload
                .as_mut()
                .ok_or(MarkdownShapingError::Poisoned)?
                .frontier_plan = Some(plan.plan());
            self.child = Some(child); // As with ASCII, attach original resource scope before ingestion or shaping.
        }
        let child = self.child.as_mut().ok_or(MarkdownShapingError::Poisoned)?;
        if child.status() == MarkdownShapingStatus::Ingesting {
            let bytes = budget.bytes - step.source_bytes;
            let scalars = budget.scalars - step.scalars;
            if bytes == 0 || scalars == 0 {
                return Ok(step);
            }
            let input = child.step(source, font, MarkdownShapingBudget { bytes, scalars })?;
            step.input_bytes = input.bytes;
            step.scalars += input.scalars;
        }
        if child.status() == MarkdownShapingStatus::AwaitingShape {
            child.shape(source, font)?;
            step.shapes = 1;
        }
        if child.status() == MarkdownShapingStatus::Ready {
            child.guard(source, font)?;
            let raw = child
                .payload
                .as_ref()
                .ok_or(MarkdownShapingError::Poisoned)?;
            if raw.frontier_view.is_none() {
                if raw.buffer.len() != state.count {
                    return Err(MarkdownShapingError::InvalidRange.into());
                }
                while state.verified < state.count
                    && UnicodeState::remaining(&step, metadata_budget)? != 0
                {
                    let (info, _) = raw
                        .buffer
                        .paged_glyph_record(state.verified)
                        .ok_or(MarkdownShapingError::ShaperStorage)?;
                    step.metadata_glyphs += 1;
                    if info.cluster as usize != state.boundaries[state.verified].utf16
                        || !harfrust::simple_gcb_output_is_base(payload.font.font(), info.glyph_id)
                    {
                        return Err(MarkdownShapingError::InvalidRange.into());
                    }
                    state.verified += 1;
                } // Verify actual 1:1/global clusters; a non-BMP scalar's two UTF-16 units still occupy one glyph ordinal.
                if state.verified != state.count {
                    return Ok(step);
                }
                let support = child.source_utf16.clone();
                let view = local::CertifiedCenterView::new(
                    &self.identity,
                    child,
                    self.next..end,
                    0..state.center_count,
                    support,
                )?;
                child
                    .payload
                    .as_mut()
                    .ok_or(MarkdownShapingError::Poisoned)?
                    .frontier_view = Some(view);
            }
            child.guard(source, font)?;
            self.answer = MarkdownShapingFrontierAnswer::PartReady;
        }
        step.answer = self.answer;
        Ok(step)
    }
}
