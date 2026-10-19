//! Native clock acquisition and explicit foreground controls for the optional scalar input trace.
//!
//! No observer, timer, file, periodic flush or currentEvent guess participates in provenance.

use super::*;
use crate::{NativeInputTraceAccessError, NativeInputTraceSnapshot};

/// The public mach_time.h structure contains the exact rational tick-to-nanosecond conversion.
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

// SAFETY: declarations match public mach/mach_time.h on this macOS-only adapter: a scalar
// uint64 return and a kern_return_t result writing the exact two-uint32 timebase structure.
#[link(name = "System")]
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

/// Read the public monotonic awake clock; retain raw ticks rather than guessing another clock's epoch.
pub(super) fn native_input_trace_now() -> u64 {
    // SAFETY: this public zero-argument function has no caller preconditions and returns one scalar.
    unsafe { mach_absolute_time() }
}

/// Copy O(1) counters without returning any document or mapping lifetime handle.
pub(super) fn native_input_trace_document(
    document: &SharedMarkdownDocument,
) -> NativeInputTraceDocument {
    let document = document.borrow();
    NativeInputTraceDocument {
        generation: document.generation(),
        utf8_bytes: document.byte_len(),
        utf16_units: document.utf16_len(),
    }
}

/// Arm one actual Source editor after fixture/focus/input-source preparation, before real typing.
///
/// No text or path is captured. A prior run must be explicitly drained. The caller's main-thread
/// marker makes native state inspection part of this foreground action, outside global state borrows.
///
/// # Errors
/// Rejects missing/busy editors, non-Source mode, existing composition/evidence or an invalid clock.
pub fn arm_native_markdown_input_trace(
    _mtm: MainThreadMarker,
) -> Result<(), NativeInputTraceAccessError> {
    if BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some() {
        // A reentrant action cannot acquire or split the unique foreground owner's current operation.
        return Err(NativeInputTraceAccessError::Busy);
    }
    if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_none) {
        // An absent/closing surface has no active trace authority to arm or drain.
        return Err(NativeInputTraceAccessError::NoEditor);
    }

    let mut timebase = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: the public function writes exactly this initialized, correctly aligned C structure.
    if unsafe { mach_timebase_info(&mut timebase) } != 0 {
        // An unavailable rational conversion cannot establish the observer's shared timing domain.
        return Err(NativeInputTraceAccessError::ClockUnavailable);
    }
    with_active_native_markdown_surface(|surface| {
        if surface.pending_note_switch.is_some() || surface.pending_file_mutation.is_some() {
            // An accepted handoff owns source identity; trace controls cannot split its evidence.
            return Ok(Err(NativeInputTraceAccessError::Busy));
        }
        let Some(editor) = &surface.editor else {
            // No canonical editor means there is no foreground trace to admit.
            return Ok(Err(NativeInputTraceAccessError::NoEditor));
        };
        if editor.backend.text_storage.presentation_mode() != MarkdownPresentationMode::Source {
            // This latency contract measures the actual literal source surface.
            return Ok(Err(NativeInputTraceAccessError::SourceRequired));
        }
        let (document, marked) = editor.backend.text_view.input_trace_observation();
        let Some(document) = document else {
            // A detached native adapter has no canonical metrics for an explicit run.
            return Ok(Err(NativeInputTraceAccessError::NoEditor));
        };
        let Some(trace) = editor.backend.text_view.input_trace() else {
            // 已封闭的 view 不能重新为关闭会话创建 trace Rc。
            return Ok(Err(NativeInputTraceAccessError::NoEditor));
        };
        let result = trace.borrow_mut().arm(
            native_input_trace_now(),
            timebase.numer,
            timebase.denom,
            document,
            marked,
        );
        Ok(result)
    })
    .map_err(|_| NativeInputTraceAccessError::Busy)?
}

/// Stop recording and return owned scalar records for caller-controlled output after measurement.
///
/// The owner lease and trace-state borrow both end before this function returns. No output, flush,
/// native event, observer or timer is created. Draining during input preserves the existing evidence.
///
/// # Errors
/// Rejects missing/busy editors, an unarmed run or any still-executing native input scope.
pub fn drain_native_markdown_input_trace(
    _mtm: MainThreadMarker,
) -> Result<NativeInputTraceSnapshot, NativeInputTraceAccessError> {
    if BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some() {
        // A reentrant action cannot acquire or split the unique foreground owner's current operation.
        return Err(NativeInputTraceAccessError::Busy);
    }
    if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_none) {
        // An absent/closing surface has no active trace authority to arm or drain.
        return Err(NativeInputTraceAccessError::NoEditor);
    }

    with_active_native_markdown_surface(|surface| {
        if surface.pending_note_switch.is_some() || surface.pending_file_mutation.is_some() {
            // An accepted handoff owns source identity; trace controls cannot split its evidence.
            return Ok(Err(NativeInputTraceAccessError::Busy));
        }
        let Some(editor) = &surface.editor else {
            // Closing a trace's foreground graph discards its scalar metadata without automatic output.
            return Ok(Err(NativeInputTraceAccessError::NoEditor));
        };
        // Move only the fixed scalar snapshot through this lease; it retains no native/document owner.
        let Some(trace) = editor.backend.text_view.input_trace() else {
            // accepted release 已撤销记录所有权，不返回其他会话的证据。
            return Ok(Err(NativeInputTraceAccessError::NoEditor));
        };
        let snapshot = trace.borrow_mut().drain();
        Ok(snapshot)
    })
    .map_err(|_| NativeInputTraceAccessError::Busy)?
}

impl PieceBackedMarkdownTextView {
    /// 只在调用栈内复制记录器，状态借用不跨越原生回调。
    pub(super) fn input_trace(&self) -> Option<SharedNativeInputTrace> {
        self.ivars().input_trace.borrow().clone()
    }

    /// accepted release 取走可选记录器，空壳不保留 opt-in heap。
    pub(super) fn release_input_trace(&self) {
        let trace = self.ivars().input_trace.borrow_mut().take();
        drop(trace);
    }

    /// Query actual superclass marking and canonical counters before borrowing optional trace state.
    pub(super) fn input_trace_observation(&self) -> (Option<NativeInputTraceDocument>, bool) {
        // SAFETY: the live main-thread input subclass asks the matching public superclass getter,
        // avoiding its own semantic override while checking actual native composition state.
        let marked: bool = unsafe { msg_send![super(self), hasMarkedText] };
        let document = self
            .ivars()
            .text_storage
            .load()
            .and_then(|storage| storage.document())
            .map(|document| native_input_trace_document(&document));
        (document, marked)
    }

    /// Begin a selector using stack provenance only; asynchronous IME callbacks keep event identity absent.
    pub(super) fn begin_input_trace_selector(
        &self,
        selector: NativeInputTraceSelector,
    ) -> Option<TraceScope> {
        let ticks = native_input_trace_now();
        let (document, marked) = self.input_trace_observation();
        let trace = self.input_trace()?;
        TraceScope::selector(&trace, selector, ticks, document, marked)
    }

    /// Finish after all standard native input coordination, without writing output from a callback.
    pub(super) fn finish_input_trace_selector(&self, scope: Option<TraceScope>) {
        if let Some(scope) = scope {
            // Query native/canonical values with no trace-state borrow held across those operations.
            let (document, marked) = self.input_trace_observation();
            scope.finish(native_input_trace_now(), document, marked);
        }
    }

    /// Record a sealed native admission rejection even when no storage primitive is called.
    pub(super) fn reject_input_trace(&self, rejection: NativeInputTraceRejection) {
        let (document, _) = self.input_trace_observation();
        let Some(trace) = self.input_trace() else {
            // 已关闭的记录器没有业务所有者，迟到回调保持无副作用。
            return;
        };
        trace
            .borrow_mut()
            .canonical(native_input_trace_now(), document, 0, 0, Some(rejection));
    }
}

impl PieceBackedMarkdownTextStorage {
    /// 只在调用栈内复制记录器，状态借用不跨越原生回调。
    pub(super) fn input_trace(&self) -> Option<SharedNativeInputTrace> {
        self.ivars().input_trace.borrow().clone()
    }

    /// accepted release 取走可选记录器，空壳不保留 opt-in heap。
    pub(super) fn release_input_trace(&self) {
        let trace = self.ivars().input_trace.borrow_mut().take();
        drop(trace);
    }

    /// Record the actual primitive Result with finite counters before notifying reentrant native observers.
    pub(super) fn record_input_trace_result(
        &self,
        removed: usize,
        inserted: usize,
        rejection: Option<NativeInputTraceRejection>,
    ) {
        let document = self
            .document()
            .map(|document| native_input_trace_document(&document));
        let Some(trace) = self.input_trace() else {
            // 已关闭的记录器没有业务所有者，迟到回调保持无副作用。
            return;
        };
        trace.borrow_mut().canonical(
            native_input_trace_now(),
            document,
            removed,
            inserted,
            rejection,
        );
    }
}
