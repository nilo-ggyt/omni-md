//! Projects current canonical aggregates and actual save outcomes; feedback uses the original body geometry.

use super::*;
use omni_markdown_core::MarkdownWorkspaceError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) struct NativeMarkdownFooterDocument {
    dirty: bool,
    characters: usize,
    lines: usize,
}
impl NativeMarkdownFooterDocument {
    /// Reads root aggregates only; callers end the document borrow before any Native setter.
    pub(in crate::native_surface) fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self {
            dirty: document.is_dirty(),
            characters: document.character_count(),
            lines: document.line_count(),
        }
    }
    pub(super) fn is_dirty(self) -> bool {
        // This projected scalar grants no disk/save authority and performs no
        // document read while a Native caller is holding the footer snapshot.
        self.dirty
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum NativeMarkdownFooterSaveResult {
    #[default]
    Current,
    Conflict,
    Unconfirmed,
}
impl NativeMarkdownFooterSaveResult {
    fn after_save(self, error: Option<&MarkdownWorkspaceError>) -> Self {
        // Preserve the outcome of the actual save boundary. A later unrelated
        // failure cannot silently resolve an already observed external conflict.
        match error {
            None => Self::Current, // Only an actual successful save or a newly installed document clears an old result.
            Some(MarkdownWorkspaceError::SaveConflict) => Self::Conflict,
            // A failed read, busy input, or unconfirmed directory sync does not resolve an earlier
            // conflict. Temporary feedback shows the new error; the footer keeps the actual conflict.
            Some(_) if self == Self::Conflict => self,
            Some(_) => Self::Unconfirmed, // Includes post-rename sync failure; do not claim no write or confirmed durability.
        }
    }
    /// Projects observed save facts without presenting unconfirmed directory durability as an absent disk write.
    fn resolution_message(
        self,
        document: Option<NativeMarkdownFooterDocument>,
    ) -> Option<&'static str> {
        document?;
        (self == Self::Conflict).then_some("检测到磁盘版本变化，本地草稿已保留。请选择处理方式。")
    }

    fn label(self, document: Option<NativeMarkdownFooterDocument>) -> &'static str {
        let Some(document) = document else {
            // Empty presentation cannot reuse the previous note's save claim.
            return "未打开笔记";
        };
        match self {
            Self::Conflict => "保存已暂停 · 等待处理冲突", // A clean model may still conflict with an external disk revision.
            Self::Unconfirmed => "保存未确认 · 草稿已保留",
            Self::Current if document.dirty => "本地修改待保存",
            Self::Current => "已保存",
        }
    }
}

/// Weak references to this note's actual scroll and Find controls, without
/// another body or layout owner.
#[derive(Clone)]
pub(super) struct NativeMarkdownDocumentAreaBinding {
    scroll: Weak<NSScrollView>,
    find: std::rc::Weak<NativeMarkdownFindBar>,
}

impl NativeMarkdownController {
    /// Confirm copy through the existing save column without changing body height,
    /// selection, or error feedback. Actual edit/mode/save notifications replace
    /// this temporary projection naturally; no timer owns a second outcome.
    pub(super) fn present_copy_confirmation(&self) {
        let document = self.ivars().footer_document.get();
        let save = self.ivars().footer_save_result.get();
        let mode = self.ivars().published_presentation_mode.get();
        let current = || {
            document.is_some()
                && !self.ivars().close_finalizer_scheduled.get()
                && native_markdown_controller_has_foreground_authority(self)
                && self.ivars().footer_document.get() == document
                && self.ivars().footer_save_result.get() == save
                && self.ivars().published_presentation_mode.get() == mode
        };
        if !current() {
            return;
        }
        let Some(labels) = self.ivars().footer_labels.get() else {
            return; // Closed or incomplete construction cannot rebuild controls for feedback.
        };
        // Keep the real save result first. Copying neither certifies persistence nor clears an
        // observed disk conflict or uncertain post-publication durability.
        let message = NSString::from_str(&format!("{} · 已复制全文", save.label(document)));
        labels[1].setStringValue(&message);
        if current() {
            labels[1].setToolTip(Some(&message));
        } // Setter reentry into another note revokes the old tooltip publication.
    }

    /// A new document or empty state retires the previous note's save result.
    /// Cached display scalars never grant file-operation authority.
    pub(in crate::native_surface) fn install_footer_document(
        &self,
        document: Option<NativeMarkdownFooterDocument>,
    ) {
        self.ivars()
            .footer_save_result
            .set(NativeMarkdownFooterSaveResult::Current);
        if document.is_none() {
            self.clear_conflict_bar();
        }
        self.refresh_footer_document(document);
    }

    /// Actual edit/undo/redo completion refreshes aggregates. Undoing to clean,
    /// copying, or clearing feedback cannot erase an unresolved disk conflict.
    pub(in crate::native_surface) fn refresh_footer_document(
        &self,
        document: Option<NativeMarkdownFooterDocument>,
    ) {
        self.ivars().footer_document.set(document);
        self.render_footer();
    }

    /// Capture the typed Workspace result before presentation converts it to text,
    /// retaining the distinction between uncertain durability and no disk write.
    pub(in crate::native_surface) fn finish_footer_save(
        &self,
        document: NativeMarkdownFooterDocument,
        error: Option<&MarkdownWorkspaceError>,
    ) {
        let previous = self.ivars().footer_save_result.get();
        self.ivars()
            .footer_save_result
            .set(previous.after_save(error));
        self.refresh_footer_document(Some(document));
    }

    pub(super) fn render_footer(&self) {
        if self.ivars().close_finalizer_scheduled.get() {
            // Accepted teardown must not republish values into controls awaiting release.
            return;
        }
        let mode = self.ivars().published_presentation_mode.get();
        let document = self.ivars().footer_document.get();
        let save = self.ivars().footer_save_result.get();
        let current = || {
            // Every Native setter below may reenter. Only these exact original
            // mode/document/save scalars authorize the remaining projection.
            !self.ivars().close_finalizer_scheduled.get()
                && self.ivars().published_presentation_mode.get() == mode
                && self.ivars().footer_document.get() == document
                && self.ivars().footer_save_result.get() == save
        };
        let mode = match mode {
            MarkdownPresentationMode::Source => "源码模式",
            MarkdownPresentationMode::InlinePreview => "行内预览",
        };
        let counts = document
            .map(|value| format!("{} 字符 · {} 行", value.characters, value.lines))
            .unwrap_or_default();
        let Some(labels) = self.ivars().footer_labels.get() else {
            return; // Construction installs the complete footer before publishing a document.
        };
        for (label, text) in labels
            .iter()
            .zip([mode, save.label(document), counts.as_str()])
        {
            if !current() {
                return;
            } // A reentrant document change stops all remaining old display values.
            let text = NSString::from_str(text);
            label.setStringValue(&text);
            if !current() {
                return;
            }
            label.setToolTip(Some(&text));
        }
        if !current() {
            return;
        }
        if let Some(bar) = self.ivars().conflict_bar.get() {
            // The conflict strip consumes the same actual save result; it cannot
            // infer another version or independently certify disk persistence.
            bar.publish(save.resolution_message(document), &current);
        }
        if current() {
            self.layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
        }
    }

    /// Clear all three business values during teardown so an empty retained
    /// controller cannot keep the old note's projected state.
    pub(super) fn clear_footer(&self) {
        self.clear_conflict_bar();
        self.ivars().footer_document.set(None);
        self.ivars()
            .footer_save_result
            .set(NativeMarkdownFooterSaveResult::Current);
        if let Some(labels) = self.ivars().footer_labels.get() {
            for label in labels {
                label.setStringValue(ns_string!(""));
                label.setToolTip(None);
            }
        } // Failed construction still revokes layout references even without installed labels.
        self.unbind_document_area();
    }

    /// Feedback visibility consumes only its actual message row; hiding it returns
    /// the space without shifting or retaining a blank area above the footer.
    pub(super) fn set_feedback_visible(&self, visible: bool) {
        if visible && self.ivars().close_finalizer_scheduled.get() {
            return;
        }
        self.ivars().feedback_visible.set(visible);
        if let Some(label) = self.ivars().status_label.get() {
            label.setHidden(!visible);
        }
        // Construction failure with no feedback control still clears scalar
        // state, without allocating a substitute or bypassing the original layout owner.
        self.layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
    }
    pub(in crate::native_surface) fn feedback_visible(&self) -> bool {
        // A scalar request alone cannot reserve a row if no actual label was installed.
        self.ivars().feedback_visible.get() && self.ivars().status_label.get().is_some()
    }

    /// Bind only after mounting the actual scroll. Note replacement synchronously
    /// retires old weak references; Find/feedback cannot reconstruct the body.
    pub(in crate::native_surface) fn bind_document_area(&self, scroll: &NSScrollView) {
        *self.ivars().document_area.borrow_mut() = Some(NativeMarkdownDocumentAreaBinding {
            scroll: Weak::from(scroll),
            find: std::rc::Weak::new(),
        });
        let _ = self.next_document_area_revision();
    }
    pub(in crate::native_surface) fn unbind_document_area(&self) {
        // Revoke synchronous layout authority before the old graph is detached.
        self.ivars().document_area.borrow_mut().take();
        let _ = self.next_document_area_revision();
    }
    pub(in crate::native_surface) fn bind_find_bar(
        &self,
        scroll: &NSScrollView,
        find: &Rc<NativeMarkdownFindBar>,
    ) -> Result<(), String> {
        let Some(binding) = self.ivars().document_area.borrow().clone() else {
            return Ok(()); // Independent embedding owns its supplied parent geometry directly.
        };
        let actual = binding.scroll.load().ok_or("笔记正文已经关闭")?;
        if !std::ptr::eq(&*actual, scroll) {
            return Err("查找栏不属于当前笔记正文".into());
        }
        // End the Native weak upgrade and identity check before changing the
        // binding. No AppKit call crosses this state publication.
        drop(actual);
        *self.ivars().document_area.borrow_mut() = Some(NativeMarkdownDocumentAreaBinding {
            scroll: binding.scroll,
            find: Rc::downgrade(find),
        });
        let _ = self.next_document_area_revision();
        Ok(())
    }
    /// The actual window reserves its existing chrome; independent Find embedding
    /// uses exactly the supplied parent. Both use the same absolute allocation
    /// algorithm without guessing or rebuilding a missing required owner.
    pub(in crate::native_surface) fn document_area_base(
        &self,
        scroll: &NSScrollView,
        parent: NSRect,
    ) -> Result<NSRect, String> {
        let binding = self.ivars().document_area.borrow().clone();
        match binding {
            Some(binding) => {
                let actual = binding.scroll.load().ok_or("原笔记正文已经关闭")?;
                if !std::ptr::eq(&*actual, scroll) {
                    return Err("查找栏不属于当前笔记正文".into());
                }
                let base = super::super::window::native_markdown_editor_base_frame(parent.size);
                self.ivars()
                    .conflict_bar
                    .get()
                    .expect("Window正文布局已装配原冲突条")
                    .reserve_area(base)
            }
            None => Ok(parent), // Independent embedding has no window chrome to subtract.
        }
    }

    pub(in crate::native_surface) fn document_area_layout_revision(&self) -> u64 {
        // A finite revision invalidates synchronous geometry, without retaining
        // Native controls or granting any source/document authority.
        self.ivars().document_area_revision.get()
    }

    fn next_document_area_revision(&self) -> Option<u64> {
        // Exhaustion refuses new layout authority rather than wrapping into an
        // old publication that a reentrant stack may still hold.
        let revision = self.ivars().document_area_revision.get().checked_add(1)?;
        self.ivars().document_area_revision.set(revision);
        Some(revision)
    }

    /// Resize and feedback share one absolute layout; never repeatedly subtract
    /// Find or message heights from the scroll's previous frame.
    pub(in crate::native_surface) fn layout_document_area(
        &self,
        authority: NativeMarkdownHeaderAuthority,
    ) -> bool {
        let foreground = matches!(authority, NativeMarkdownHeaderAuthority::Foreground);
        if self.ivars().close_finalizer_scheduled.get()
            || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
            || (foreground && !native_markdown_controller_has_foreground_authority(self))
        {
            return false;
        }
        let Some(revision) = self.next_document_area_revision() else {
            return false;
        };
        let Some(binding) = self.ivars().document_area.borrow().clone() else {
            return true;
        }; // An empty document owns no scroll requiring layout.
        let Some(scroll) = binding.scroll.load() else {
            return false;
        };
        let current = || {
            self.ivars().document_area_revision.get() == revision
                && !self.ivars().close_finalizer_scheduled.get()
                && VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_none()
                && (!foreground || native_markdown_controller_has_foreground_authority(self))
        };
        if !current() {
            return false;
        }
        // SAFETY: Read only the original scroll's parent. The local retained
        // reference is never published to a controller or successor note.
        let Some(parent) = (unsafe { scroll.superview() }) else {
            return false;
        };
        if !current() {
            return false;
        }
        let bounds = parent.bounds();
        let size = bounds.size;
        if !current() {
            return false;
        }
        let find = binding.find.upgrade();
        // Measure the original Find's occupied height, then derive all frames
        // from this one parent size and the same current conflict reservation.
        let height = find.as_ref().map_or(0.0, |find| find.occupied_height());
        let full = super::super::window::native_markdown_editor_base_frame(size);
        let Ok(base) = self.document_area_base(&scroll, bounds) else {
            return false;
        };
        if !current() {
            return false;
        }
        let Some(frames) =
            super::super::header_layout::editor_area_frames(base, self.feedback_visible(), height)
        else {
            return false;
        };
        scroll.setFrame(NSRect::new(
            NSPoint::new(frames.scroll.x, frames.scroll.y),
            NSSize::new(frames.scroll.width, frames.scroll.height),
        ));
        if !current() {
            return false;
        }
        if let (Some(find), Some(frame)) = (find, frames.find)
            && !find.position_in_document_area(
                NSRect::new(
                    NSPoint::new(frame.x, frame.y),
                    NSSize::new(frame.width, frame.height),
                ),
                self,
            )
        {
            return false;
        }
        if !current() {
            return false;
        }
        if let Some(bar) = self.ivars().conflict_bar.get() {
            // Position the conflict strip at the same computed content boundary,
            // preserving Find/body/feedback order without another height formula.
            let content_top = frames
                .find
                .as_ref()
                .map_or(frames.scroll.y + frames.scroll.height, |frame| {
                    frame.y + frame.height
                });
            bar.position(&parent, full, base, content_top, &current);
        }
        if !current() {
            return false;
        }
        // The information card overlays this actual body region using the same
        // geometry; it neither squeezes document text nor creates another window.
        self.position_information_panel(&parent, scroll.frame(), &current);
        current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    /// Actual file-backed edits and undo supply root aggregates and dirty state without another body string.
    fn canonical_edit_and_undo_supply_the_footer_counts_and_dirty_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("footer.md");
        std::fs::write(&path, "A😀\r\n中").unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 4096).unwrap();
        let clean = NativeMarkdownFooterDocument::capture(&document);
        assert_eq!((clean.characters, clean.lines, clean.dirty), (4, 2, false));
        document
            .replace_utf16_range(0..1, "ab", MarkdownTextSelection::caret(2))
            .unwrap();
        let edited = NativeMarkdownFooterDocument::capture(&document);
        assert_eq!(
            (edited.characters, edited.lines, edited.dirty),
            (5, 2, true)
        );
        document.undo().unwrap();
        assert_eq!(NativeMarkdownFooterDocument::capture(&document), clean);
    }
}
