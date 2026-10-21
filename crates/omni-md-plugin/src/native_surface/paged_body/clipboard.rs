//! Explicit copy spools only actual selections and publishes after separating output from the document;
//! cut deletes through the original input transaction.

use super::*;
use crate::native_surface::text_storage::{MarkdownPasteboardOutput, MarkdownPasteboardWriteError};
use objc2_app_kit::NSPasteboard;

impl NativeMarkdownPagedBody {
    /// System copy uses the general pasteboard and the request captured at the command boundary; getter
    /// reentry cannot change its target.
    pub(super) fn copy_source_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> std::result::Result<(), ViewError> {
        let pasteboard = NSPasteboard::generalPasteboard();
        self.copy_source_selection_to(request, &pasteboard)
    }

    /// Publish to an explicit Native destination; isolated acceptance uses a private pasteboard while
    /// the product supplies its original general destination.
    pub(in crate::native_surface) fn copy_source_selection_to(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pasteboard: &NSPasteboard,
    ) -> std::result::Result<(), ViewError> {
        let current = |epoch| {
            epoch == self.configuration.epoch
                && self.current_body()
                && self
                    .input
                    .target_domain()
                    .is_ok_and(|target| target.same(&request.target))
                && Weak::ptr_eq(&request.document, &self.input.document_identity())
                && self.input.state().is_ok_and(|state| state == request.input)
        };
        let permit = self.input.capture(self.configuration.epoch, &current)?;
        let ranges = self.input.selected_ranges(&permit, &current)?;
        if ranges.iter().all(std::ops::Range::is_empty) {
            // Single or multiple carets select no text; copy/cut leaves existing clipboard contents
            // intact and creates no blank lines.
            return Ok(());
        }
        let source = self.input.capture_layout_source(&permit, &current)?;
        let output = MarkdownPasteboardOutput::prepare_source_ranges(&source, &ranges);
        // Original Source/Cell failure outranks spool failure; validate the original permit without
        // recapturing newer input.
        let checked = self.input.check_permit(&permit, &current);
        drop(source); // Return this read lease first; Native output receives only its independent mmap, retaining neither Cell nor document.
        checked?;
        let output = output.map_err(|cause| match cause {
            MarkdownPasteboardWriteError::DocumentReadUnavailable(cause) => {
                ViewError::Document(cause) // Preserve actual Source read/return causality rather than reporting an empty selection.
            }
            other => ViewError::ClipboardWrite(other.to_string()), // Unpublished file/Native preparation failure remains distinct from the first Source failure.
        })?;
        output
            .publish(pasteboard)
            .map_err(|cause| ViewError::ClipboardWrite(cause.to_string()))?;
        // Publication already caused an external side effect; switching or closing rejects subsequent
        // Cut deletion without recapturing or replaying the old action.
        self.input.check_permit(&permit, &current)?;
        Ok(())
    }
}
