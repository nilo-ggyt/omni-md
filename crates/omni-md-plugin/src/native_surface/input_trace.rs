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
        let Some(editor) = surface.editor.as_ref() else {
            // 正式正文只认当前分页会话；不存在时不能借独立TextKit夹具冒充活动编辑器。
            return Ok(Err(NativeInputTraceAccessError::NoEditor));
        };
        if editor.presentation_mode() != MarkdownPresentationMode::Source {
            // This latency contract measures the actual literal source surface.
            return Ok(Err(NativeInputTraceAccessError::SourceRequired));
        }
        // 原Input在真正arm成功后才拥有固定记录器；没有正文、路径或Native上下文进入payload。
        let result = editor.text_view.arm_input_trace(
            native_input_trace_now(),
            timebase.numer,
            timebase.denom,
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
        let Some(editor) = surface.editor.as_ref() else {
            // Closing a trace's foreground graph discards its scalar metadata without automatic output.
            return Ok(Err(NativeInputTraceAccessError::NoEditor));
        };
        // 未arm与已关闭是不同真实结果；只移交固定标量，不凭空报告正式编辑器不存在。
        Ok(editor.text_view.drain_input_trace())
    })
    .map_err(|_| NativeInputTraceAccessError::Busy)?
}
