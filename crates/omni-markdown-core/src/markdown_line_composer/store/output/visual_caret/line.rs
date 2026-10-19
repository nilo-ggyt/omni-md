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
}
impl Scan {
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
        if f.kind == crate::MarkdownComposedLinePartKind::ParagraphBreak {
            self.first_control.get_or_insert(ordinal);
            return;
        }
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
            self.center = Some(run.clone());
            self.before = self.previous.clone();
            self.before2 = self.before_previous.clone();
            self.after = None;
            self.after2 = None;
        } else if self.center.is_some() {
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

/// Hold only weak output identity, the authenticated line and fixed records; suspension owns no text/font/FD.
pub struct MarkdownComposedVisualQuery {
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    head: usize,
    head_search: Option<graphemes::BoundarySearch>,
    side: Option<MarkdownComposedCaretSide>,
    right: bool,
    lookup: Option<MarkdownComposedLineLookup>,
    scan: Option<Scan>,
    selected: Option<PartRead>,
    run: Option<Run>,
    adjacent: Option<Run>,
    gcb: Option<MarkdownComposedVisualGraphemeQuery>,
    entry: bool,
    edge_only: bool,
    answer: Option<MarkdownComposedVisualAnswer>,
    failed: bool,
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
            lookup: Some(lookup),
            scan: None,
            selected: None,
            run: None,
            adjacent: None,
            gcb: None,
            entry: false,
            edge_only: false,
            answer: None,
            failed: false,
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
                    let Some((_, boundary)) = step.answer else {
                        break;
                    };
                    if boundary.is_none_or(|boundary| boundary.utf16 != query.head) {
                        return Err(Error::InvalidRequest);
                    }
                    query.head_search = None;
                    continue; // Crossing lines cannot bypass GCB authentication inside CRLF or a combining sequence.
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
                    let center = scan.center.clone();
                    if center.is_none() {
                        if scan.first_control.is_none() {
                            return Err(Error::InvalidRequest);
                        } // A foreign caret side cannot become a cross-line permission.
                        if query.entry {
                            if query.right && !query.edge_only {
                                // 软折行入口本身没有字符；若下一行只有真实换行控制，仍须在同一
                                // 动作和剩余配额内越过该控制，不能先交付一个零距离的亲和性变化。
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
                        // 右移是否已跨硬换行来自上一行；左移则由刚认证的前行控制确认。
                        // 纯软折行继续同一GCB查询，不能把显示行入口当成一次字符移动。
                        query.edge_only |= !query.right && scan.first_control.is_some();
                    } else if query.head == run.edge(query.right) {
                        let Some(next) = adjacent.take() else {
                            self.visual_cross(query)?;
                            continue;
                        };
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
                    query.gcb = Some(self.visual_grapheme_query(
                        &part,
                        query.head,
                        if query.edge_only {
                            // Authenticate the actual entry endpoint, including a control-only empty line.
                            (query.head != part.facts.source_utf16.start)
                                == (part.facts.level & 1 == 0)
                        } else {
                            query.right
                        },
                    )?);
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
                        return Err(Error::InconsistentCoverage);
                    }
                    MarkdownComposedVisualGraphemeAnswer::Found { utf16, side } => {
                        if query.edge_only {
                            return Err(Error::InconsistentCoverage);
                        }
                        if utf16 == run.edge(query.right)
                            && let Some(adjacent) = &query.adjacent
                            && usize::from(adjacent.level) + usize::from(!query.right)
                                < usize::from(run.level)
                        {
                            (
                                adjacent.edge(!query.right),
                                adjacent.side(query.right == (adjacent.level & 1 == 0)),
                            )
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
            self.require(&query.source)?;
            return result;
        }
        self.finish(result)
    }

    /// Cross only to the original adjacent ordinal; unknown prefixes stay pending and only real BOF/EOF clamp.
    fn visual_cross(&mut self, query: &mut MarkdownComposedVisualQuery) -> Result<()> {
        let scan = query.scan.as_ref().ok_or(Error::InconsistentCoverage)?;
        let ordinal = scan.line.facts.ordinal;
        let next = if query.right {
            ordinal.checked_add(1)
        } else {
            ordinal.checked_sub(1)
        };
        let terminal = if query.right {
            ordinal.checked_add(1) == Some(self.count)
                && self.range.end
                    == self
                        .payload
                        .as_ref()
                        .ok_or(Error::Poisoned)?
                        .source
                        .utf16_len()
                && scan.first_control.is_none()
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
            let (utf16, side) = if let Some(run) = run {
                (
                    run.edge(query.right),
                    run.side(query.right != (run.level & 1 == 0)),
                )
            } else {
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
        let crossed_hard = query.right && scan.first_control.is_some();
        query.lookup = Some(self.line_lookup(MarkdownComposedLineLookupTarget::Ordinal(next))?);
        query.scan = None;
        query.head_search = None;
        query.run = None;
        query.selected = None;
        query.gcb = None;
        query.adjacent = None;
        query.entry = true;
        // 只有真实ParagraphBreak才消耗本次字符动作；可见范围或soft line末端不授予此事实。
        // 左移的硬换行需等前行真实part认证完成后才能判定，不从两个range相接猜测。
        query.edge_only = crossed_hard;
        Ok(())
    }
}
