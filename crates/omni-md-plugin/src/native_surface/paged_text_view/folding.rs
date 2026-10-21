//! Existing Source menu/gutter actions change bounded fold anchors through the sole input chain without text/Undo transactions.

use super::*;

/// Resolve menu current-line at queue head; gutter intent holds weak same-Scene authority, retaining no text, reader, or Native graph.
#[derive(Clone)]
pub enum NativeMarkdownPagedSourceFoldAction {
    ToggleCurrent,
    ToggleLine {
        key: Rc<NativeMarkdownPagedSceneKey>,
        line: usize,
    },
    ExpandAll,
}

impl NativeMarkdownPagedTextView {
    /// The existing fold/unfold-current menu submits display intent in arrival order through the existing input barrier.
    pub(in crate::native_surface) fn toggle_current_source_fold(&self) -> Result<()> {
        self.perform_source_fold(NativeMarkdownPagedSourceFoldAction::ToggleCurrent)
    }

    /// Gutter actions use actual canonical lines from published Scene; queuing cannot renew authority across Input/layout publication.
    pub(in crate::native_surface) fn toggle_source_fold_line(
        &self,
        key: NativeMarkdownPagedSceneKey,
        line: usize,
    ) -> Result<()> {
        self.perform_source_fold(NativeMarkdownPagedSourceFoldAction::ToggleLine {
            key: Rc::new(key),
            line,
        })
    }

    /// Existing Expand All removes bounded display anchors while retaining text, save location, and history cursor.
    pub(in crate::native_surface) fn expand_source_folds(&self) -> Result<()> {
        self.perform_source_fold(NativeMarkdownPagedSourceFoldAction::ExpandAll)
    }

    /// Public entry completes actual system composition before capturing current Input, without another folding focus/text client.
    fn perform_source_fold(&self, action: NativeMarkdownPagedSourceFoldAction) -> Result<()> {
        self.commit_composition_before_release()?;
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_link_intent();
        let origin = self.origin()?;
        self.source_fold_at(&origin, action)
    }

    /// Execute the Native menu slot once; interpreted ownership belongs to this controlled queue-head call, not shared depth.
    pub(in crate::native_surface) fn perform_queued_source_fold(
        &self,
        action: NativeMarkdownPagedSourceFoldAction,
    ) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback = PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.cancel_link_intent();
            let origin = self.origin()?;
            if origin.state.marked.is_some() {
                let context = self
                    .ivars()
                    .context
                    .borrow()
                    .clone()
                    .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
                // An earlier queued key may just have started composition; accept displayed text/end system conversion before capturing fold before-state.
                // Old IME buffers must not replay across folding; the gutter key still requires the same-Scene authority check below.
                self.finish_composition_in_context(
                    &origin.input,
                    &origin.delegate,
                    &context,
                    origin.epoch,
                )?;
            }
            let mut origin = self.origin()?;
            origin.interpreted = true;
            self.source_fold_at(&origin, action)
        })
    }

    /// Preserve arrival order, then derive the actual line from this document/selection; the fold receipt retires old display through committed.
    fn source_fold_at(
        &self,
        origin: &Origin,
        action: NativeMarkdownPagedSourceFoldAction,
    ) -> Result<()> {
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if origin.input.is_cell() {
            return Err(NativeMarkdownPagedTextViewError::FoldAction(
                "折叠仅适用于源码正文".into(),
            ));
        }
        if !origin.interpreted && origin.delegate.defer_source_fold(action.clone(), &permit)? {
            return Ok(());
        }
        let outcome = match action {
            NativeMarkdownPagedSourceFoldAction::ExpandAll => {
                origin.input.expand_folds(&permit, &current)
            }
            NativeMarkdownPagedSourceFoldAction::ToggleLine { key, line } => {
                if !origin.delegate.scene_current(&key) {
                    return Err(NativeMarkdownPagedTextViewError::FoldAction("正文布局已改变，请重新点击行号".into()));
                } // Do not guess a new target after edits; the old gutter intent has lost this canonical authority.
                origin.input.toggle_fold(&permit, line, &current)
            }
            NativeMarkdownPagedSourceFoldAction::ToggleCurrent => {
                let original = origin
                    .input
                    .document_identity()
                    .upgrade()
                    .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
                let document = original
                    .try_borrow()
                    .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
                // The original menu locates current line from NSTextView.selectedRange's ordered start, including reversed selection/Select All.
                // A directed head cannot move the action to selection end, nor can unknown direction alone imply multiple ranges.
                let start = origin.state.primary_range.location;
                let line = document.line_for_byte(document.byte_offset_for_utf16(start)?)?;
                drop(document);
                drop(original);
                origin.input.toggle_fold(&permit, line, &current)
            }
        }.map_err(|cause| match cause {
            cause @ (NativeMarkdownPagedInputError::MultipleSelectionInput
                | NativeMarkdownPagedInputError::BodyTargetRequired
                | NativeMarkdownPagedInputError::CompositionActive
                | NativeMarkdownPagedInputError::Fold(omni_markdown_core::markdown_folding::anchor_mapping::MarkdownFoldMappingError::TooManyAnchors)) => {
                NativeMarkdownPagedTextViewError::FoldAction(cause.to_string())
            }
            other => NativeMarkdownPagedTextViewError::Input(other),
        })?;
        self.finish_outcome(origin, outcome)
    }

    /// Report ordinary use-case rejection only after the original chain settles, without revoking healthy text or committed successor input.
    pub(in crate::native_surface) fn reject_source_fold(
        &self,
        cause: NativeMarkdownPagedTextViewError,
    ) {
        crate::native_surface::present_native_markdown_error(&cause.to_string());
        self.failure(cause);
    }
}
