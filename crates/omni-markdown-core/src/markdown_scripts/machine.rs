//! Keeps only the current cluster/run and bracket-stack heads in memory.
//! Arbitrarily deep frames belong to the bounded paged storage owner.
use super::{
    MarkdownScript as Script, MarkdownScriptBudget as Budget, MarkdownScriptError as Error,
    MarkdownScriptRun as Run, MarkdownScriptStep as Step, Result,
    properties::{self, Bracket, Set},
    store::{self, Frame, Store},
    unicode_data,
};
use crate::markdown_graphemes::MarkdownGraphemeScalarCursor;

struct Decoded {
    properties: properties::Properties,
    start_byte: usize,
    end_byte: usize,
    start_utf16: usize,
    end_utf16: usize,
    boundary: bool,
}
#[derive(Clone, Copy)]
struct Cluster {
    start_byte: usize,
    end_byte: usize,
    start_utf16: usize,
    end_utf16: usize,
    scripts: Option<Set>,
    primary: Option<u8>,
    anchored: bool,
    paragraph: bool,
    bracket: Option<Bracket>,
}
impl Cluster {
    /// Starts one grapheme cluster at actual source coordinates, with no script
    /// guessed before its first contributing scalar establishes the candidate set.
    fn new(value: &Decoded) -> Self {
        Self {
            start_byte: value.start_byte,
            end_byte: value.start_byte,
            start_utf16: value.start_utf16,
            end_utf16: value.start_utf16,
            scripts: None,
            primary: None,
            anchored: false,
            paragraph: false,
            bracket: None,
        }
    }
    /// Keeps an indivisible grapheme cluster under its first specific base's
    /// SCX authority; marks cannot split a user-visible cluster into script runs.
    fn add(&mut self, value: Decoded) -> Result<()> {
        let p = value.properties;
        if !self.anchored {
            if !matches!(p.primary, unicode_data::COMMON | unicode_data::INHERITED) {
                self.scripts = p.scripts;
                self.primary = Some(p.primary);
                self.anchored = true; // Unknown is specific too; never reinterpret it as Latin or a wildcard.
            } else if let Some(next) = p.scripts {
                self.scripts = match self.scripts {
                    Some(current) => Some(current.intersection(next).unwrap_or(current)),
                    None => Some(next),
                }; // Without a specific base, narrow SCX while preserving the complete cluster.
            }
        } // The first specific base owns full SCX; later marks/cluster members follow it rather than split by scalar properties.
        if let Some(bracket) = p.bracket {
            if self.bracket.is_some() {
                return Err(Error::CorruptMetadata);
            }
            self.bracket = Some(bracket); // Unicode 18 paired punctuation is non-Mark/GCB Other: at most one per cluster.
        }
        self.paragraph |= p.paragraph;
        self.end_byte = value.end_byte;
        self.end_utf16 = value.end_utf16;
        Ok(())
    }
}
struct CurrentRun {
    start_byte: usize,
    start_utf16: usize,
    scripts: Option<Set>,
    primary: Option<u8>,
    // Advance only after complete clusters and their bracket actions settle,
    // never merely because bytes have been decoded or borrowed.
    #[cfg(feature = "paged-shaping-frontier")]
    settled_end: (usize, usize),
}
impl CurrentRun {
    /// Chooses only among surviving candidates: prefer a compatible preceding
    /// script, then the base's primary script, then deterministic generated order.
    fn script(&self, preceding: Option<u8>) -> Result<u8> {
        let Some(set) = self.scripts else {
            // A genuinely unconstrained run remains Common rather than borrowing
            // an incompatible preceding script or inventing Latin coverage.
            return Ok(unicode_data::COMMON);
        };
        if let Some(script) = preceding.filter(|&script| set.contains(script)) {
            return Ok(script);
        }
        if let Some(script) = self.primary.filter(|&script| set.contains(script)) {
            return Ok(script);
        }
        set.first().ok_or(Error::CorruptMetadata) // IDs follow ISO-tag order; an empty set is corruption, not Unknown.
    }
}
enum Phase {
    Locate,
    Resolve { target: usize, run: usize },
    Pop { target: usize },
    Merge,
    Open,
    Paragraph,
    Done,
}
struct Action {
    cluster: Cluster,
    phase: Phase,
}

pub(super) struct Machine {
    pub(super) byte: usize,
    pub(super) utf16: usize,
    scalar_start: usize,
    pending_utf8: [u8; 4],
    used: usize,
    needed: usize,
    cursor: MarkdownGraphemeScalarCursor,
    held: Option<Decoded>,
    cluster: Option<Cluster>,
    action: Option<Action>,
    current: Option<CurrentRun>,
    preceding: Option<u8>,
    top: usize,
    same_kind: [usize; 65],
    finish_phase: u8,
    pub(super) ready: bool,
}
impl Default for Machine {
    /// Initializes fixed decoder/cluster state and empty bracket heads only;
    /// arbitrarily deep actual frames remain the separate paged store's responsibility.
    fn default() -> Self {
        Self {
            byte: 0,
            utf16: 0,
            scalar_start: 0,
            pending_utf8: [0; 4],
            used: 0,
            needed: 0,
            cursor: MarkdownGraphemeScalarCursor::default(),
            held: None,
            cluster: None,
            action: None,
            current: None,
            preceding: None,
            top: store::NONE,
            same_kind: [store::NONE; 65],
            finish_phase: 0,
            ready: false,
        }
    }
}
impl Machine {
    /// Settles every pre-row action/cluster through the original pump. A structural
    /// jump cannot discard obligations from body text already accepted.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn settle_before_table(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
        store: &mut Store,
        step: &mut Step,
        budget: Budget,
    ) -> Result<bool> {
        if !self.pump(store, step, budget, false)? {
            return Ok(false);
        }
        if self.byte == span.start.byte && span.start.byte != 0 {
            return self.settle_hard_boundary(
                span.start.byte,
                span.start.utf16,
                store,
                step,
                budget,
            );
        }
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.cluster.start_byte < span.start.byte)
            || self
                .cluster
                .as_ref()
                .is_some_and(|cluster| cluster.start_byte < span.start.byte)
            || self
                .current
                .as_ref()
                .is_some_and(|run| run.start_byte < span.start.byte)
        {
            return Err(Error::CorruptMetadata);
        }
        Ok(true)
    }

    /// Discards only unfinished SCX state inside the table. Original paragraph
    /// processing already settled bracket context before its hard boundary.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn begin_table_tail(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
    ) {
        self.held = None;
        self.cluster = None;
        self.action = None;
        self.current = None;
        self.preceding = None;
        self.top = store::NONE;
        self.same_kind = [store::NONE; 65];
        self.byte = span.tail.byte;
        self.utf16 = span.tail.utf16;
        self.used = 0;
        self.needed = 0;
        self.pending_utf8 = [0; 4];
    }

    /// Restores only original grapheme-cursor context from actual trailing CR/LF.
    /// An unterminated EOF invents neither later context nor script values.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn finish_table(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
        tail: &[u8],
    ) -> Result<()> {
        if tail.iter().any(|byte| !matches!(*byte, b'\r' | b'\n')) {
            return Err(Error::CorruptMetadata);
        }
        self.cursor = MarkdownGraphemeScalarCursor::default();
        for byte in tail {
            self.cursor.push(char::from(*byte));
        }
        self.byte = span.end.byte;
        self.utf16 = span.end.utf16;
        self.scalar_start = span.end.byte;
        Ok(())
    }
    /// Actual header CR/LF has fully decoded. Grapheme/paragraph hard-boundary
    /// facts settle the current cluster without reading the first hidden character.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn settle_hard_boundary(
        &mut self,
        byte: usize,
        utf16: usize,
        store: &mut Store,
        step: &mut Step,
        budget: Budget,
    ) -> Result<bool> {
        if self.byte != byte || self.utf16 != utf16 || self.used != 0 {
            return Err(Error::CorruptMetadata);
        }
        if !self.pump(store, step, budget, false)? {
            return Ok(false);
        }
        if let Some(cluster) = self.cluster.take() {
            if !cluster.paragraph
                || cluster.end_byte != self.byte
                || cluster.end_utf16 != self.utf16
            {
                return Err(Error::CorruptMetadata);
            }
            self.action = Some(Action {
                cluster,
                phase: Phase::Locate,
            });
        }
        if !self.pump(store, step, budget, false)? {
            return Ok(false);
        }
        if self.current.is_some() || self.held.is_some() || self.top != store::NONE {
            return Err(Error::CorruptMetadata);
        }
        Ok(true)
    }

    /// Original paragraph processing ended header bracket/preceding context. Actual
    /// tail terminators restore grapheme state only, granting no hidden-text SCX.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn resume_after_fold(
        &mut self,
        span: &crate::markdown_layout_source::folds::FoldSpan,
        tail: &[u8],
    ) -> Result<()> {
        if self.byte != span.header_end.byte
            || self.held.is_some()
            || self.cluster.is_some()
            || self.action.is_some()
            || self.current.is_some()
            || self.top != store::NONE
            || tail.iter().any(|byte| !matches!(*byte, b'\r' | b'\n'))
        {
            return Err(Error::CorruptMetadata);
        }
        if !tail.is_empty() {
            self.cursor = MarkdownGraphemeScalarCursor::default();
            for byte in tail {
                self.cursor.push(char::from(*byte));
            }
        } // No tail separator requires the original proof's actual EOF; no later visible scalar needs context.
        self.byte = span.end.byte;
        self.utf16 = span.end.utf16;
        self.scalar_start = span.end.byte;
        self.pending_utf8 = [0; 4];
        self.used = 0;
        self.needed = 0;
        self.preceding = None;
        self.same_kind = [store::NONE; 65];
        Ok(())
    }

    #[cfg(all(test, feature = "paged-shaping-frontier"))]
    pub(super) fn has_pending_action(&self) -> bool {
        self.action.is_some()
    }

    /// Once the candidate set narrows to one script, later compatible intersections
    /// cannot change the script of this settled prefix.
    /// Pending held scalars/actions forbid lending a proof, so partially completed
    /// bracket or metadata operations cannot masquerade as settled work.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn stable_open_prefix(&self) -> Option<Run> {
        if self.held.is_some() || self.action.is_some() {
            return None;
        }
        let run = self.current.as_ref()?;
        let scripts = run.scripts?;
        let script = scripts.first()?;
        if scripts != Set::singleton(script)
            || run.settled_end.0 <= run.start_byte
            || run.settled_end.1 <= run.start_utf16
        {
            return None; // Common/Inherited or multiple SCX candidates must await actual context.
        }
        Some(Run {
            start_byte: run.start_byte,
            end_byte: run.settled_end.0,
            start_utf16: run.start_utf16,
            end_utf16: run.settled_end.1,
            script: Script(script),
        })
    }

    /// Extends only original stable SCX context to the actual body endpoint. Pending
    /// actions, incomplete UTF-8, and real bracket frames must not be skipped.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn latin_suffix(&self, position: usize, content_end: usize) -> Option<Run> {
        let run = self.stable_open_prefix()?;
        (self.used == 0
            && self.utf16 <= content_end
            && run.end_utf16 <= content_end
            && self.top == store::NONE
            && run.script.0 == properties::get('a').primary
            && run.start_utf16 <= position
            && position < run.end_utf16)
            .then_some(run)
    }

    /// A proven ASCII suffix advances this same run to the actual paragraph body
    /// endpoint, without writing a closed run or sealing its store.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn advance_proven_ascii(&mut self, byte: usize, utf16: usize) {
        self.current
            .as_mut()
            .expect("证明期间原Latn run未改变")
            .settled_end = (byte, utf16);
        self.byte = byte;
        self.utf16 = utf16;
        self.scalar_start = byte;
        self.pending_utf8 = [0; 4];
        self.used = 0;
        self.needed = 0;
        self.cluster = None;
        // Held scalars/actions already settled; the last actual scalar belongs to
        // the proven ASCII domain. Original grapheme advancement for
        // Other/incb=None/ep=false clears RI, EP/ZWJ, and Indic suffixes. Another
        // ASCII scalar has that same state, so preserve the original cursor.
        // Later paragraph controls/CRLF still enter push_byte/pump and Paragraph;
        // this content endpoint cannot authorize a closed SCX run.
    }

    /// Completes the same machine only after complete-aggregate or contextual-suffix
    /// EOF proof and actual original-store sealing.
    /// Byte/UTF-16 endpoints describe semantic completion. Outer owners account for
    /// actual reads separately; aggregate skipping is never reported as source I/O.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn finish_proven_eof(&mut self, byte: usize, utf16: usize) {
        self.byte = byte;
        self.utf16 = utf16;
        self.scalar_start = byte;
        self.pending_utf8 = [0; 4];
        self.used = 0;
        self.needed = 0;
        self.held = None;
        self.cluster = None;
        self.action = None;
        self.current = None;
        self.preceding = None;
        self.top = store::NONE;
        self.same_kind = [store::NONE; 65];
        self.finish_phase = 2;
        self.ready = true;
    }

    /// Completion requires both an empty byte carry and the actual expected
    /// UTF-16 endpoint, not merely the absence of another borrowed source page.
    pub(super) fn complete_utf8(&self, expected: usize) -> bool {
        self.used == 0 && self.utf16 == expected
    }
    /// Accepts one actual byte only when prior scalar/actions have settled.
    /// A complete scalar enters the same grapheme cursor once; partial UTF-8 stays fixed-size.
    pub(super) fn push_byte(&mut self, byte: u8, step: &mut Step) -> Result<()> {
        if self.held.is_some() || self.action.is_some() || self.ready {
            return Err(Error::CorruptMetadata);
        }
        let position = self.byte;
        self.byte = self.byte.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        step.bytes += 1; // Charge actual source bytes even when subsequent UTF-8 validation fails.
        if self.used == 0 {
            self.scalar_start = position;
            self.needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(Error::InvalidUtf8),
            };
        }
        self.pending_utf8[self.used] = byte;
        self.used += 1;
        if self.used != self.needed {
            // A source chunk can end inside a scalar. Preserve its original bytes
            // without advancing UTF-16 or applying grapheme/script properties early.
            return Ok(());
        }
        let scalar = std::str::from_utf8(&self.pending_utf8[..self.used])
            .ok()
            .and_then(|value| value.chars().next())
            .ok_or(Error::InvalidUtf8)?;
        let start_utf16 = self.utf16;
        self.utf16 = self
            .utf16
            .checked_add(scalar.len_utf16())
            .ok_or(Error::CoordinateOverflow)?;
        self.held = Some(Decoded {
            properties: properties::get(scalar),
            start_byte: self.scalar_start,
            end_byte: self.byte,
            start_utf16,
            end_utf16: self.utf16,
            boundary: self.cursor.push(scalar),
        });
        self.used = 0;
        step.scalars += 1;
        Ok(())
    }

    /// False means this quantum exhausted its budget only. Preserve held scalar
    /// and bracket-phase identity, without rereading text or repushing grapheme state.
    pub(super) fn pump(
        &mut self,
        store: &mut Store,
        step: &mut Step,
        budget: Budget,
        eof: bool,
    ) -> Result<bool> {
        loop {
            if let Some(mut action) = self.action.take() {
                // Resume this exact action before accepting the held next scalar.
                // Quota exhaustion restores its phase rather than replaying I/O.
                if !self.advance_action(&mut action, store, step, budget)? {
                    self.action = Some(action);
                    return Ok(false);
                }
                continue;
            }
            if let Some(value) = self.held.take() {
                let mark = value.properties.mark;
                let closes = self
                    .cluster
                    .as_ref()
                    .is_some_and(|cluster| value.boundary && (cluster.paragraph || !mark));
                if closes {
                    // Settle the completed cluster first; the lookahead scalar stays
                    // held so grapheme state and byte consumption are not repeated.
                    let cluster = self.cluster.take().ok_or(Error::CorruptMetadata)?;
                    self.action = Some(Action {
                        cluster,
                        phase: Phase::Locate,
                    });
                    self.held = Some(value);
                    continue;
                }
                let cluster = self.cluster.get_or_insert_with(|| Cluster::new(&value));
                cluster.add(value)?;
                continue;
            }
            if !eof {
                // No pending scalar/action remains, but an open cluster/run can
                // still gain context from actual later source input.
                return Ok(true);
            }
            if let Some(cluster) = self.cluster.take() {
                self.action = Some(Action {
                    cluster,
                    phase: Phase::Locate,
                });
                continue;
            }
            if self.current.is_some() {
                if step.records == budget.records {
                    return Ok(false);
                }
                self.commit_run(self.byte, self.utf16, store, step)?;
                continue;
            }
            // After actual EOF leaves no pending cluster/run, seal and release the
            // two actual storage owners through separate bounded operations.
            if step.stack_ops == budget.stack_ops {
                return Ok(false);
            }
            match self.finish_phase {
                0 => {
                    store.seal_frames()?;
                    self.finish_phase = 1;
                    step.stack_ops += 1;
                }
                1 => {
                    store.seal_runs()?;
                    self.finish_phase = 2;
                    step.stack_ops += 1;
                    self.ready = true;
                    return Ok(true);
                }
                _ => return Ok(true),
            }
        }
    }

    /// Commits only a settled nonempty run under its surviving candidate set.
    /// Failed append preserves the current candidate for the original failure owner.
    fn commit_run(
        &mut self,
        end_byte: usize,
        end_utf16: usize,
        store: &mut Store,
        step: &mut Step,
    ) -> Result<()> {
        let current = self.current.as_ref().ok_or(Error::CorruptMetadata)?;
        let script = current.script(self.preceding)?;
        store.append_run(Run {
            start_byte: current.start_byte,
            end_byte,
            start_utf16: current.start_utf16,
            end_utf16,
            script: Script(script),
        })?;
        self.current = None;
        self.preceding = Some(script);
        step.records += 1;
        Ok(()) // Clear the candidate/advance its ID only after actual append succeeds.
    }

    /// Settles one complete cluster through resumable pairing, run merge, and
    /// paragraph phases. Every real frame read/write or emitted record charges its quota.
    fn advance_action(
        &mut self,
        action: &mut Action,
        store: &mut Store,
        step: &mut Step,
        budget: Budget,
    ) -> Result<bool> {
        loop {
            match action.phase {
                Phase::Locate => {
                    // Same-kind heads locate a potential matching opener directly;
                    // no scan over arbitrary historical frames or source text is needed.
                    if let Some(Bracket {
                        kind,
                        opening: false,
                    }) = action.cluster.bracket
                    {
                        let target = self.same_kind[usize::from(kind)];
                        if target != store::NONE {
                            if step.stack_ops == budget.stack_ops {
                                return Ok(false);
                            }
                            let frame = store.frame(target)?;
                            step.stack_ops += 1;
                            action.phase = Phase::Resolve {
                                target,
                                run: frame.run,
                            };
                            continue;
                        }
                    }
                    action.phase = Phase::Merge; // An unmatched closer creates no pair and scans no entire historical stack.
                }
                Phase::Resolve { target, run } => {
                    // An opener in a committed run gives its closer that exact script.
                    // An opener in the current unresolved run shares the eventual choice.
                    if run < store.run_count() {
                        if step.stack_ops == budget.stack_ops {
                            return Ok(false);
                        }
                        let script = store.run(run)?.script.0;
                        step.stack_ops += 1;
                        action.cluster.scripts = Some(Set::singleton(script));
                        action.cluster.primary = Some(script);
                    } else if run > store.run_count() {
                        return Err(Error::CorruptMetadata);
                    }
                    // A pair in the same pending run adds no constraint; both ends receive
                    // its eventual actual script together.
                    action.phase = Phase::Pop { target };
                }
                Phase::Pop { target } => {
                    // Pop actual frames through the matched opener, updating both the
                    // nesting head and per-kind links only after each real read succeeds.
                    if step.stack_ops == budget.stack_ops {
                        return Ok(false);
                    }
                    let index = self.top;
                    if index == store::NONE {
                        return Err(Error::CorruptMetadata);
                    }
                    let frame = store.frame(index)?;
                    step.stack_ops += 1;
                    self.same_kind[usize::from(frame.kind)] = frame.same_kind;
                    self.top = frame.previous;
                    if index == target {
                        action.phase = Phase::Merge;
                    }
                    // Every popped frame consumes budget. Deep/misnested input may pause,
                    // but no arbitrary depth cutoff discards earlier pairing facts.
                }
                Phase::Merge => {
                    // Intersect complete-cluster candidates with the open run. Only
                    // actual incompatibility closes the earlier run at this cluster start.
                    let cluster = action.cluster;
                    let intersection = match (
                        self.current.as_ref().and_then(|run| run.scripts),
                        cluster.scripts,
                    ) {
                        (Some(current), Some(next)) => Some(current.intersection(next)),
                        _ => None,
                    };
                    if intersection == Some(None) {
                        // Disjoint nonwildcard sets require a boundary; an exhausted
                        // record quota cannot silently merge them or discard the cluster.
                        if step.records == budget.records {
                            return Ok(false);
                        }
                        self.commit_run(cluster.start_byte, cluster.start_utf16, store, step)?;
                    }
                    if let Some(run) = self.current.as_mut() {
                        if let Some(Some(set)) = intersection {
                            run.scripts = Some(set);
                            if run.primary.is_none() {
                                run.primary = cluster.primary;
                            }
                        } else if run.scripts.is_none() {
                            // Earlier wildcard text adopts the first actual constrained
                            // cluster's candidates without copying any preceding bytes.
                            run.scripts = cluster.scripts;
                            run.primary = cluster.primary;
                        }
                    } else {
                        self.current = Some(CurrentRun {
                            start_byte: cluster.start_byte,
                            start_utf16: cluster.start_utf16,
                            scripts: cluster.scripts,
                            primary: cluster.primary,
                            #[cfg(feature = "paged-shaping-frontier")]
                            settled_end: (cluster.start_byte, cluster.start_utf16),
                        });
                    }
                    action.phase = Phase::Open;
                }
                Phase::Open => {
                    // Record a real opening bracket against the current run ID only
                    // after merge settles its position in the contiguous coverage stream.
                    if let Some(Bracket {
                        kind,
                        opening: true,
                    }) = action.cluster.bracket
                    {
                        if step.records == budget.records || step.stack_ops == budget.stack_ops {
                            return Ok(false);
                        }
                        let index = store.append_frame(Frame {
                            previous: self.top,
                            same_kind: self.same_kind[usize::from(kind)],
                            run: store.run_count(),
                            kind,
                        })?;
                        step.records += 1;
                        step.stack_ops += 1;
                        self.top = index;
                        self.same_kind[usize::from(kind)] = index;
                    }
                    action.phase = Phase::Paragraph;
                }
                Phase::Paragraph => {
                    // A real paragraph control closes the current run and ends both
                    // preceding-script preference and pairing context for later text.
                    if action.cluster.paragraph {
                        if step.records == budget.records {
                            return Ok(false);
                        }
                        self.commit_run(
                            action.cluster.end_byte,
                            action.cluster.end_utf16,
                            store,
                            step,
                        )?;
                        self.top = store::NONE;
                        self.same_kind = [store::NONE; 65];
                        self.preceding = None;
                    } // Real paragraph controls end context by resetting 65 heads, without rereading old frames/text.
                    action.phase = Phase::Done;
                }
                Phase::Done => {
                    #[cfg(feature = "paged-shaping-frontier")]
                    if let Some(run) = &mut self.current {
                        // Preserve original run-selection/write ordering. Advance the borrowable
                        // open prefix only after every phase of this cluster succeeds;
                        // later scalars remain in their original cluster/held state and cannot
                        // enter this certified prefix.
                        run.settled_end = (action.cluster.end_byte, action.cluster.end_utf16);
                    }
                    return Ok(true);
                }
            }
        }
    }
}
