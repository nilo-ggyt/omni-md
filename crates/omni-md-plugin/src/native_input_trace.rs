//! Opt-in bounded scalar input evidence; this module never stores text or performs output.
//!
//! Actual responder-stack provenance is distinct from a last-event guess. Native adapters
//! supply clocks and document counters, while this pure state machine owns finite records.

#[cfg(any(target_os = "macos", test))]
use std::cell::RefCell;
#[cfg(any(target_os = "macos", test))]
use std::rc::Rc;

#[cfg(any(target_os = "macos", test))]
mod state;
#[cfg(target_os = "macos")]
pub(crate) use state::{NativeInputTrace, TraceScope};

/// A short measurement admits at most this many records; overflow invalidates rather than overwrites.
pub const NATIVE_INPUT_TRACE_CAPACITY: usize = 256;
/// Native reentry remains bounded independently of the number of records.
#[cfg(any(target_os = "macos", test))]
const MAX_SCOPE_DEPTH: usize = 8;

/// O(1) canonical counters copied without retaining a document, source piece or native object.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativeInputTraceDocument {
    /// Canonical generation; meaningful only within this snapshot's foreground document.
    pub generation: u64,
    /// Current UTF-8 length, without flattening the piece tree.
    pub utf8_bytes: usize,
    /// Current UTF-16 length used by the native input protocol.
    pub utf16_units: usize,
}

/// The public text-input selector that owns a bounded synchronous scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeInputTraceSelector {
    /// A normal or IME-finalized user insertion.
    InsertText,
    /// Provisional composition, even when canonical bytes/generation change.
    SetMarkedText,
    /// Acceptance of existing marked text, potentially without another character mutation.
    UnmarkText,
}

/// A record describes a source callback or an actual canonical decision, never a displayed frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeInputTraceKind {
    /// Unused fixed-array entry; excluded from the snapshot iterator.
    #[default]
    Empty,
    /// Entry to the actual text responder's keyDown: stack.
    KeyDownBegin,
    /// Return from this actual client's keyDown; deferred interpretation does
    /// not remain part of the original synchronous event stack.
    KeyDownEnd,
    /// Entry to a standard input selector, including nested selectors.
    SelectorBegin,
    /// Settled return from that selector; only an outer selector can represent commit completion.
    SelectorEnd,
    /// The canonical operation returned Ok; generation counters distinguish actual changes.
    CanonicalAccepted,
    /// Input admission or the canonical operation rejected a change.
    CanonicalRejected,
}

/// Interpretation of a selector return is separate from canonical generation changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeInputTraceOutcome {
    /// Event/canonical records have no independent commit claim.
    #[default]
    NotApplicable,
    /// A nested selector cannot prematurely complete its outer operation.
    Nested,
    /// Marked input is provisional regardless of canonical mutation.
    Provisional,
    /// An outer insertion accepted a canonical change and native marking has cleared.
    CommittedChanged,
    /// Existing marked text became accepted without a generation change.
    CommittedExisting,
    /// No accepted commit is established by this scope.
    NoCommit,
    /// At least one admission/canonical failure vetoed this input's commit claim.
    Rejected,
}

/// Finite failure categories contain no native exception, filesystem path or arbitrary error string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeInputTraceRejection {
    /// The accepted close/handoff gate forbids further input.
    Closing,
    /// Native unsigned/signed coordinates cannot represent this edit.
    CoordinateOverflow,
    /// The foreground canonical document is already absent.
    DocumentUnavailable,
    /// The canonical scalar/range/capacity transaction returned an error.
    CanonicalError,
}

/// One Copy-only record; optional serials are absent when stack causality is unavailable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativeInputTraceRecord {
    /// Strictly increasing record serial inside one explicit arm/drain interval.
    pub serial: u64,
    /// Callback or canonical-decision category.
    pub kind: NativeInputTraceKind,
    /// Mach absolute ticks supplied at the recorded boundary, never wall-clock time.
    pub mach_ticks: u64,
    /// Actual currently executing keyDown: scope; asynchronous IME delivery has None.
    pub event_serial: Option<u64>,
    /// Current native input selector scope, if any.
    pub selector_serial: Option<u64>,
    /// Composition identity spans provisional updates but owns no composition text.
    pub composition_serial: Option<u64>,
    /// Selector kind for selector/canonical records.
    pub selector: Option<NativeInputTraceSelector>,
    /// Exact f64 bits of NSEvent.timestamp, retained without assuming a Mach epoch.
    pub nsevent_timestamp_bits: Option<u64>,
    /// Scalar canonical counters copied at this boundary.
    pub document: Option<NativeInputTraceDocument>,
    /// Removed code-unit count, with no source offset or content.
    pub removed_utf16_units: usize,
    /// Inserted code-unit count, with no replacement text.
    pub inserted_utf16_units: usize,
    /// Actual NSTextInputClient marking. Canonical-result records have no
    /// independent input-protocol state and therefore contain None.
    pub native_marked: Option<bool>,
    /// Explicit commit/provisional interpretation, never inferred from a draw callback.
    pub outcome: NativeInputTraceOutcome,
    /// Finite rejection reason, if this record rejects input.
    pub rejection: Option<NativeInputTraceRejection>,
    /// Actual canonical accepts accumulated by the current outer selector.
    pub accepted_count: u32,
    /// Actual admission/canonical rejects accumulated by the current outer selector.
    pub rejected_count: u32,
}

/// The caller receives owned finite evidence after recording stops and every owner borrow ends.
#[derive(Clone, Debug)]
pub struct NativeInputTraceSnapshot {
    records: [NativeInputTraceRecord; NATIVE_INPUT_TRACE_CAPACITY],
    len: usize,
    /// Record capacity was exhausted; this sample cannot establish latency acceptance.
    pub overflowed: bool,
    /// Scope order, identifier capacity or raw event timestamp failed; no guessed repair was applied.
    pub invalid_scope: bool,
    /// A scope unwound without explicit completion; Drop restores state but emits no record/output.
    pub aborted_scope: bool,
    /// First arm boundary in the same Mach absolute clock as every native record.
    pub armed_at_mach_ticks: u64,
    /// Mach nanosecond conversion numerator; divide by timebase_denom using wide arithmetic.
    pub timebase_numer: u32,
    /// Nonzero Mach nanosecond conversion denominator.
    pub timebase_denom: u32,
    /// Initial canonical counters identify the measured loaded-document size/generation.
    pub initial_document: NativeInputTraceDocument,
}

impl NativeInputTraceSnapshot {
    /// Iterate populated records without exposing unused array entries or allocating a copy.
    pub fn records(&self) -> &[NativeInputTraceRecord] {
        &self.records[..self.len]
    }

    /// Require finite complete source evidence before any external correlation attempts acceptance.
    pub fn is_valid(&self) -> bool {
        !self.overflowed && !self.invalid_scope && !self.aborted_scope
    }
}

/// Explicit control failures leave current recording/evidence intact for a later safe drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeInputTraceAccessError {
    /// No explicit trace interval has been armed.
    NotArmed,
    /// No current document editor exists.
    NoEditor,
    /// Foreground ownership is temporarily leased or a transition owns it.
    Busy,
    /// Recording/evidence already exists and must be drained before rearming.
    AlreadyArmed,
    /// A synchronous input scope is still active; copying midway would split its provenance.
    ActiveScope,
    /// Arming during composition cannot establish the start of that IME sequence.
    CompositionActive,
    /// This measurement contract requires actual literal Source presentation.
    SourceRequired,
    /// Native clock/timebase acquisition failed.
    ClockUnavailable,
}

/// Owned only by the original current input client and its synchronous scopes,
/// without global history, a document, or a reverse Native reference.
#[cfg(any(target_os = "macos", test))]
pub(crate) type SharedNativeInputTrace = Rc<RefCell<state::NativeInputTrace>>;
