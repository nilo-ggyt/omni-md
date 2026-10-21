//! The original coordinator issues font-trial authority for one actual complete
//! grapheme without constructing another grapheme, script, or bidi index.

use std::sync::Weak;

use super::*;

/// Preparation of one actual shaping segment within a complete editing
/// grapheme; Ready does not mean the whole grapheme has selected a font.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineFontProbeSegmentAnswer {
    Pending,
    Ready,
    Complete,
}

/// Report this step's original directory work only. Text and glyph work in the
/// font worker remains attributable to that same actual trial.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLineFontProbeSegmentStep {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
    pub answer: MarkdownLineFontProbeSegmentAnswer,
}

/// Retain one actual resolved-level segment at a time, without a full-grapheme
/// segment directory or a text copy proportional to its length.
#[derive(Clone)]
struct ProbeSegment {
    utf16: Range<usize>,
    bytes: Range<usize>,
    level: u8,
    // Only mixed-level graphemes need additional context-barrier discovery;
    // ordinary single-segment input retains its existing direct path.
    context: Option<Range<usize>>,
}

/// Stream original bidi UTF-16 records and keep the next segment's first record
/// in one fixed slot rather than rereading it from storage.
#[derive(Clone)]
struct SegmentScan {
    start: usize,
    byte: usize,
    next: usize,
    level: Option<u8>,
    held: Option<crate::markdown_bidi::MarkdownBidiShapingUnit>,
    context_phase: usize,
    context_cursor: usize,
    context: Range<usize>,
}

impl SegmentScan {
    /// Resume from exact source coordinates and an optional already-paid original record.
    fn new(
        start: usize,
        byte: usize,
        held: Option<crate::markdown_bidi::MarkdownBidiShapingUnit>,
    ) -> Self {
        Self {
            start,
            byte,
            next: start,
            level: None,
            held,
            context_phase: 0,
            context_cursor: start,
            context: byte..byte,
        }
    }
}

/// Share the original frozen Source without copying text. A consumed, cancelled,
/// or returned request immediately invalidates this weak scope. One complete
/// grapheme can contain many scalars, so this ticket promises no fixed shaping time.
#[derive(Clone)]
pub struct MarkdownLineFontProbe {
    source: MarkdownLayoutTextSource,
    request: MarkdownLineStyleRequest,
    range: Range<usize>,
    // Both coordinates come from the same original GCB directory or retained
    // certificates, with no repeated UTF-16 lookup at the native entry point.
    bytes: Range<usize>,
    scope: Weak<()>,
    cancellation: MarkdownLineComposeCancellation,
    segment: Option<ProbeSegment>,
    scan: Option<SegmentScan>,
    // Ordinary single-segment scope already has the item's complete level proof
    // when issued; success on its first trial segment cannot infer that fact.
    single_segment: bool,
    complete: bool,
}

impl MarkdownLineFontProbe {
    /// Check original Source health before cancellation and request lifetime,
    /// without querying the active document or acquiring a text window.
    ///
    /// # Errors
    /// Reject source failure, cancellation, or an ended original style phase.
    /// A later request cannot restore this older probe's authority.
    pub fn validate(&self) -> Result<()> {
        self.source.health()?;
        if self.cancellation.is_cancelled() {
            // Cancellation revokes trial work even if the original request shell survives.
            return Err(MarkdownLineComposeError::Cancelled);
        }
        if self.scope.strong_count() == 0 {
            // Only the still-waiting original coordinator owns this scope;
            // cloned request/probe values cannot extend it.
            return Err(MarkdownLineComposeError::Poisoned);
        }
        Ok(())
    }

    /// Borrow the same immutable Source for a Core worker without recapturing
    /// a document or constructing another language/index owner.
    pub fn source(&self) -> Result<&MarkdownLayoutTextSource> {
        self.validate()?;
        Ok(&self.source)
    }

    #[must_use]
    /// Return finite geometry only; consuming text still requires validate/byte-range authority.
    pub fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// Borrow UTF-8 coordinates already certified by the original two grapheme
    /// endpoints, without another distant UTF-16-to-byte scan.
    ///
    /// # Errors
    /// Source failure, cancellation, or consumed request rejects the query;
    /// numeric endpoints themselves cannot extend trial lifetime.
    pub fn byte_range(&self) -> Result<Range<usize>> {
        self.validate()?;
        Ok(self.bytes.clone())
    }

    /// Hand off only a current segment with actual endpoints and complete
    /// context extent. Pending/Complete must not replay the prior ready segment.
    pub fn shaping_segment_ready(&self) -> Result<bool> {
        self.validate()?;
        Ok(!self.complete
            && self.segment.is_some()
            && self
                .scan
                .as_ref()
                .is_none_or(|scan| scan.context_phase == 3))
    }

    /// Reuse a font choice only for an entire single-segment grapheme proved
    /// by original analysis; mixed graphemes cannot cache the first segment's result.
    pub fn complete_single_segment(&self) -> Result<bool> {
        self.validate()?;
        Ok(self.single_segment)
    }

    /// Borrow only the original currently ready segment, never an older pending value.
    fn ready_segment(&self) -> Result<&ProbeSegment> {
        self.validate()?;
        if !self.shaping_segment_ready()? {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        self.segment
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)
    }

    /// Copy exact finite input for an already selected font choice, confined to
    /// this grapheme plus the shaper's original 20-byte context on each side.
    /// At most 72 bytes are delivered/copied (32-byte small grapheme plus context).
    /// This is not a promise of a 72-byte mapping or kernel page pin: Source
    /// still leases bounded logical pieces and manages actual system pages.
    /// Return bytes already copied on failure so callers charge callback work;
    /// cache hits/failure do not redefine that work as zero.
    pub fn copy_choice_bytes(&self, range: Range<usize>, target: &mut [u8]) -> (usize, Result<()>) {
        let mut read = 0;
        let result = (|| {
            self.validate()?;
            let end = self
                .bytes
                .end
                .saturating_add(20)
                .min(self.source.byte_len());
            if range.start > range.end
                || range.start < self.bytes.start.saturating_sub(20)
                || range.end > end
                || range.len() != target.len()
                || range.len() > 72
            {
                // Public range values cannot widen this trial's exact input
                // envelope or force a larger target buffer than the fixed choice limit.
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            self.source.visit_byte_range(range, |piece| {
                // Account actual callback-delivered bytes copied into the caller's
                // finite buffer, not virtual extent or page-directory accounting.
                target[read..read + piece.len()].copy_from_slice(piece);
                read += piece.len();
                Ok(())
            })?;
            self.validate()
        })();
        // Original Source health takes precedence over an earlier operation
        // error. Copied-byte accounting remains, without inferring resource release from success.
        (read, self.validate().and(result))
    }

    /// Transfer the trial's original certified byte/UTF-16 coordinates directly
    /// into its shaping entry without distant endpoint relocation. The actual
    /// job still consumes bounded source input through the existing context,
    /// flags, resource, and cancellation construction path.
    ///
    /// # Errors
    /// Reject invalid original source/font/options/cancellation. Revalidate the
    /// probe after construction and actually destroy a rejected candidate job.
    pub fn shaping_job(
        &self,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<MarkdownShapingJob> {
        self.validate()?;
        let segment = self.ready_segment()?;
        let result = MarkdownShapingJob::from_certified_item_input(
            self.source.clone(),
            &self.request.source,
            font,
            font_authority,
            (
                segment.utf16.clone(),
                segment.bytes.clone(),
                segment.context.clone(),
            ),
            options,
            cancellation,
        )
        .map_err(MarkdownLineComposeError::from);
        // Even failed construction cannot hide original Source failure behind
        // a font/options/cancellation error discovered during that attempt.
        self.validate()?;
        result
    }

    /// Copy/decode at most the grapheme's first four UTF-8 bytes. The first scalar
    /// guides system candidates but cannot prove complete grapheme coverage.
    /// Report actual callback bytes copied even after revocation/decoding/source
    /// failure. Bounded logical leases and system-page pins have separate extents;
    /// a four-byte scalar query does not imply a four-byte mapped page or syscall.
    /// Validate original tickets before/after access and preserve source failure priority.
    pub fn first_scalar(&self) -> (usize, Result<char>) {
        let mut read = 0;
        let result = (|| {
            self.validate()?;
            if self.bytes.is_empty() {
                // A complete nonempty grapheme must have original input bytes.
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            let mut bytes = [0_u8; 4];
            let count = self.bytes.len().min(bytes.len());
            self.source
                .visit_byte_range(self.bytes.start..self.bytes.start + count, |piece| {
                    bytes[read..read + piece.len()].copy_from_slice(piece);
                    read += piece.len();
                    Ok(())
                })?;
            // Revocation here still returns already-copied byte accounting in the outer tuple.
            self.validate()?;
            let width = match bytes[0] {
                0x00..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            if width > read {
                // A partial first scalar cannot be used as a system font-candidate character.
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            // This finite prefix may end inside the next scalar. Decode only
            // the complete first one, without classifying the unrelated partial suffix as bad text.
            let value = std::str::from_utf8(&bytes[..width])
                .ok()
                .and_then(|value| value.chars().next())
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            self.validate()?;
            Ok(value)
        })();
        (read, result)
    }

    /// A font choice serves only its issuing nonce; equal range/text or even
    /// the same Source cannot replace that original outstanding request.
    pub fn accepts_request(&self, request: &MarkdownLineStyleRequest) -> Result<bool> {
        self.validate()?;
        Ok(Arc::ptr_eq(&self.request.nonce, &request.nonce)
            && self.request.source == request.source)
    }

    /// Derive direction, script, and BOT/EOT from original bidi/paragraph facts;
    /// the caller supplies only its finite style.
    ///
    /// # Errors
    /// Reject lost original authority or exceeded Core style limits without
    /// truncating language/features or substituting defaults.
    pub fn shaping_options(
        &self,
        style: &MarkdownLineComposeStyle,
    ) -> Result<MarkdownShapingOptions> {
        self.validate()?;
        let text = self.request.text_scope()?;
        let segment = self.ready_segment()?;
        if !style.points.is_finite()
            || style.points <= 0.0
            || style.features.len() > crate::MARKDOWN_SHAPING_MAX_FEATURES
            || style
                .language
                .as_ref()
                .is_some_and(|value| value.len() > 64)
        {
            // Invalid sizes and oversized metadata cannot reach shaping or
            // allocate a second retained feature/language representation.
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        Ok(MarkdownShapingOptions {
            direction: if segment.level & 1 == 0 {
                // Preserve the actual segment's resolved bidi parity.
                MarkdownShapingDirection::LeftToRight
            } else {
                // Odd resolved level requires the original right-to-left shaping direction.
                MarkdownShapingDirection::RightToLeft
            },
            script: text.script,
            language: style.language.clone(),
            points: style.points,
            beginning_of_text: segment.utf16.start == text.paragraph.start,
            end_of_text: segment.utf16.end == text.paragraph.end,
            features: style.features.clone(),
        })
    }
}

impl MarkdownLineItemCoordinator {
    /// Continue only the current original style nonce. Another coordinator,
    /// old scope, or cancelled Source cannot restart a probe through numeric coordinates.
    fn require_font_probe(&mut self, probe: &MarkdownLineFontProbe) -> Result<()> {
        self.guard()?;
        probe.validate()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        let request = payload
            .request
            .as_ref()
            .ok_or(MarkdownLineComposeError::InvalidRequest)?;
        let scope = payload
            .font_probe_scope
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        if !matches!(payload.stage, Stage::Style)
            || !Arc::ptr_eq(&request.nonce, &probe.request.nonce)
            || request.source != probe.request.source
            || !std::sync::Weak::ptr_eq(&probe.scope, &Arc::downgrade(scope))
        {
            // Both original request and original probe-scope identity must
            // still belong to this coordinator's actual style phase.
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        Ok(())
    }

    /// Advance after the real candidate worker consumed its current segment;
    /// retain one actual level segment at a time. Bidi/script directories remain
    /// in the sole coordinator; a worker receives no non-Send index owner or duplicate index.
    pub fn step_font_probe_segment(
        &mut self,
        probe: &mut MarkdownLineFontProbe,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineFontProbeSegmentStep> {
        let result = (|| {
            self.require_font_probe(probe)?;
            if budget.metadata_units == 0 {
                // A zero metadata quantum cannot query or certify another segment.
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let payload = self.payload.as_mut().expect("原试次已经核同唯一协调器");
            let bidi = payload
                .bidi
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let mut step = MarkdownLineFontProbeSegmentStep {
                bytes: 0,
                scalars: 0,
                records: 0,
                metadata_units: 0,
                answer: MarkdownLineFontProbeSegmentAnswer::Pending,
            };
            if probe.complete {
                // Completed trials return only terminal status, never the prior segment again.
                step.answer = MarkdownLineFontProbeSegmentAnswer::Complete;
                return Ok(step);
            }
            if probe.shaping_segment_ready()? {
                // Consume the previous actually ready segment before preparing
                // its successor from an already-paid held original record.
                let prior = probe.segment.take().expect("Ready 持唯一实际段");
                if prior.utf16.end == probe.range.end {
                    // Only reaching the actual whole-grapheme endpoint completes the trial.
                    probe.complete = true;
                    probe.scan = None;
                    step.answer = MarkdownLineFontProbeSegmentAnswer::Complete;
                    return Ok(step);
                }
                let held = probe.scan.take().and_then(|scan| scan.held);
                probe.scan = Some(SegmentScan::new(prior.utf16.end, prior.bytes.end, held));
            }
            let limit = budget.metadata_units.min(METADATA_UNITS);
            while step.metadata_units < limit {
                // Every new original bidi record consumes this finite metadata
                // quantum; retained records do not trigger duplicate directory I/O.
                let scan = probe
                    .scan
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                if scan.context_phase == 0 {
                    // Find the next true resolved-level boundary within the
                    // complete grapheme before discovering its shaping context.
                    if scan.next == probe.range.end {
                        // Actual grapheme end closes the final level segment,
                        // rather than treating a spent quantum as a segment edge.
                        let level = scan
                            .level
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        probe.segment = Some(ProbeSegment {
                            utf16: scan.start..scan.next,
                            bytes: scan.byte..probe.bytes.end,
                            level,
                            context: None,
                        });
                        scan.context_phase = 1;
                        scan.context_cursor = scan.start;
                        continue;
                    }
                    let unit = if let Some(unit) = scan.held.take() {
                        // Transfer the previous step's already-charged first
                        // right record exactly once, without rereading or recharging it.
                        unit
                    } else {
                        // Only a fresh directory read consumes another metadata unit.
                        step.metadata_units += 1;
                        bidi.shaping_unit_at_utf16(&self.authority, scan.next)?
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                    };
                    if unit.utf16 != scan.next || !unit.permits(&self.authority) {
                        // Equal-looking coordinates from another source or an
                        // unexpected record cannot fill this original contiguous scan.
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    }
                    if let Some(level) = scan.level {
                        if unit.level != level {
                            // Real changed-level scalar heads partition shaping
                            // even when the complete editing grapheme spans both levels.
                            if unit.continuation {
                                // A low surrogate cannot independently introduce a level seam.
                                return Err(MarkdownLineComposeError::InconsistentCoverage);
                            }
                            probe.segment = Some(ProbeSegment {
                                utf16: scan.start..unit.utf16,
                                bytes: scan.byte..unit.byte,
                                level,
                                context: None,
                            });
                            scan.held = Some(unit);
                            scan.context_phase = 1;
                            scan.context_cursor = scan.start;
                            continue;
                        }
                    } else {
                        // The first actual scalar head determines this segment's
                        // level and must match its original byte starting coordinate.
                        if unit.continuation || unit.byte != scan.byte {
                            return Err(MarkdownLineComposeError::InconsistentCoverage);
                        }
                        scan.level = Some(unit.level);
                    }
                    scan.next += 1;
                    continue;
                }
                let segment = probe.segment.as_mut().expect("实际段已由原 level 记录封闭");
                let paragraph = &probe.request.text_scope()?.paragraph;
                if scan.context_phase == 1 {
                    // Search backward for actual level barriers only inside
                    // the shaper's existing 20-byte context envelope. Its start
                    // may split UTF-8; the original context decoder discards that partial scalar.
                    let floor = segment.bytes.start.saturating_sub(20);
                    if scan.context_cursor == paragraph.start || scan.context.start <= floor {
                        // Actual paragraph start or the fixed context floor
                        // ends backward lookahead before forward context begins.
                        scan.context_phase = 2;
                        scan.context_cursor = segment.utf16.end;
                        scan.context.end = segment.bytes.end;
                        continue;
                    }
                    step.metadata_units += 1;
                    let unit = bidi
                        .shaping_unit_at_utf16(&self.authority, scan.context_cursor - 1)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                    if unit.level != segment.level {
                        // A real different-level record is the context barrier,
                        // not permission to feed its text into this segment.
                        scan.context_phase = 2;
                        scan.context_cursor = segment.utf16.end;
                        scan.context.end = segment.bytes.end;
                    } else {
                        // Extend only within the original same-level context envelope.
                        scan.context.start = unit.byte.max(floor);
                        scan.context_cursor -= 1;
                    }
                    continue;
                }
                if scan.context_phase == 2 {
                    // Discover only the finite right context after the actual
                    // segment, retaining original paragraph/level barriers.
                    let ceiling = segment
                        .bytes
                        .end
                        .saturating_add(20)
                        .min(probe.source.byte_len());
                    if scan.context.end >= ceiling || scan.context_cursor == paragraph.end {
                        // Complete only at the actual paragraph endpoint or
                        // existing finite context ceiling, after prior reads finished.
                        segment.context = Some(scan.context.clone());
                        scan.context_phase = 3;
                        step.answer = MarkdownLineFontProbeSegmentAnswer::Ready;
                        return Ok(step);
                    }
                    step.metadata_units += 1;
                    let unit = bidi
                        .shaping_unit_at_utf16(&self.authority, scan.context_cursor)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                    if unit.level != segment.level || unit.byte >= ceiling {
                        // This real next-level/extent barrier ends context without
                        // borrowing content from an unrelated shaping segment.
                        scan.context.end = unit.byte.min(ceiling);
                        segment.context = Some(scan.context.clone());
                        scan.context_phase = 3;
                        step.answer = MarkdownLineFontProbeSegmentAnswer::Ready;
                        return Ok(step);
                    }
                    // This original record supplies the complete scalar tail.
                    // Never obtain proof from an unprepared later paragraph;
                    // the original decoder handles a 20-byte cap splitting the final scalar.
                    scan.context.end = unit.byte_end.min(ceiling);
                    scan.context_cursor += 1;
                    continue;
                }
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            probe.validate()?;
            Ok(step)
        })();
        self.finish(result)
    }

    /// After a candidate misses any segment, the same sole owner may try the
    /// next actual font for this whole grapheme. Replace no Source, nonce, or
    /// cancellation state, and never revive failed/cancelled/consumed authority.
    pub fn restart_font_probe(&mut self, probe: &mut MarkdownLineFontProbe) -> Result<()> {
        let result = (|| {
            self.require_font_probe(probe)?;
            probe.complete = false;
            if probe.single_segment {
                // Original complete single-level proof can reset its one ready segment directly.
                probe.segment = Some(ProbeSegment {
                    utf16: probe.range.clone(),
                    bytes: probe.bytes.clone(),
                    level: probe.request.text_scope()?.level,
                    context: None,
                });
                probe.scan = None;
            } else {
                // A mixed-level grapheme must replay its actual segment scan;
                // first-segment success cannot stand in for whole-grapheme font coverage.
                probe.segment = None;
                probe.scan = Some(SegmentScan::new(probe.range.start, probe.bytes.start, None));
            }
            probe.validate()
        })();
        self.finish(result)
    }

    /// Obtain the first complete grapheme from the sole current NeedStyle's
    /// original dependencies through finite directory queries, without scanning its text.
    ///
    /// These facts prove only this grapheme's script, bidi, and Source; its font
    /// choice cannot be extended to the remainder of the item.
    ///
    /// # Errors
    /// Retire the coordinator on wrong nonce/phase or discontinuous original
    /// dependencies; failure cannot be repaired by constructing another index.
    pub fn font_probe(
        &mut self,
        request: &MarkdownLineStyleRequest,
    ) -> Result<MarkdownLineFontProbe> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let original = payload
                .request
                .as_ref()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            if !matches!(payload.stage, Stage::Style)
                || !Arc::ptr_eq(&original.nonce, &request.nonce)
                || original.source != request.source
            {
                // Require the actual currently outstanding request before issuing
                // a probe, regardless of equal text or source coordinates.
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            // Structural handshakes have no Unicode/font authority. Reject them
            // before issuing any grapheme or script query.
            let text = original.text_scope()?;
            #[cfg(feature = "paged-shaping-frontier")]
            if original.continuation_font.is_some()
                && original.shaping_seams.iter().all(Option::is_none)
            {
                // Frontier/closed-item continuation issues a request only with
                // both original endpoints for the actual missing/adjacent grapheme.
                // Reuse that complete scope without synchronously rereading indexes/text.
                let scope = payload
                    .font_probe_scope
                    .as_ref()
                    .expect("原字体交接请求必须持试次scope");
                return Ok(MarkdownLineFontProbe {
                    source: payload.source.clone(),
                    request: original.clone(),
                    range: original.range.clone(),
                    bytes: original
                        .continuation_bytes
                        .clone()
                        .expect("原字体交接已持同 GCB 字节端点"),
                    scope: Arc::downgrade(scope),
                    cancellation: self.cancellation.clone(),
                    segment: Some(ProbeSegment {
                        utf16: original.range.clone(),
                        bytes: original
                            .continuation_bytes
                            .clone()
                            .expect("原交接持真实字节端点"),
                        level: text.level,
                        context: None,
                    }),
                    scan: None,
                    single_segment: true,
                    complete: false,
                });
            }
            // An original shaping seam may be inside a real grapheme. Obtain
            // the enclosing complete grapheme from the same GCB directory for
            // whole-grapheme font trials, without enlarging style-response authority.
            let start = request.range.start;
            let start_boundary = match payload.breaks.grapheme_query(
                &self.authority,
                start,
                MarkdownGraphemeQuery::IsBoundary,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 == start => value,
                MarkdownGraphemeAnswer::Known(None) if original.shaping_seams[0].is_some() => {
                    match payload.breaks.grapheme_query(
                        &self.authority,
                        start,
                        MarkdownGraphemeQuery::Previous,
                    )? {
                        MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 < start => value,
                        _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
                    }
                }
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            let end_boundary = match payload.breaks.grapheme_query(
                &self.authority,
                start_boundary.utf16,
                MarkdownGraphemeQuery::Next,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value))
                    if value.utf16 > start && value.utf16 <= text.paragraph.end =>
                {
                    value
                }
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            let end = end_boundary.utf16;
            let whole_start = start_boundary.utf16;
            let single_segment = whole_start == start && end <= request.range.end;
            // Script comes from the same original job's certified coverage;
            // neither a first scalar nor another local run can classify the whole grapheme.
            #[cfg(feature = "paged-shaping-frontier")]
            let script = match payload.scripts.stable_prefix_at(
                &self.authority,
                MarkdownScriptUnit::Utf16,
                whole_start,
            )? {
                crate::markdown_scripts::MarkdownScriptPrefixAnswer::Known(Some(prefix)) => {
                    Some(prefix.facts()?)
                }
                _ => None,
            };
            #[cfg(not(feature = "paged-shaping-frontier"))]
            let script = match payload.scripts.run_at(
                &self.authority,
                MarkdownScriptUnit::Utf16,
                whole_start,
            )? {
                MarkdownScriptAnswer::Known(value) => value,
                MarkdownScriptAnswer::Pending => None,
            };
            match script {
                Some(run)
                    if run.start_utf16 <= whole_start
                        && run.end_utf16 >= end
                        && run.script.iso15924_tag() == text.script => {}
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            }
            let scope = payload
                .font_probe_scope
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            Ok(MarkdownLineFontProbe {
                source: payload.source.clone(),
                request: original.clone(),
                range: whole_start..end,
                bytes: start_boundary.byte..end_boundary.byte,
                scope: Arc::downgrade(scope),
                cancellation: self.cancellation.clone(),
                segment: single_segment.then_some(ProbeSegment {
                    utf16: whole_start..end,
                    bytes: start_boundary.byte..end_boundary.byte,
                    level: text.level,
                    context: None,
                }),
                scan: (!single_segment)
                    .then(|| SegmentScan::new(whole_start, start_boundary.byte, None)),
                single_segment,
                complete: false,
            })
        })();
        self.finish(result)
    }
}
