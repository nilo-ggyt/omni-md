//! One bounded visual query over original Line/Part/GCB records; every 64-byte read spends this turn budget.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownComposedVisualAnswer {
    Pending,
    /// This immutable prefix lacks the target; only its Producer may extend it, so this is not EOF.
    PrefixNotCovered,
    Found {
        utf16: usize,
        side: MarkdownComposedCaretSide,
        line: Range<usize>,
        ordinal: usize,
    },
}
pub struct MarkdownComposedVisualStep {
    pub records: usize,
    pub answer: MarkdownComposedVisualAnswer,
}

#[derive(Clone)]
struct Run {
    first: usize,
    last: usize,
    source: Range<usize>,
    level: u8,
    at: Option<usize>,
    registration: Option<MarkdownLineFontRegistration>,
}
impl Run {
    /// Resolve a visual edge from this real run direction, not from logical offset ordering.
    fn edge(&self, right: bool) -> usize {
        if right == (self.level & 1 == 0) {
            self.source.end
        } else {
            self.source.start
        }
    }
    /// Return only the logical side and actual level; no font or output lifetime is transferred.
    fn side(&self, after: bool) -> MarkdownComposedCaretSide {
        MarkdownComposedCaretSide {
            level: self.level,
            association: if after {
                MarkdownComposedCaretAssociation::After
            } else {
                MarkdownComposedCaretAssociation::Before
            },
        }
    }
}

/// Five fixed records hold part, metadata and the original item directory, without a reader or a new index.
struct PartRead {
    ordinal: usize,
    rows: [[u8; 64]; 5],
    read: usize,
}
impl PartRead {
    /// Reserve only the five fixed original part/item records needed to resume
    /// this ordinal under subsequent work quanta.
    fn new(ordinal: usize) -> Self {
        Self {
            ordinal,
            rows: [[0; 64]; 5],
            read: 0,
        }
    }
}
struct Scan {
    line: MarkdownComposedLine,
    part: PartRead,
    open: Option<Run>,
    previous: Option<Run>,
    before_previous: Option<Run>,
    center: Option<Run>,
    before: Option<Run>,
    after: Option<Run>,
    before2: Option<Run>,
    after2: Option<Run>,
    first_control: Option<usize>,
    folded_end: Option<usize>,
    #[cfg(feature = "paged-shaping-frontier")]
    folded_range: Option<Range<usize>>,
    #[cfg(feature = "paged-shaping-frontier")]
    paragraph_break: bool,
}
impl Scan {
    /// Keep one authenticated line and finite visual neighbors, never a second
    /// bidi directory or all of the paragraph's parts in memory.
    fn new(line: MarkdownComposedLine) -> Self {
        Self {
            line,
            part: PartRead::new(0),
            open: None,
            previous: None,
            before_previous: None,
            center: None,
            before: None,
            after: None,
            before2: None,
            after2: None,
            first_control: None,
            folded_end: None,
            #[cfg(feature = "paged-shaping-frontier")]
            folded_range: None,
            #[cfg(feature = "paged-shaping-frontier")]
            paragraph_break: false,
        }
    }
    /// Join only source-contiguous font slices at the same actual level; control parts add no visual glyph.
    fn accept(
        &mut self,
        part: &MarkdownComposedLinePart,
        head: usize,
        side: Option<MarkdownComposedCaretSide>,
        right: bool,
        entry: bool,
    ) {
        let f = part.facts();
        let ordinal = self.part.ordinal;
        if matches!(
            f.kind,
            crate::MarkdownComposedLinePartKind::ParagraphBreak
                | crate::MarkdownComposedLinePartKind::FoldedSection
        ) {
            self.first_control.get_or_insert(ordinal);
            if f.kind == crate::MarkdownComposedLinePartKind::FoldedSection {
                self.folded_end = Some(f.source_utf16.end);
            }
            #[cfg(feature = "paged-shaping-frontier")]
            {
                if f.kind == crate::MarkdownComposedLinePartKind::FoldedSection {
                    self.folded_range = Some(f.source_utf16.clone());
                } else {
                    self.paragraph_break |= !f.source_utf16.is_empty();
                }
            } // Real paragraph controls and opaque folds remain distinct; Word cannot classify hidden text as space.
            return;
        }
        // The chosen visual direction decides which actual endpoint includes
        // this head; equal logical offsets can have different bidi associations.
        let at = if right == (f.level & 1 == 0) {
            f.source_utf16.start <= head && head < f.source_utf16.end
        } else {
            f.source_utf16.start < head && head <= f.source_utf16.end
        };
        if let Some(open) = self.open.as_mut() {
            let contiguous = if open.level & 1 == 0 {
                open.source.end == f.source_utf16.start
            } else {
                open.source.start == f.source_utf16.end
            };
            if open.level == f.level && contiguous {
                // A font split at the same resolved level preserves this visual
                // run only when its original Source interval is truly adjacent.
                open.last = ordinal;
                open.source.start = open.source.start.min(f.source_utf16.start);
                open.source.end = open.source.end.max(f.source_utf16.end);
                if at {
                    open.at = Some(ordinal);
                }
                return;
            }
        }
        self.close(head, side, right, entry);
        self.open = Some(Run {
            first: ordinal,
            last: ordinal,
            source: f.source_utf16.clone(),
            level: f.level,
            at: at.then_some(ordinal),
            registration: Some(part.registration.clone()),
        });
    }
    /// Retain the selected run and two real neighbors per side; a lower-level replacement overwrites fixed facts.
    fn close(
        &mut self,
        head: usize,
        side: Option<MarkdownComposedCaretSide>,
        right: bool,
        entry: bool,
    ) {
        let Some(run) = self.open.take() else {
            return;
        };
        let covers = run.source.start <= head
            && head <= run.source.end
            && side.is_none_or(|side| {
                side.level == run.level
                    && match side.association {
                        MarkdownComposedCaretAssociation::After => head < run.source.end,
                        MarkdownComposedCaretAssociation::Before => run.source.start < head,
                    }
            });
        // An incoming line uses its first/last visual run; an ordinary caret keeps the lowest real level unless explicit.
        let choose = if entry {
            if right { self.center.is_none() } else { true }
        } else {
            covers && self.center.as_ref().is_none_or(|old| old.level > run.level)
        };
        if choose {
            // Replace all finite neighbor facts together when this actual run
            // becomes the center; old successor facts no longer describe it.
            self.center = Some(run.clone());
            self.before = self.previous.clone();
            self.before2 = self.before_previous.clone();
            self.after = None;
            self.after2 = None;
        } else if self.center.is_some() {
            // Keep at most the next two real visual runs for edge association.
            if self.after.is_none() {
                self.after = Some(run.clone());
            } else if self.after2.is_none() {
                self.after2 = Some(run.clone());
            }
        }
        self.before_previous = self.previous.take();
        self.previous = Some(run);
    }
}

/// Follow existing visual order across shaping parts while preserving the
/// original valid caret; an internal shaping seam can never become a stop.
#[derive(Clone, Copy)]
struct ShapingSeamWalk {
    utf16: usize,
    side: MarkdownComposedCaretSide,
    #[cfg(feature = "paged-shaping-frontier")]
    boundary: Option<(usize, crate::MarkdownGraphemeBoundary)>,
}

/// A neighboring run entry still needs an original GCB. An internal seam resumes
/// through that actual part until an editable boundary is established.
struct CandidateCaret {
    utf16: usize,
    side: MarkdownComposedCaretSide,
    part: usize,
    boundary: graphemes::BoundarySearch,
}

/// Retain weak output identity, original line facts, and fixed record state.
/// Suspension keeps no document text, font payload, or additional descriptor.
pub struct MarkdownComposedVisualQuery {
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    head: usize,
    head_search: Option<graphemes::BoundarySearch>,
    side: Option<MarkdownComposedCaretSide>,
    right: bool,
    /// The first actual paragraph determines document-flow direction once.
    /// Each later soft line/paragraph converts that flow using its own real base.
    pub(super) flow_forward: Option<bool>,
    lookup: Option<MarkdownComposedLineLookup>,
    scan: Option<Scan>,
    selected: Option<PartRead>,
    run: Option<Run>,
    adjacent: Option<Run>,
    pub(super) gcb: Option<MarkdownComposedVisualGraphemeQuery>,
    seam_walk: Option<ShapingSeamWalk>,
    /// Keep the valid position/side before entering a neighbor. It closes a
    /// shaping-seam walk only when the actual entry is not itself a GCB.
    run_origin: Option<ShapingSeamWalk>,
    entered_part: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    head_boundary: Option<(usize, crate::MarkdownGraphemeBoundary)>,
    candidate: Option<CandidateCaret>,
    entry: bool,
    edge_only: bool,
    answer: Option<MarkdownComposedVisualAnswer>,
    failed: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) crossed_break: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) crossed_fold: Option<Range<usize>>,
}
#[cfg(feature = "paged-shaping-frontier")]
impl MarkdownComposedVisualQuery {
    /// Word classification may finish after this directory step. Transfer the
    /// original finite weak font tickets, retaining neither fonts nor a reader.
    pub(super) fn word_registrations(&self) -> [Option<MarkdownLineFontRegistration>; 5] {
        [
            self.run.as_ref().and_then(|run| run.registration.clone()),
            self.adjacent
                .as_ref()
                .and_then(|run| run.registration.clone()),
            self.scan
                .as_ref()
                .and_then(|scan| scan.center.as_ref())
                .and_then(|run| run.registration.clone()),
            self.scan
                .as_ref()
                .and_then(|scan| scan.previous.as_ref())
                .and_then(|run| run.registration.clone()),
            self.gcb.as_ref().map(|gcb| gcb.registration.clone()),
        ]
    }
}

impl MarkdownComposedLines {
    /// Use position/soft-wrap affinity for the original line lookup; match bidi side against actual L1/L2 runs.
    ///
    /// # Errors
    /// Source failure, cancellation or an out-of-range target retain the original line lookup error.
    pub fn visual_query(
        &mut self,
        utf16: usize,
        affinity: MarkdownComposedLineAffinity,
        side: Option<MarkdownComposedCaretSide>,
        right: bool,
    ) -> Result<MarkdownComposedVisualQuery> {
        let lookup =
            self.line_lookup(MarkdownComposedLineLookupTarget::Source { utf16, affinity })?;
        Ok(MarkdownComposedVisualQuery {
            output: Arc::downgrade(&self.identity),
            source: self.authority.clone(),
            head: utf16,
            head_search: Some(graphemes::BoundarySearch::offset(utf16)),
            side,
            right,
            flow_forward: None,
            lookup: Some(lookup),
            scan: None,
            selected: None,
            run: None,
            adjacent: None,
            gcb: None,
            seam_walk: None,
            run_origin: None,
            entered_part: false,
            #[cfg(feature = "paged-shaping-frontier")]
            head_boundary: None,
            candidate: None,
            entry: false,
            edge_only: false,
            answer: None,
            failed: false,
            #[cfg(feature = "paged-shaping-frontier")]
            crossed_break: false,
            #[cfg(feature = "paged-shaping-frontier")]
            crossed_fold: None,
        })
    }

    /// Share decoding with the public part path; each real record is separately suspendable, including item records.
    fn visual_part(
        &mut self,
        line: &MarkdownComposedLine,
        read: &mut PartRead,
        remaining: &mut usize,
    ) -> Result<Option<MarkdownComposedLinePart>> {
        while read.read < 5 && *remaining != 0 {
            let record = if read.read < 2 {
                line.header
                    .checked_add(1)
                    .and_then(|value| {
                        read.ordinal
                            .checked_mul(2)
                            .and_then(|part| value.checked_add(part))
                    })
                    .and_then(|value| value.checked_add(read.read))
                    .ok_or(Error::Overflow)?
            } else {
                usize_at(&read.rows[1], 40)?
                    .checked_add(read.read - 2)
                    .ok_or(Error::Overflow)?
            };
            read.rows[read.read] = self.row(record)?;
            read.read += 1;
            *remaining -= 1;
            if read.read == 2 {
                Self::part_header(line, &read.rows[0], &read.rows[1])?;
            } // Unauthenticated part metadata cannot authorize an item pointer read.
        }
        if read.read != 5 {
            return Ok(None);
        }
        let item = decode_directory([read.rows[2], read.rows[3], read.rows[4]])?;
        self.decode_part(line, read.rows[0], read.rows[1], item)
            .map(Some)
    }

    /// Line authentication, part/item reads and GCB links share one record budget with no hidden pread.
    ///
    /// # Errors
    /// Foreign/revoked tickets, cancellation, real IO and corrupt records revoke the query and original reader.
    /// Invalid caret/GCB positions reject only this query and leave a healthy output available for other input.
    pub fn step_visual(
        &mut self,
        query: &mut MarkdownComposedVisualQuery,
        budget: usize,
    ) -> Result<MarkdownComposedVisualStep> {
        let result = (|| {
            self.require(&query.source)?;
            if query.failed {
                return Err(Error::Poisoned);
            }
            if !Weak::ptr_eq(&query.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            if budget == 0 || budget > STEP {
                return Err(Error::InvalidRequest);
            }
            let mut remaining = budget;
            while query.answer.is_none() && remaining != 0 {
                // First authenticate the original line through its separately budgeted tail/head records.
                if let Some(lookup) = query.lookup.as_mut() {
                    let step = self.step_line_lookup(lookup, remaining)?;
                    remaining -= step.records;
                    match step.answer {
                        MarkdownComposedLineLookupAnswer::Pending => break,
                        MarkdownComposedLineLookupAnswer::PrefixNotCovered => {
                            query.answer = Some(MarkdownComposedVisualAnswer::PrefixNotCovered);
                            break;
                        }
                        MarkdownComposedLineLookupAnswer::Found(line) => {
                            let forward = *query
                                .flow_forward
                                .get_or_insert(query.right == (line.base_level == 0));
                            query.right = forward == (line.base_level == 0);
                            // Adjacent lines have their own paragraph base. Keep
                            // this action's original document flow instead of
                            // reversing adjacency each time the base changes.
                            query.scan = Some(Scan::new(line));
                            query.lookup = None;
                        }
                    }
                    continue;
                }
                if let Some(search) = query.head_search.as_mut() {
                    let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
                    let bytes = source.byte_len();
                    let units = source.utf16_len();
                    let step = search.step(self.graphemes, bytes, units, remaining, |record| {
                        self.row(record)
                    })?;
                    remaining -= step.records;
                    let Some((_ordinal, boundary)) = step.answer else {
                        break;
                    };
                    if boundary.is_none_or(|boundary| boundary.utf16 != query.head) {
                        // A scalar or CRLF-internal coordinate cannot become an
                        // editable caret merely because line lookup accepted it.
                        return Err(Error::InvalidRequest);
                    }
                    #[cfg(feature = "paged-shaping-frontier")]
                    {
                        query.head_boundary = boundary.map(|boundary| (_ordinal, boundary));
                    }
                    query.head_search = None;
                    continue; // Crossing lines cannot bypass GCB authentication inside CRLF or a combining sequence.
                }
                if let Some(candidate) = query.candidate.as_mut() {
                    let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
                    let step = candidate.boundary.step(
                        self.graphemes,
                        source.byte_len(),
                        source.utf16_len(),
                        remaining,
                        |record| self.row(record),
                    )?;
                    remaining -= step.records;
                    let Some((_, boundary)) = step.answer else {
                        break;
                    };
                    if boundary.is_some_and(|boundary| boundary.utf16 == candidate.utf16) {
                        // Publish only a candidate that the original GCB
                        // directory authenticates at this exact coordinate.
                        let scan = query.scan.as_ref().ok_or(Error::InconsistentCoverage)?;
                        query.answer = Some(MarkdownComposedVisualAnswer::Found {
                            utf16: candidate.utf16,
                            side: candidate.side,
                            line: scan.line.facts.source_utf16.clone(),
                            ordinal: scan.line.facts.ordinal,
                        });
                        query.candidate = None;
                        continue;
                    }
                    let part = candidate.part;
                    query.candidate = None;
                    self.begin_shaping_seam_walk(query)?;
                    query.selected = Some(PartRead::new(part));
                    query.gcb = None;
                    continue; // An internal scalar is no caret; rounding up would skip actual visual order.
                }
                let scan = query.scan.as_mut().ok_or(Error::InconsistentCoverage)?;
                // Stream visual parts in original L1/L2 order; only fixed neighboring run facts survive a turn.
                if query.run.is_none() && scan.part.ordinal < scan.line.facts.parts {
                    let Some(part) =
                        self.visual_part(&scan.line, &mut scan.part, &mut remaining)?
                    else {
                        break;
                    };
                    scan.accept(&part, query.head, query.side, query.right, query.entry);
                    scan.part = PartRead::new(scan.part.ordinal + 1);
                    continue;
                }
                if query.run.is_none() {
                    scan.close(query.head, query.side, query.right, query.entry);
                    #[cfg(feature = "paged-shaping-frontier")]
                    if query.entry && query.flow_forward == Some(false) {
                        query.crossed_break |= scan.paragraph_break;
                        query.crossed_fold = scan.folded_range.clone();
                    } // A preceding control-only line still contributes its actual paragraph break.
                    let center = scan.center.clone();
                    if center.is_none() {
                        if scan.first_control.is_none() {
                            return Err(Error::InvalidRequest);
                        } // A foreign caret side cannot become a cross-line permission.
                        if query.entry {
                            if query.flow_forward == Some(true) && !query.edge_only {
                                // A soft-line entry consumes no character. If
                                // the next line contains only a real break,
                                // continue across it under the same action and
                                // remaining budget rather than stop at equal offsets.
                                self.visual_cross(query)?;
                                continue;
                            }
                            // A real empty/control-only line still authenticates its GCB instead of skipping a line or inventing a scalar.
                            let ordinal = scan.first_control.ok_or(Error::InconsistentCoverage)?;
                            let range = scan.line.facts.source_utf16.clone();
                            query.run = Some(Run {
                                first: ordinal,
                                last: ordinal,
                                source: range.clone(),
                                level: scan.line.base_level,
                                at: Some(ordinal),
                                registration: None,
                            });
                            query.head = range.start;
                            query.edge_only = true;
                            query.selected = Some(PartRead::new(ordinal));
                            continue;
                        }
                        self.visual_cross(query)?;
                        continue;
                    }
                    let mut run = center.expect("已证明有真实run");
                    let mut adjacent = if query.right {
                        scan.after.clone()
                    } else {
                        scan.before.clone()
                    };
                    if query.entry {
                        query.head = run.edge(!query.right);
                        // Forward hard-break authority comes from the prior
                        // line; backward authority comes from the authenticated
                        // preceding line's control. Soft wraps keep moving to an
                        // actual GCB instead of consuming a display-line entry.
                        query.edge_only |=
                            query.flow_forward == Some(false) && scan.first_control.is_some();
                    } else if query.head == run.edge(query.right) {
                        // An outgoing edge enters only the actual neighbor run.
                        // Preserve the valid original caret for seam recovery.
                        let Some(next) = adjacent.take() else {
                            self.visual_cross(query)?;
                            continue;
                        };
                        query.run_origin = Some(ShapingSeamWalk {
                            utf16: query.head,
                            side: query
                                .side
                                .unwrap_or_else(|| run.side(query.head < run.source.end)),
                            #[cfg(feature = "paged-shaping-frontier")]
                            boundary: query.head_boundary,
                        });
                        query.entered_part = true;
                        run = next;
                        query.head = run.edge(!query.right);
                        adjacent = if query.right {
                            scan.after2.clone()
                        } else {
                            scan.before2.clone()
                        };
                    }
                    let part = if query.head == run.edge(!query.right) {
                        if query.right { run.first } else { run.last }
                    } else {
                        run.at.ok_or(Error::InvalidRequest)?
                    };
                    query.selected = Some(PartRead::new(part));
                    query.run = Some(run);
                    query.adjacent = adjacent;
                    continue;
                }
                // Reauthenticate only the selected part under the remaining budget before consulting its GCB links.
                if query.gcb.is_none() {
                    let read = query.selected.as_mut().ok_or(Error::InconsistentCoverage)?;
                    let Some(part) = self.visual_part(&scan.line, read, &mut remaining)? else {
                        break;
                    };
                    let position = if query.seam_walk.is_some() {
                        // The original GCB spans shaping parts. Start at the
                        // neighbor's actual visual edge to locate its next real GCB.
                        if query.right == (part.facts.level & 1 == 0) {
                            part.facts.source_utf16.start
                        } else {
                            part.facts.source_utf16.end
                        }
                    } else {
                        query.head
                    };
                    let mut gcb = self.visual_grapheme_query(
                        &part,
                        position,
                        if query.edge_only {
                            // Authenticate the actual entry endpoint, including a control-only empty line.
                            (query.head != part.facts.source_utf16.start)
                                == (part.facts.level & 1 == 0)
                        } else {
                            query.right
                        },
                    )?;
                    if query.seam_walk.is_some() || query.entry || query.entered_part {
                        // Internal part-entry handling differs from an external
                        // caret: only the established seam walk can be inclusive.
                        gcb.start = GraphemeQueryStart::PartEntry {
                            inclusive: query.seam_walk.is_some(),
                        };
                    }
                    #[cfg(feature = "paged-shaping-frontier")]
                    if let Some(walk) = query.seam_walk.as_ref().or(query.run_origin.as_ref()) {
                        gcb.origin = walk.boundary;
                    }
                    query.gcb = Some(gcb);
                    continue;
                }
                let step = self.step_visual_grapheme(
                    query.gcb.as_mut().expect("原GCB续体已装配"),
                    remaining,
                )?;
                remaining -= step.records;
                let run = query.run.as_ref().expect("原真实run已选定");
                let target = match step.answer {
                    MarkdownComposedVisualGraphemeAnswer::Pending => break,
                    MarkdownComposedVisualGraphemeAnswer::Edge if query.edge_only => (
                        query.head,
                        run.side(run.registration.is_none() || query.right == (run.level & 1 == 0)),
                    ),
                    MarkdownComposedVisualGraphemeAnswer::Edge => {
                        let part_count = scan.line.facts.parts;
                        self.begin_shaping_seam_walk(query)?;
                        let ordinal = query
                            .selected
                            .as_ref()
                            .ok_or(Error::InconsistentCoverage)?
                            .ordinal;
                        let next = if query.right {
                            ordinal.checked_add(1)
                        } else {
                            ordinal.checked_sub(1)
                        };
                        if let Some(next) = next.filter(|next| *next < part_count) {
                            // Consume the same remaining record budget for the
                            // actual adjacent part, without text rescan or a
                            // copied visual-order directory.
                            query.selected = Some(PartRead::new(next));
                            query.gcb = None;
                            continue;
                        }
                        self.visual_cross(query)?;
                        continue;
                    }
                    MarkdownComposedVisualGraphemeAnswer::Found { utf16, side } => {
                        if query.edge_only {
                            // A control endpoint query cannot also claim that
                            // it advanced through a visible grapheme.
                            return Err(Error::InconsistentCoverage);
                        }
                        if query.seam_walk.is_none()
                            && utf16 == run.edge(query.right)
                            && let Some(adjacent) = &query.adjacent
                            && usize::from(adjacent.level) + usize::from(!query.right)
                                < usize::from(run.level)
                        {
                            let target = adjacent.edge(!query.right);
                            query.candidate = Some(CandidateCaret {
                                utf16: target,
                                side: adjacent.side(query.right == (adjacent.level & 1 == 0)),
                                part: if query.right {
                                    adjacent.first
                                } else {
                                    adjacent.last
                                },
                                boundary: graphemes::BoundarySearch::offset(target),
                            });
                            continue; // Extra GCB reads apply only to an actual side adjustment at a bidi edge.
                        } else {
                            (utf16, side)
                        }
                    }
                };
                query.answer = Some(MarkdownComposedVisualAnswer::Found {
                    utf16: target.0,
                    side: target.1,
                    line: scan.line.facts.source_utf16.clone(),
                    ordinal: scan.line.facts.ordinal,
                });
            }
            self.require(&query.source)?;
            if let Some(scan) = &query.scan {
                // Neighbor runs retain weak font registrations; resuming or repeating a result cannot revive revoked fonts.
                for run in [
                    query.run.as_ref(),
                    query.adjacent.as_ref(),
                    scan.center.as_ref(),
                    scan.previous.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    if let Some(registration) = &run.registration {
                        self.payload
                            .as_ref()
                            .ok_or(Error::Poisoned)?
                            .fonts
                            .require_registration(registration)?;
                    }
                }
            }
            if let Some(gcb) = query.gcb.as_mut() {
                // A completed GCB query reads nothing further but still authenticates its Source/font registration.
                if query.answer.is_some() {
                    self.step_visual_grapheme(gcb, 1)?;
                }
            }
            Ok(MarkdownComposedVisualStep {
                records: budget - remaining,
                answer: query
                    .answer
                    .clone()
                    .unwrap_or(MarkdownComposedVisualAnswer::Pending),
            })
        })();
        if result.is_err() {
            query.failed = true;
        }
        if matches!(result, Err(Error::InvalidRequest)) {
            // Reject the original invalid caret query without revoking healthy
            // output needed by another legitimate input action.
            self.require(&query.source)?;
            return result;
        }
        self.finish(result)
    }

    /// Take over only at an actual original part edge, continuing the same
    /// output through suspension without another bidi or GCB directory.
    fn begin_shaping_seam_walk(&self, query: &mut MarkdownComposedVisualQuery) -> Result<()> {
        if query.seam_walk.is_none() {
            if let Some(origin) = query.run_origin.take() {
                // The actual neighbor entry already captured a valid starting
                // caret; preserve it rather than blessing the internal seam.
                query.seam_walk = Some(origin);
                return Ok(());
            }
            let run = query.run.as_ref().ok_or(Error::InconsistentCoverage)?;
            query.seam_walk = Some(ShapingSeamWalk {
                utf16: query.head,
                side: query
                    .side
                    .unwrap_or_else(|| run.side(query.head < run.source.end)),
                #[cfg(feature = "paged-shaping-frontier")]
                boundary: query.gcb.as_ref().and_then(|gcb| gcb.origin),
            });
        }
        Ok(())
    }

    /// Cross adjacent line ordinals in this action's original document flow.
    /// Uncovered prefix remains pending; only actual BOF/EOF supplies a stop.
    fn visual_cross(&mut self, query: &mut MarkdownComposedVisualQuery) -> Result<()> {
        let scan = query.scan.as_ref().ok_or(Error::InconsistentCoverage)?;
        let forward = query.flow_forward.ok_or(Error::InconsistentCoverage)?;
        #[cfg(feature = "paged-shaping-frontier")]
        if forward {
            query.crossed_break |= scan.paragraph_break;
            query.crossed_fold = scan.folded_range.clone();
        } // Trace comes from actually crossed line controls; soft wrapping grants no such fact.
        let ordinal = scan.line.facts.ordinal;
        let next = if forward {
            ordinal.checked_add(1)
        } else {
            ordinal.checked_sub(1)
        };
        let terminal = if forward {
            ordinal.checked_add(1) == Some(self.count)
                && self.range.end
                    == self
                        .payload
                        .as_ref()
                        .ok_or(Error::Poisoned)?
                        .source
                        .utf16_len()
                && (scan.first_control.is_none() || scan.folded_end == Some(self.range.end))
                || scan.line.facts.source_utf16.is_empty()
        } else {
            ordinal == 0
        };
        if terminal {
            let run = if query.right {
                scan.previous.as_ref()
            } else {
                scan.center.as_ref().or(scan.previous.as_ref())
            };
            let (utf16, side) = if let Some(walk) = &query.seam_walk {
                // No further GCB exists in this visual direction. Retain the
                // original valid caret/side, never the last part's scalar seam.
                (walk.utf16, walk.side)
            } else if forward && scan.folded_end == Some(self.range.end) {
                // A fold through actual unterminated EOF crosses only its
                // original hidden range; no nonexistent successor line is queried.
                (
                    self.range.end,
                    MarkdownComposedCaretSide {
                        level: scan.line.base_level,
                        association: MarkdownComposedCaretAssociation::Before,
                    },
                )
            } else if let Some(run) = run {
                // A real terminal run supplies its own direction and caret side.
                (
                    run.edge(query.right),
                    run.side(query.right != (run.level & 1 == 0)),
                )
            } else {
                // The authenticated empty terminal line retains its actual base
                // and Source start; no synthetic character is added.
                (
                    scan.line.facts.source_utf16.start,
                    MarkdownComposedCaretSide {
                        level: scan.line.base_level,
                        association: MarkdownComposedCaretAssociation::After,
                    },
                )
            };
            query.answer = Some(MarkdownComposedVisualAnswer::Found {
                utf16,
                side,
                line: scan.line.facts.source_utf16.clone(),
                ordinal,
            });
            return Ok(());
        }
        let next = next.ok_or(Error::InconsistentCoverage)?;
        let crossed_hard = forward && scan.first_control.is_some();
        query.lookup = Some(self.line_lookup(MarkdownComposedLineLookupTarget::Ordinal(next))?);
        query.scan = None;
        query.head_search = None;
        query.run = None;
        query.selected = None;
        query.gcb = None;
        query.seam_walk = None;
        query.run_origin = None;
        query.entered_part = false;
        #[cfg(feature = "paged-shaping-frontier")]
        {
            query.head_boundary = None;
        }
        query.candidate = None;
        query.adjacent = None;
        query.entry = true;
        // Only a real ParagraphBreak consumes this character action. Viewport
        // and soft-line ends cannot grant that fact. Backward hard-break
        // traversal waits for actual preceding-part authentication instead of
        // guessing from screen direction or numerically adjoining ranges.
        query.edge_only = crossed_hard;
        Ok(())
    }
}
