//! Production paged input retains only bounded trace payload; observation reads no text and owns no document/Native resource.

use super::*;
use crate::native_input_trace::{NativeInputTrace, SharedNativeInputTrace};
use crate::native_surface::input_trace::native_input_trace_now;
use crate::{NativeInputTraceAccessError, NativeInputTraceDocument, NativeInputTraceRejection};

impl NativeMarkdownPagedInput {
    /// Arm only the current Source client; composition, closure, or a busy input cannot start another evidence interval.
    pub(in crate::native_surface) fn arm_input_trace(
        &self,
        ticks: u64,
        numer: u32,
        denom: u32,
    ) -> std::result::Result<(), NativeInputTraceAccessError> {
        if self.closed.get() {
            return Err(NativeInputTraceAccessError::NoEditor);
        }
        if self.busy.get() {
            return Err(NativeInputTraceAccessError::Busy);
        }
        if self.is_cell() {
            return Err(NativeInputTraceAccessError::SourceRequired);
        } // Cell versions differ from canonical versions; do not mix local coordinates into the Source trace.
        let state = self.state().map_err(|cause| match cause {
            NativeMarkdownPagedInputError::Closed
            | NativeMarkdownPagedInputError::DocumentReleased => {
                NativeInputTraceAccessError::NoEditor
            }
            _ => NativeInputTraceAccessError::Busy,
        })?;
        let (document, _) = self.input_trace_observation();
        let document = document.ok_or(NativeInputTraceAccessError::Busy)?;
        let previous = self.input_trace();
        let trace = previous
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(NativeInputTrace::default())));
        trace
            .borrow_mut()
            .arm(ticks, numer, denom, document, state.marked.is_some())?;
        if previous.is_none() {
            // A failed arm leaves no new retained allocation; on success Input owns it and View/scopes borrow only synchronously.
            self.input_trace.replace(Some(trace));
        }
        Ok(())
    }

    /// Clone the bounded recorder handle without crossing Native/document borrows; unarmed/closed hot paths return None.
    pub(in crate::native_surface) fn input_trace(&self) -> Option<SharedNativeInputTrace> {
        self.input_trace.borrow().clone()
    }

    /// A live unarmed Input reports NotArmed; only closed Input whose payload was removed reports NoEditor.
    pub(in crate::native_surface) fn drain_input_trace(
        &self,
    ) -> std::result::Result<crate::NativeInputTraceSnapshot, NativeInputTraceAccessError> {
        if self.closed.get() {
            return Err(NativeInputTraceAccessError::NoEditor);
        }
        let trace = self
            .input_trace()
            .ok_or(NativeInputTraceAccessError::NotArmed)?;
        let snapshot = trace.borrow_mut().drain()?;
        Ok(snapshot)
    }

    /// Observe NSTextInputClient's actual marked fact and O(1) canonical totals, without Source reads or external callbacks.
    pub(in crate::native_surface) fn input_trace_observation(
        &self,
    ) -> (Option<NativeInputTraceDocument>, bool) {
        let document = self.document_identity().upgrade().and_then(|original| {
            let document = original.try_borrow().ok()?;
            Some(NativeInputTraceDocument {
                generation: document.generation(),
                utf8_bytes: document.byte_len(),
                utf16_units: document.utf16_len(),
            })
        });
        (document, self.marked.get().is_some())
    }

    /// Record Core's typed success before reentrant presentation effects; display failure cannot rewrite canonical success as rejection.
    pub(super) fn record_input_trace_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
        document: &MarkdownDocumentBuffer,
    ) {
        let Some(trace) = self.input_trace() else {
            return;
        };
        let removed = commit
            .invalidation
            .replaced_utf16_range
            .as_ref()
            .map_or(0, Range::len);
        let inserted = removed
            .checked_add_signed(commit.invalidation.change_in_utf16_length)
            .expect("原Core编辑回执的移除量和长度差必须组成实际新增UTF16量");
        // publish already owns the document borrow; copy three totals without reborrowing or triggering Source/Native callbacks.
        let document = NativeInputTraceDocument {
            generation: document.generation(),
            utf8_bytes: document.byte_len(),
            utf16_units: document.utf16_len(),
        };
        let ticks = native_input_trace_now();
        trace
            .borrow_mut()
            .canonical(ticks, Some(document), removed, inserted, None);
    }

    /// Record only the callback's bounded rejection category, with no error string, text, path, or extra Source query.
    pub(in crate::native_surface) fn reject_input_trace(
        &self,
        rejection: NativeInputTraceRejection,
    ) {
        let Some(trace) = self.input_trace() else {
            return;
        };
        let (document, _) = self.input_trace_observation();
        let ticks = native_input_trace_now();
        trace
            .borrow_mut()
            .canonical(ticks, document, 0, 0, Some(rejection));
    }
}
