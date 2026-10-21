//! Existing file Paste/drop uses the original input queue; confined import's provisional transaction still confirms file ownership.

use objc2::{DefinedClass, runtime::ProtocolObject};
use objc2_app_kit::{NSDragOperation, NSDraggingInfo, NSPasteboardTypeString};
use omni_markdown_core::MarkdownTextSelection;

use super::{NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError as Error, Result};
use crate::native_surface::workspace_actions::{
    insert_native_markdown_paged_attachments, native_markdown_pasteboard_file_paths,
};
use crate::payload_use::PayloadUseGuard;

impl NativeMarkdownPagedTextView {
    /// Original Input facts captured on path arrival must hold; Body admits this bounded payload without copying/opening external files yet.
    pub(in crate::native_surface) fn queue_attachment_paths(
        &self,
        paths: Vec<std::path::PathBuf>,
        drop: Option<(u64, usize)>,
        before: super::NativeMarkdownPagedInputState,
    ) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = self.origin()?;
        if origin.state != before {
            return Err(Error::Revoked); // Native path reads delivered successor input; an old file request cannot acquire the newer selection.
        }
        if origin.input.is_cell() {
            return Err(Error::UnsupportedCommand);
        }
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if !origin.delegate.defer_attachments(paths, drop, &permit)? {
            return Err(Error::UnsupportedCommand);
        }
        Ok(())
    }

    /// Drag entry checks bounded types/paths only, without file copy, selection changes, or early composition confirmation.
    pub(super) fn accepted_drag_operation(
        &self,
        sender: &ProtocolObject<dyn NSDraggingInfo>,
    ) -> NSDragOperation {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let accepted = (|| {
            let origin = self.origin()?;
            if origin.input.is_cell() {
                return Ok(false); // Cell cannot borrow Body attachment semantics for undefined table escaping or external file effects.
            }
            if !origin
                .step(self, || sender.draggingSourceOperationMask())?
                .contains(NSDragOperation::Copy)
            {
                return Ok(false); // This target copies external input only; a source without Copy permission cannot import through Move/Link.
            }
            let board = origin.step(self, || sender.draggingPasteboard())?;
            let paths = origin
                .step(self, || native_markdown_pasteboard_file_paths(&board))?
                .map_err(Error::AttachmentImport)?;
            if !paths.is_empty() {
                return Ok(true);
            }
            // SAFETY: The public string-type constant only queries this drag pasteboard's formats, without reading/materializing text.
            let types = objc2_foundation::NSArray::from_slice(&[unsafe { NSPasteboardTypeString }]);
            Ok(origin
                .step(self, || board.availableTypeFromArray(&types))?
                .is_some())
        })();
        match accepted {
            Ok(true) => NSDragOperation::Copy,
            Ok(false) => NSDragOperation::None,
            Err(cause) => {
                self.report_attachment_rejection(&cause);
                self.failure(cause); // Reject the whole over-budget batch through existing feedback rather than silently accept a prefix.
                NSDragOperation::None
            }
        }
    }

    /// Actual drop uses the existing Scene's measured hit; missing Ready rejects without whole-document reads or guessed line heights.
    pub(super) fn accept_drop(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_link_intent();
        let origin = self.origin()?;
        if origin.state.marked.is_some() {
            let context = self
                .ivars()
                .context
                .borrow()
                .clone()
                .ok_or(Error::ContextUnavailable)?;
            // Drop accepts displayed composition and ends original conversion before old IME buffers could submit again after attachments.
            // Callbacks still validate the original client; no external file was copied, and Scene hits are acquired after composition completion.
            self.finish_composition_in_context(
                &origin.input,
                &origin.delegate,
                &context,
                origin.epoch,
            )?;
        }
        let scene = self.scene()?;
        if scene.origin.input.is_cell() {
            return Err(Error::UnsupportedCommand);
        }
        if !scene
            .step(self, || sender.draggingSourceOperationMask())?
            .contains(NSDragOperation::Copy)
        {
            return Err(Error::UnsupportedCommand); // Recheck actual source-operation permission at drop; earlier enter cannot authorize a successor mask.
        }
        let board = scene.step(self, || sender.draggingPasteboard())?;
        let point = scene.step(self, || sender.draggingLocation())?;
        let local = scene.step(self, || self.convertPoint_fromView(point, None))?;
        let position = scene.scene.hit(local)?;
        if !scene.current(self) {
            return Err(Error::Revoked);
        }
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        let paths = scene
            .step(self, || native_markdown_pasteboard_file_paths(&board))?
            .map_err(Error::AttachmentImport)?;
        if !paths.is_empty() {
            return self.queue_attachment_paths(
                paths,
                Some((scene.origin.state.generation, position)),
                scene.origin.state,
            ); // Admit the original file action only; file ownership, text, and Undo commit at the actual queue head.
        }
        if scene.origin.delegate.has_native_input_barrier() {
            return Err(Error::Busy); // An unfinished navigation cannot be overwritten by old pointer selection, nor may the drag pasteboard become general clipboard.
        }
        let text = scene
            .step(self, || {
                // SAFETY: Read this actual drag pasteboard's public plain-text type only; the sole Input performs bounded conversion.
                board.stringForType(unsafe { NSPasteboardTypeString })
            })?
            .ok_or(Error::ClipboardUnavailable)?;
        let commit = scene.origin.input.insert_native_isolated(
            &permit,
            &text,
            objc2_foundation::NSRange::new(position, 0),
            &current,
        )?;
        self.finish_edit_commit(&scene.origin, commit)
    }

    /// The sole queue head continues the preceding actual receipt; external drop uses its original physical hit only if canonical generation is unchanged.
    pub(in crate::native_surface) fn perform_queued_attachments(
        &self,
        paths: &[std::path::PathBuf],
        drop: Option<(u64, usize)>,
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
                    .ok_or(Error::ContextUnavailable)?;
                // Earlier queued input may have started composition; accept only that client's displayed text and end system conversion.
                // Do not renew interpretation authority; recheck original drop generation/physical hit strictly after completion.
                self.finish_composition_in_context(
                    &origin.input,
                    &origin.delegate,
                    &context,
                    origin.epoch,
                )?;
            }
            if let Some((generation, position)) = drop {
                let mut origin = self.origin()?;
                origin.interpreted = true;
                if origin.state.generation != generation {
                    return Err(Error::AttachmentImport(
                        "拖入前原文稿已改变，请重新拖入".into(),
                    )); // The original hit is invalid; reject the file action while retaining healthy document state.
                }
                let current = |epoch| epoch == origin.epoch && origin.current(self);
                let permit = origin.input.capture(origin.epoch, &current)?;
                let outcome = origin.input.set_selection_intent(
                    &permit,
                    MarkdownTextSelection {
                        anchor_utf16: position,
                        head_utf16: position,
                    },
                    None,
                    None,
                    &current,
                )?;
                self.finish_outcome(&origin, outcome)?;
            }
            insert_native_markdown_paged_attachments(self, paths)
        })
    }
}
