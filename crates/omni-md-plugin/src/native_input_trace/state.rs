//! Finite call-stack provenance and canonical decisions, independent of native clock acquisition.

use super::*;

/// One input selector frame owns counters only; no native event or text payload is retained.
#[derive(Clone, Copy)]
struct SelectorFrame {
    serial: u64,
    selector: NativeInputTraceSelector,
    before: Option<NativeInputTraceDocument>,
    had_marked: bool,
    accepted: u32,
    rejected: u32,
}

/// A guard restores the precise enclosing event/selector context on return or unwinding.
#[derive(Clone, Copy)]
enum ScopeToken {
    Event { serial: u64, previous: Option<u64> },
    Selector { serial: u64, depth: usize },
}

/// All recording storage belongs to one activated editor and disappears with that native graph.
pub(crate) struct NativeInputTrace {
    records: [NativeInputTraceRecord; NATIVE_INPUT_TRACE_CAPACITY],
    len: usize,
    armed: bool,
    overflowed: bool,
    invalid_scope: bool,
    aborted_scope: bool,
    next_record: u64,
    next_event: u64,
    next_selector: u64,
    next_composition: u64,
    event: Option<u64>,
    event_depth: usize,
    composition: Option<u64>,
    selectors: [Option<SelectorFrame>; MAX_SCOPE_DEPTH],
    depth: usize,
    armed_at: u64,
    numer: u32,
    denom: u32,
    initial: NativeInputTraceDocument,
}

impl Default for NativeInputTrace {
    fn default() -> Self {
        // A feature-enabled but unarmed editor owns no trace text, native links, observer or timer.
        Self {
            records: [NativeInputTraceRecord::default(); NATIVE_INPUT_TRACE_CAPACITY],
            len: 0,
            armed: false,
            overflowed: false,
            invalid_scope: false,
            aborted_scope: false,
            next_record: 0,
            next_event: 0,
            next_selector: 0,
            next_composition: 0,
            event: None,
            event_depth: 0,
            composition: None,
            selectors: [None; MAX_SCOPE_DEPTH],
            depth: 0,
            armed_at: 0,
            numer: 0,
            denom: 0,
            initial: NativeInputTraceDocument::default(),
        }
    }
}

impl NativeInputTrace {
    /// Start only at an explicit clean boundary, preserving any earlier undrained evidence.
    pub(crate) fn arm(
        &mut self,
        ticks: u64,
        numer: u32,
        denom: u32,
        initial: NativeInputTraceDocument,
        marked: bool,
    ) -> Result<(), NativeInputTraceAccessError> {
        if self.armed || self.len != 0 {
            // Replacing an in-progress or undrained run would silently erase evidence.
            return Err(NativeInputTraceAccessError::AlreadyArmed);
        }
        if marked {
            // A composition already underway has no observed starting event in this run.
            return Err(NativeInputTraceAccessError::CompositionActive);
        }
        if numer == 0 || denom == 0 || ticks == 0 {
            // Invalid clock units cannot support later frame correlation.
            return Err(NativeInputTraceAccessError::ClockUnavailable);
        }
        *self = Self::default();
        self.armed = true;
        self.armed_at = ticks;
        self.numer = numer;
        self.denom = denom;
        self.initial = initial;
        Ok(())
    }

    /// Stop and transfer scalar evidence only after every synchronous input stack has returned.
    pub(crate) fn drain(
        &mut self,
    ) -> Result<NativeInputTraceSnapshot, NativeInputTraceAccessError> {
        if !self.armed {
            // An empty unarmed state is not a completed measurement interval.
            return Err(NativeInputTraceAccessError::NotArmed);
        }
        if self.depth != 0 || self.event_depth != 0 {
            // Mid-stack draining would split a selector from its canonical decisions and causal event.
            return Err(NativeInputTraceAccessError::ActiveScope);
        }
        let records = std::mem::replace(
            &mut self.records,
            [NativeInputTraceRecord::default(); NATIVE_INPUT_TRACE_CAPACITY],
        );
        let snapshot = NativeInputTraceSnapshot {
            records,
            len: self.len,
            overflowed: self.overflowed,
            invalid_scope: self.invalid_scope,
            aborted_scope: self.aborted_scope,
            armed_at_mach_ticks: self.armed_at,
            timebase_numer: self.numer,
            timebase_denom: self.denom,
            initial_document: self.initial,
        };
        // No automatic flush or periodic work exists; the caller now owns the only copied evidence.
        *self = Self::default();
        Ok(snapshot)
    }

    /// Snapshot only finite current causal context; caller-supplied counters contain no ownership handle.
    fn record(
        &self,
        kind: NativeInputTraceKind,
        ticks: u64,
        document: Option<NativeInputTraceDocument>,
        marked: bool,
    ) -> NativeInputTraceRecord {
        let frame = self
            .depth
            .checked_sub(1)
            .and_then(|index| self.selectors[index]);
        NativeInputTraceRecord {
            kind,
            mach_ticks: ticks,
            event_serial: self.event,
            selector_serial: frame.map(|frame| frame.serial),
            composition_serial: self.composition,
            selector: frame.map(|frame| frame.selector),
            document,
            native_marked: Some(marked),
            ..NativeInputTraceRecord::default()
        }
    }

    /// Capacity and serial exhaustion are explicit invalidity, never a rolling history overwrite.
    fn push(&mut self, mut record: NativeInputTraceRecord) {
        if self.len == NATIVE_INPUT_TRACE_CAPACITY {
            // Keep the earliest causal evidence intact so overflow cannot create a deceptively short trace.
            self.overflowed = true;
            return;
        }
        let Some(serial) = self.next_record.checked_add(1) else {
            // A wrapped identifier could falsely join unrelated callbacks.
            self.invalid_scope = true;
            return;
        };
        self.next_record = serial;
        record.serial = serial;
        self.records[self.len] = record;
        self.len += 1;
    }

    /// Record the actual canonical Result before native notifications can cause further input reentry.
    pub(crate) fn canonical(
        &mut self,
        ticks: u64,
        document: Option<NativeInputTraceDocument>,
        removed: usize,
        inserted: usize,
        rejection: Option<NativeInputTraceRejection>,
    ) {
        if !self.armed {
            // Feature-enabled normal use stores no records until an explicit synthetic measurement arms it.
            return;
        }
        if let Some(root) = self.selectors[0].as_mut() {
            // Nested native selectors contribute to their one outer operation, not separate premature commits.
            let count = if rejection.is_some() {
                // A failed admission/domain decision vetoes the complete outer operation.
                &mut root.rejected
            } else {
                // Actual Ok decisions remain distinct from generation-change inference.
                &mut root.accepted
            };
            if let Some(next) = count.checked_add(1) {
                // A real accepted/rejected transaction increments exactly one finite decision counter.
                *count = next;
            } else {
                // Counter saturation is invalid evidence rather than a silent wrap.
                self.invalid_scope = true;
            }
        }
        let kind = if rejection.is_some() {
            // Admission and domain failures never become successful committed-input records.
            NativeInputTraceKind::CanonicalRejected
        } else {
            // Ok is observed directly, even when a domain no-op leaves generation unchanged.
            NativeInputTraceKind::CanonicalAccepted
        };
        let mut record = self.record(kind, ticks, document, false);
        // The storage primitive observes a canonical Result, not a native marked-state getter.
        record.native_marked = None;
        record.removed_utf16_units = removed;
        record.inserted_utf16_units = inserted;
        record.rejection = rejection;
        self.push(record);
    }

    /// Restore guards without output; an uncompleted scope invalidates the entire sample.
    fn restore(&mut self, token: ScopeToken, aborted: bool) {
        self.aborted_scope |= aborted;
        match token {
            // Key events restore their exact enclosing event, never a last-event global guess.
            ScopeToken::Event { serial, previous } => {
                self.invalid_scope |= self.event != Some(serial) || self.event_depth == 0;
                self.event = previous;
                self.event_depth = self.event_depth.saturating_sub(1);
            }
            // Selector nesting must unwind in order; finite slots retain no text or native owners.
            ScopeToken::Selector { serial, depth } => {
                self.invalid_scope |= self.depth != depth + 1
                    || self.selectors[depth].is_none_or(|frame| frame.serial != serial);
                self.selectors[depth] = None;
                self.depth = depth;
            }
        }
    }
}

/// No RefCell borrow survives into AppKit. Drop only restores scalar state; it never logs or flushes.
pub(crate) struct TraceScope {
    shared: SharedNativeInputTrace,
    token: Option<ScopeToken>,
}

impl TraceScope {
    /// Capture a real keyDown stack and preserve exact source timestamp bits without clock conversion.
    pub(crate) fn event(
        shared: &SharedNativeInputTrace,
        ticks: u64,
        timestamp_bits: u64,
        document: Option<NativeInputTraceDocument>,
        marked: bool,
    ) -> Option<Self> {
        let mut state = shared.borrow_mut();
        if !state.armed {
            // Cold feature-enabled runs do not create a guard or retain additional trace ownership.
            return None;
        }
        if state.event_depth == MAX_SCOPE_DEPTH || state.next_event == u64::MAX {
            // Unbounded native reentry cannot exceed fixed scope/identifier storage.
            state.invalid_scope = true;
            return None;
        }
        // A different event reentering an unfinished selector makes causal attribution ambiguous.
        state.invalid_scope |= state.depth != 0;
        // Preserve exact source bits while rejecting nonfinite/negative native timestamps as evidence.
        state.invalid_scope |=
            !f64::from_bits(timestamp_bits).is_finite() || f64::from_bits(timestamp_bits) < 0.0;
        let previous = state.event;
        state.next_event += 1;
        let serial = state.next_event;
        state.event = Some(serial);
        state.event_depth += 1;
        let mut record = state.record(NativeInputTraceKind::KeyDownBegin, ticks, document, marked);
        record.nsevent_timestamp_bits = Some(timestamp_bits);
        state.push(record);
        drop(state);
        Some(Self {
            shared: Rc::clone(shared),
            token: Some(ScopeToken::Event { serial, previous }),
        })
    }

    /// Begin one standard input callback, retaining only its currently executing causal event identity.
    pub(crate) fn selector(
        shared: &SharedNativeInputTrace,
        selector: NativeInputTraceSelector,
        ticks: u64,
        document: Option<NativeInputTraceDocument>,
        marked: bool,
    ) -> Option<Self> {
        let mut state = shared.borrow_mut();
        if !state.armed {
            // Nothing is retained or recorded before explicit measurement admission.
            return None;
        }
        if state.depth == MAX_SCOPE_DEPTH || state.next_selector == u64::MAX {
            // Overflow fails evidence; a nested operation is never silently reassigned to another selector.
            state.invalid_scope = true;
            return None;
        }
        if selector == NativeInputTraceSelector::SetMarkedText && state.composition.is_none() {
            // One composition identity spans multiple real keyDown events and asynchronous callbacks.
            let Some(next) = state.next_composition.checked_add(1) else {
                // Serial exhaustion cannot be repaired by wrapping into an earlier composition identity.
                state.invalid_scope = true;
                return None;
            };
            state.next_composition = next;
            state.composition = Some(next);
        }
        state.next_selector += 1;
        let serial = state.next_selector;
        let depth = state.depth;
        state.selectors[depth] = Some(SelectorFrame {
            serial,
            selector,
            before: document,
            had_marked: marked,
            accepted: 0,
            rejected: 0,
        });
        state.depth += 1;
        let record = state.record(NativeInputTraceKind::SelectorBegin, ticks, document, marked);
        state.push(record);
        drop(state);
        Some(Self {
            shared: Rc::clone(shared),
            token: Some(ScopeToken::Selector { serial, depth }),
        })
    }

    /// Finish only after superclass input and adapter reconciliation complete, with actual native marked state.
    pub(crate) fn finish(
        mut self,
        ticks: u64,
        document: Option<NativeInputTraceDocument>,
        marked: bool,
    ) {
        let Some(token) = self.token.take() else {
            // A consumed scope cannot publish a duplicate completion.
            return;
        };
        let mut state = self.shared.borrow_mut();
        match token {
            // Returning from actual event dispatch closes only that event's stack association.
            ScopeToken::Event { .. } => {
                let record =
                    state.record(NativeInputTraceKind::KeyDownEnd, ticks, document, marked);
                state.push(record);
            }
            // Only the outer selector can claim committed input; nested unmark remains explicitly nested.
            ScopeToken::Selector { depth, .. } => {
                let Some(frame) = state.selectors[depth] else {
                    // A broken nesting contract invalidates evidence instead of panicking through AppKit.
                    state.invalid_scope = true;
                    state.restore(token, false);
                    return;
                };
                let mut record =
                    state.record(NativeInputTraceKind::SelectorEnd, ticks, document, marked);
                record.accepted_count = frame.accepted;
                record.rejected_count = frame.rejected;
                record.outcome = if depth != 0 {
                    // Native unmark callbacks inside insert/mark cannot move the commit boundary earlier.
                    NativeInputTraceOutcome::Nested
                } else if frame.rejected != 0 {
                    // One failed canonical attempt vetoes a successful-commit claim for the whole outer operation.
                    NativeInputTraceOutcome::Rejected
                } else if frame.selector == NativeInputTraceSelector::SetMarkedText {
                    // Canonical provisional edits are not committed IME text even when generation changed.
                    NativeInputTraceOutcome::Provisional
                } else if marked {
                    // A still-marked native client has not completed its composition.
                    NativeInputTraceOutcome::NoCommit
                } else if frame
                    .before
                    .zip(document)
                    .is_some_and(|(before, after)| before.generation != after.generation)
                    && frame.accepted != 0
                {
                    // Actual accepted canonical change plus cleared native marking establishes committed changed text.
                    NativeInputTraceOutcome::CommittedChanged
                } else if frame.had_marked {
                    // Unmark may accept existing characters without a new generation or primitive call.
                    NativeInputTraceOutcome::CommittedExisting
                } else {
                    // No-op/unhandled native callbacks provide no evidence of a completed insertion.
                    NativeInputTraceOutcome::NoCommit
                };
                state.push(record);
                if depth == 0 && !marked {
                    // Preserve identity through its last record, then end the composition even when
                    // an empty/cancelled provisional span cleared native marking without committing.
                    state.composition = None;
                }
            }
        }
        state.restore(token, false);
    }
}

impl Drop for TraceScope {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            // Unwind restores provenance and marks invalidity only. No clock, text, output or native call occurs here.
            self.shared.borrow_mut().restore(token, true);
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
