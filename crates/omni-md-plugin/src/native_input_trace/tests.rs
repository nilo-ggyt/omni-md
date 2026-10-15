//! Behavior contracts for causal scope restoration, IME phases, rejection and finite evidence.

use super::*;

/// Stable scalar fixtures avoid any text, native event object or platform clock dependency.
fn document(generation: u64) -> NativeInputTraceDocument {
    NativeInputTraceDocument {
        generation,
        utf8_bytes: 1_048_576 + generation as usize,
        utf16_units: 1_048_576 + generation as usize,
    }
}

/// Every scenario explicitly arms its own foreground recorder and timebase.
fn armed() -> SharedNativeInputTrace {
    let state = Rc::new(RefCell::new(NativeInputTrace::default()));
    state
        .borrow_mut()
        .arm(10, 125, 3, document(0), false)
        .unwrap();
    state
}

/// Real synchronous dispatch retains the exact timestamp bits and associates accepted input with that stack only.
#[test]
fn actual_event_scope_tracks_canonical_acceptance_without_retaining_the_event() {
    let state = armed();
    let timestamp = 12345.6789_f64.to_bits();
    let event = TraceScope::event(&state, 20, timestamp, Some(document(0)), false).unwrap();
    let insert = TraceScope::selector(
        &state,
        NativeInputTraceSelector::InsertText,
        21,
        Some(document(0)),
        false,
    )
    .unwrap();
    state
        .borrow_mut()
        .canonical(22, Some(document(1)), 0, 1, None);
    insert.finish(23, Some(document(1)), false);
    event.finish(24, Some(document(1)), false);
    let snapshot = state.borrow_mut().drain().unwrap();
    assert!(snapshot.is_valid());
    assert_eq!(
        snapshot.records()[0].nsevent_timestamp_bits,
        Some(timestamp)
    );
    assert!(
        snapshot
            .records()
            .iter()
            .all(|record| record.event_serial == Some(1))
    );
    let commit = snapshot
        .records()
        .iter()
        .find(|record| record.outcome == NativeInputTraceOutcome::CommittedChanged)
        .unwrap();
    assert_eq!(commit.mach_ticks, 23);
    assert_eq!(commit.accepted_count, 1);
    assert_eq!(commit.rejected_count, 0);
    assert_eq!(commit.native_marked, Some(false));
    assert_eq!(snapshot.records()[2].native_marked, None);
    // Draining consumes the run rather than preserving an idle global history.
    assert_eq!(
        state.borrow_mut().drain().unwrap_err(),
        NativeInputTraceAccessError::NotArmed
    );
}

/// Provisional mutations are never commits; a later async callback cannot inherit the last key's identity.
#[test]
fn asynchronous_ime_commit_keeps_composition_but_has_no_guessed_causal_event() {
    let state = armed();
    let event = TraceScope::event(&state, 20, 1.0_f64.to_bits(), Some(document(0)), false).unwrap();
    let mark = TraceScope::selector(
        &state,
        NativeInputTraceSelector::SetMarkedText,
        21,
        Some(document(0)),
        false,
    )
    .unwrap();
    state
        .borrow_mut()
        .canonical(22, Some(document(1)), 0, 2, None);
    mark.finish(23, Some(document(1)), true);
    event.finish(24, Some(document(1)), true);
    let commit = TraceScope::selector(
        &state,
        NativeInputTraceSelector::InsertText,
        30,
        Some(document(1)),
        true,
    )
    .unwrap();
    state
        .borrow_mut()
        .canonical(31, Some(document(2)), 2, 2, None);
    commit.finish(32, Some(document(2)), false);
    let snapshot = state.borrow_mut().drain().unwrap();
    let provisional = snapshot
        .records()
        .iter()
        .find(|record| record.outcome == NativeInputTraceOutcome::Provisional)
        .unwrap();
    assert_eq!(provisional.composition_serial, Some(1));
    let committed = snapshot
        .records()
        .iter()
        .find(|record| record.outcome == NativeInputTraceOutcome::CommittedChanged)
        .unwrap();
    assert_eq!(committed.composition_serial, Some(1));
    assert_eq!(committed.event_serial, None);
    assert_eq!(committed.mach_ticks, 32);
}

/// Native nested unmark cannot publish a commit before its surrounding insert and reconciliation finish.
#[test]
fn nested_unmark_and_zero_generation_commit_have_distinct_completion_authority() {
    let state = armed();
    let marked = TraceScope::selector(
        &state,
        NativeInputTraceSelector::SetMarkedText,
        20,
        Some(document(0)),
        false,
    )
    .unwrap();
    state
        .borrow_mut()
        .canonical(21, Some(document(1)), 0, 1, None);
    marked.finish(22, Some(document(1)), true);
    let insert = TraceScope::selector(
        &state,
        NativeInputTraceSelector::InsertText,
        30,
        Some(document(1)),
        true,
    )
    .unwrap();
    let unmark = TraceScope::selector(
        &state,
        NativeInputTraceSelector::UnmarkText,
        31,
        Some(document(1)),
        true,
    )
    .unwrap();
    unmark.finish(32, Some(document(1)), false);
    insert.finish(33, Some(document(1)), false);
    let snapshot = state.borrow_mut().drain().unwrap();
    let endings: Vec<_> = snapshot
        .records()
        .iter()
        .filter(|record| record.kind == NativeInputTraceKind::SelectorEnd)
        .collect();
    assert_eq!(endings[1].outcome, NativeInputTraceOutcome::Nested);
    assert_eq!(
        endings[2].outcome,
        NativeInputTraceOutcome::CommittedExisting
    );
    assert_eq!(endings[2].mach_ticks, 33);
    assert_eq!(endings[2].accepted_count, 0);
    assert_eq!(endings[2].document, Some(document(1)));
    // An independent outer unmark is also a valid existing-text commit, with no fabricated primitive.
    state
        .borrow_mut()
        .arm(40, 1, 1, document(1), false)
        .unwrap();
    let unmark = TraceScope::selector(
        &state,
        NativeInputTraceSelector::UnmarkText,
        41,
        Some(document(1)),
        true,
    )
    .unwrap();
    unmark.finish(42, Some(document(1)), false);
    assert_eq!(
        state.borrow_mut().drain().unwrap().records()[1].outcome,
        NativeInputTraceOutcome::CommittedExisting
    );
}

/// Rejected primitives veto the whole outer commit even if native marked bookkeeping clears afterwards.
#[test]
fn canonical_rejection_cannot_be_reclassified_by_native_unmark_or_generation_changes() {
    let state = armed();
    let insert = TraceScope::selector(
        &state,
        NativeInputTraceSelector::InsertText,
        20,
        Some(document(0)),
        true,
    )
    .unwrap();
    state.borrow_mut().canonical(
        21,
        Some(document(0)),
        0,
        8,
        Some(NativeInputTraceRejection::CanonicalError),
    );
    insert.finish(22, Some(document(0)), false);
    let snapshot = state.borrow_mut().drain().unwrap();
    assert_eq!(
        snapshot.records()[1].kind,
        NativeInputTraceKind::CanonicalRejected
    );
    assert_eq!(
        snapshot.records()[2].outcome,
        NativeInputTraceOutcome::Rejected
    );
    assert_eq!(snapshot.records()[2].accepted_count, 0);
    assert_eq!(snapshot.records()[2].rejected_count, 1);
}

/// A bounded recorder preserves its earliest records and explicitly invalidates excess evidence.
#[test]
fn overflow_never_overwrites_and_explicit_drain_allows_a_fresh_finite_run() {
    let state = armed();
    // More than one run's capacity is intentional adversarial input, not a rolling-history request.
    for index in 0..NATIVE_INPUT_TRACE_CAPACITY + 9 {
        state
            .borrow_mut()
            .canonical(20 + index as u64, Some(document(0)), 0, 1, None);
    }
    let snapshot = state.borrow_mut().drain().unwrap();
    assert_eq!(snapshot.records().len(), NATIVE_INPUT_TRACE_CAPACITY);
    assert_eq!(snapshot.records()[0].mach_ticks, 20);
    assert_eq!(
        snapshot.records().last().unwrap().serial,
        NATIVE_INPUT_TRACE_CAPACITY as u64
    );
    assert!(snapshot.overflowed);
    assert!(!snapshot.is_valid());
    state
        .borrow_mut()
        .arm(1000, 1, 1, document(0), false)
        .unwrap();
    let event =
        TraceScope::event(&state, 1001, 2.0_f64.to_bits(), Some(document(0)), false).unwrap();
    event.finish(1002, Some(document(0)), false);
    assert!(state.borrow_mut().drain().unwrap().is_valid());
}

/// Active-scope draining/rearming is rejected, while unwind restores context without output or guessed completion.
#[test]
fn scope_unwind_and_control_reentry_preserve_finite_evidence() {
    let state = armed();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _event =
            TraceScope::event(&state, 20, 1.0_f64.to_bits(), Some(document(0)), false).unwrap();
        assert_eq!(
            state.borrow_mut().drain().unwrap_err(),
            NativeInputTraceAccessError::ActiveScope
        );
        assert_eq!(
            state.borrow_mut().arm(21, 1, 1, document(0), false),
            Err(NativeInputTraceAccessError::AlreadyArmed)
        );
        // A synthetic caught panic tests RAII state restoration, not an input acceptance path.
        panic!("synthetic scope unwind");
    }));
    assert!(unwind.is_err());
    let async_insert = TraceScope::selector(
        &state,
        NativeInputTraceSelector::InsertText,
        30,
        Some(document(0)),
        false,
    )
    .unwrap();
    state
        .borrow_mut()
        .canonical(31, Some(document(1)), 0, 1, None);
    async_insert.finish(32, Some(document(1)), false);
    let snapshot = state.borrow_mut().drain().unwrap();
    assert!(snapshot.aborted_scope);
    assert!(!snapshot.is_valid());
    assert_eq!(snapshot.records().last().unwrap().event_serial, None);
}

/// Arming during existing composition and exceeding finite selector depth both fail evidence explicitly.
#[test]
fn incomplete_composition_admission_and_excessive_nesting_are_not_accepted_samples() {
    let mut cold = NativeInputTrace::default();
    assert_eq!(
        cold.arm(1, 1, 1, document(0), true),
        Err(NativeInputTraceAccessError::CompositionActive)
    );
    let state = armed();
    let mut scopes = Vec::new();
    // The test owns at most the declared finite scope capacity plus one rejected acquisition.
    for depth in 0..MAX_SCOPE_DEPTH {
        scopes.push(
            TraceScope::selector(
                &state,
                NativeInputTraceSelector::InsertText,
                20 + depth as u64,
                Some(document(0)),
                false,
            )
            .unwrap(),
        );
    }
    assert!(
        TraceScope::selector(
            &state,
            NativeInputTraceSelector::UnmarkText,
            40,
            Some(document(0)),
            false
        )
        .is_none()
    );
    // Complete accepted finite scopes in their actual nested return order.
    while let Some(scope) = scopes.pop() {
        scope.finish(50, Some(document(0)), false);
    }
    let snapshot = state.borrow_mut().drain().unwrap();
    assert!(snapshot.invalid_scope);
    assert!(!snapshot.is_valid());
}

/// Cold state and invalid timebases never acquire a measurement or retain a scope guard.
#[test]
fn unarmed_recording_and_invalid_clock_cannot_create_an_accepted_interval() {
    let state = Rc::new(RefCell::new(NativeInputTrace::default()));
    assert!(TraceScope::event(&state, 20, 1.0_f64.to_bits(), Some(document(0)), false).is_none());
    assert!(
        TraceScope::selector(
            &state,
            NativeInputTraceSelector::InsertText,
            21,
            Some(document(0)),
            false
        )
        .is_none()
    );
    state
        .borrow_mut()
        .canonical(22, Some(document(1)), 0, 1, None);
    assert_eq!(
        state.borrow_mut().drain().unwrap_err(),
        NativeInputTraceAccessError::NotArmed
    );
    assert_eq!(Rc::strong_count(&state), 1);
    assert_eq!(
        state.borrow_mut().arm(23, 1, 0, document(0), false),
        Err(NativeInputTraceAccessError::ClockUnavailable)
    );
}

/// A second actual event during an unfinished selector is observable ambiguity, not a guessed cause.
#[test]
fn reentrant_different_event_invalidates_source_attribution_and_restores_outer_identity() {
    let state = armed();
    let outer = TraceScope::event(&state, 20, 1.0_f64.to_bits(), Some(document(0)), false).unwrap();
    let insert = TraceScope::selector(
        &state,
        NativeInputTraceSelector::InsertText,
        21,
        Some(document(0)),
        false,
    )
    .unwrap();
    let nested =
        TraceScope::event(&state, 22, 1.1_f64.to_bits(), Some(document(0)), false).unwrap();
    nested.finish(23, Some(document(0)), false);
    state
        .borrow_mut()
        .canonical(24, Some(document(1)), 0, 1, None);
    insert.finish(25, Some(document(1)), false);
    outer.finish(26, Some(document(1)), false);
    let snapshot = state.borrow_mut().drain().unwrap();
    assert!(snapshot.invalid_scope);
    assert!(!snapshot.is_valid());
    assert_eq!(snapshot.records().last().unwrap().event_serial, Some(1));
    assert!(
        snapshot
            .records()
            .iter()
            .any(|record| record.event_serial == Some(2))
    );
}

/// Cancelling a provisional span ends its composition identity without fabricating a commit.
#[test]
fn cleared_provisional_span_does_not_merge_the_next_composition_into_the_old_one() {
    let state = armed();
    let first = TraceScope::selector(
        &state,
        NativeInputTraceSelector::SetMarkedText,
        20,
        Some(document(0)),
        false,
    )
    .unwrap();
    state
        .borrow_mut()
        .canonical(21, Some(document(1)), 0, 1, None);
    first.finish(22, Some(document(1)), true);
    let cancelled = TraceScope::selector(
        &state,
        NativeInputTraceSelector::SetMarkedText,
        23,
        Some(document(1)),
        true,
    )
    .unwrap();
    state
        .borrow_mut()
        .canonical(24, Some(document(2)), 1, 0, None);
    cancelled.finish(25, Some(document(2)), false);
    let next = TraceScope::selector(
        &state,
        NativeInputTraceSelector::SetMarkedText,
        30,
        Some(document(2)),
        false,
    )
    .unwrap();
    next.finish(31, Some(document(2)), true);
    let snapshot = state.borrow_mut().drain().unwrap();
    let completions: Vec<_> = snapshot
        .records()
        .iter()
        .filter(|record| record.kind == NativeInputTraceKind::SelectorEnd)
        .collect();
    assert!(
        completions
            .iter()
            .all(|record| record.outcome == NativeInputTraceOutcome::Provisional)
    );
    assert_eq!(completions[1].composition_serial, Some(1));
    assert_eq!(completions[2].composition_serial, Some(2));
}

/// A direct Enter/Tab/format publisher can reject on a real key stack without inventing insertText provenance.
#[test]
fn direct_command_rejection_keeps_actual_event_without_fabricating_a_selector_commit() {
    let state = armed();
    let event = TraceScope::event(&state, 20, 1.0_f64.to_bits(), Some(document(0)), false).unwrap();
    state.borrow_mut().canonical(
        21,
        Some(document(0)),
        0,
        0,
        Some(NativeInputTraceRejection::CanonicalError),
    );
    event.finish(22, Some(document(0)), false);
    let snapshot = state.borrow_mut().drain().unwrap();
    let rejected = &snapshot.records()[1];
    assert_eq!(rejected.kind, NativeInputTraceKind::CanonicalRejected);
    assert_eq!(rejected.event_serial, Some(1));
    assert_eq!(rejected.selector_serial, None);
    assert!(
        snapshot
            .records()
            .iter()
            .all(|record| record.outcome == NativeInputTraceOutcome::NotApplicable)
    );
}
