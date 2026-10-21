//! Presentation changes replace only this body's layout continuation. The document,
//! input client, history, and Native root remain in the same session.

use super::NativeMarkdownPagedBodyError as Error;
use super::*;
use crate::native_surface::editor::viewport::ModeSourceAnchor;
use omni_markdown_core::MarkdownPresentationMode;

/// Shares ordinary preparation's synchronous admission. Native reentry records deferred
/// work instead of acquiring a second concurrent Producer.
pub(super) struct PresentationUse<'a>(pub(super) &'a Cell<bool>);

impl Drop for PresentationUse<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl NativeMarkdownPagedBody {
    /// A later mode action must first verify actual return of the preceding derived layout.
    /// The shared current document and base fonts are outside that retirement set.
    pub(in crate::native_surface) fn verify_mode_retirement(
        &self,
    ) -> std::result::Result<(), String> {
        let slot = self.mode_retirement.borrow();
        let Some(receipt) = slot.as_ref() else {
            return Ok(()); // Initial attachment retired no Producer; do not invent an already-closed resource.
        };
        if !receipt.source.payload_released() {
            return Err("上一模式的排版 Source 仍未归还".into());
        }
        for reader in receipt
            .reader
            .iter()
            .chain(std::iter::once(&receipt.writer))
        {
            let actual = reader.snapshot();
            if actual.file_open
                || actual.mapped_bytes != 0
                || actual.buffer_bytes != 0
                || actual.cleanup_failed
                || actual.reader_failed
                || !actual.source_payload_released
                || actual.first_source_errno.is_some()
            {
                return Err(format!("上一模式的排版目录仍未归还：{actual:?}"));
            }
        }
        let worker = receipt
            .worker
            .snapshot()
            .ok_or("上一模式的 worker 观察已损坏")?;
        if !worker.is_released() {
            return Err(format!("上一模式的整形 worker 仍未归还：{worker:?}"));
        }
        for cell in &receipt.cells {
            cell.verify_released()?; // Each cell's original receipt verifies its display Source, mappings, and actual directories.
        }
        // A new size or mode can own another Core payload while sharing the same frozen
        // mapping. Recognize only the mapping identity held by actual current fonts.
        // Close empties fonts first, so final verification still requires every mapping back.
        let current_mappings = self
            .fonts
            .borrow()
            .iter()
            .map(|font| font.native().bytes_observer())
            .collect::<Vec<_>>();
        for (bytes, payload) in &receipt.fonts {
            let mapping_retained = current_mappings
                .iter()
                .any(|current| current.same_mapping_resource(bytes));
            if !payload.payload_released()
                || bytes.failed()
                || (!mapping_retained && (!bytes.unmap_succeeded() || bytes.mapped_bytes() != 0))
            {
                return Err("上一模式的样式字体仍持有映射或文件".into());
            }
        }
        Ok(())
    }

    /// Admit a switch only while the original visible glyph and complete Input ticket
    /// remain valid. Font preparation failure leaves the usable old layout intact.
    pub(in crate::native_surface) fn change_presentation_mode(
        &self,
        mode: MarkdownPresentationMode,
        scroll: &NSScrollView,
        anchor: ModeSourceAnchor,
        key: &NativeMarkdownPagedSceneKey,
    ) -> Result<()> {
        self.preflight_release()?;
        self.verify_mode_retirement()
            .map_err(|_| Error::ResourceReleaseFailed)?;
        if self.input.is_cell()
            || self.active_cell.borrow().is_some()
            || self.pending_cell.borrow().is_some()
            || self.has_pending_input()
        {
            return Err(Error::Busy); // Admitted text and cell transactions remain their original owner's obligation; switching cannot cancel them.
        }
        let previous = self.presentation_mode();
        if previous == mode {
            return Ok(());
        }
        let view = self.view()?;
        let original = || {
            self.current_body()
                && self.presentation_mode() == previous
                && anchor.current_document()
                && view
                    .capture_ready_scene_key()
                    .is_ok_and(|actual| actual.same(key))
        };
        if !original() {
            return Err(ViewError::Revoked.into());
        }
        // Subsequent AppKit getters may deliver notifications. Acquire the same
        // synchronous preparation admission before retaining original input authority.
        self.preparing.set(true);
        let preparation = PresentationUse(&self.preparing);
        let result = (|| {
            let current_scroll = view.enclosingScrollView();
            if !original()
                || !current_scroll
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq(actual, scroll))
            {
                return Err(ViewError::Revoked.into());
            }
            drop(current_scroll);
            // Keep the original Scene only during pre-admission font preparation to
            // preserve Source failure across Native callbacks. Release it before
            // retirement so font validation cannot keep the old Reader across modes.
            let scene = self.scene.borrow().clone().ok_or(ViewError::Pending)?;
            if !scene.key().same(key) {
                return Err(ViewError::Revoked.into());
            }
            // Product roles select their actual Native base; generic Body keeps its
            // supplied font. Capture borrows this body's bounded live palette and
            // still verifies each instance's origin, axes, size, and platform advances.
            let captured = self.capture_base_font(mode, &original);
            scene.validate_ready(&original)?;
            let base = captured?;
            let typography = (mode == MarkdownPresentationMode::InlinePreview)
                .then(|| semantics::Typography::new(base.native_font()))
                .transpose();
            scene.validate_ready(&original)?;
            let typography = typography?;
            if !original() {
                return Err(ViewError::Revoked.into());
            }
            // The target wrapper captures its mode's actual line metrics. Shared
            // payload or mapping ownership comes only from the actual capture above.
            let registered = NativeMarkdownLineFont::register(base, &self.catalog, &original);
            scene.validate_ready(&original)?;
            let registered = registered?;
            let registered = if let Some(typography) = &typography {
                let captured =
                    registered.capture_preview_metrics(&typography.metrics_owner, &original);
                scene.validate_ready(&original)?;
                captured?
            } else {
                registered
            };
            drop(scene);
            if !original() {
                return Err(ViewError::Revoked.into());
            }
            let next_serial = self
                .serial
                .get()
                .checked_add(1)
                .ok_or(Error::CounterExhausted)?;
            let next_layout = self
                .layout_epoch
                .get()
                .checked_add(1)
                .ok_or(Error::CounterExhausted)?;
            let mut retired = self.resource_receipts()?;
            // Font zero can retire when its role changes. Retain only old fonts
            // sharing the target's actual Core payload; all other payloads must return.
            // Shared mapping verification separately recognizes current ownership.
            let target_authority = registered.native().core_font().authority();
            retired.fonts = self
                .fonts
                .borrow()
                .iter()
                .filter(|font| !font.native().core_font().permits(&target_authority))
                .map(|font| {
                    (
                        font.native().bytes_observer(),
                        font.native().core_font().resource_observer(),
                    )
                })
                .collect();
            // Admission begins here: revoke old navigation and drawing readers while
            // preserving Input's multiple ranges, affinities, and folds. The mode
            // action already ended actual composition/cell work; it does not replace
            // text, write the document, or append Undo history.
            self.cancel_pending_find()?;
            self.cancel_word_delete()?;
            self.cancel_mode_viewport();
            self.revoke_source_target();
            drop(self.selection_reveal.borrow_mut().take());
            drop(self.pending_vertical.borrow_mut().take());
            drop(self.pending_page_scroll.borrow_mut().take());
            view.prepare_presentation_change()?;
            self.clear_scene()?;
            view.release_scene_readers()?;
            let producer = self.producer.borrow_mut().take();
            let worker = self.worker.borrow_mut().take();
            let cleanup = producer::merge_preparation_cleanup(
                producer.map_or(Ok(()), Producer::retire),
                worker.map_or(Ok(()), FontWorker::close),
            );
            *self.mode_retirement.borrow_mut() = Some(retired);
            if let Err(cause) = cleanup {
                // Do not hide actual retirement failure by creating new layout.
                // The normal close boundary still returns this body's remaining resources.
                self.failed.set(true);
                return Err(cause);
            }
            self.scheduled.set(None);
            self.deferred.set(false);
            self.font_notices.borrow_mut().clear();
            if self.font_notice_failed.get() {
                self.failed.set(true);
                return Err(Error::WorkerFailed);
            }
            self.serial.set(next_serial);
            self.layout_epoch.set(next_layout);
            self.font_choices.borrow_mut().clear();
            let old_fonts = self.fonts.replace(vec![Rc::new(registered)]);
            let old_typography = self.typography.replace(typography);
            self.presentation.set(mode); // The original Producer has retired and cannot accept another mode with coincident Input/width.
            drop((old_fonts, old_typography));
            // Mode structure and old-input position restoration have separate duties.
            // New input may revoke the old anchor, but accepted Preview must still hide
            // the Source ruler, and Source must still mount its line-number attachment.
            let mode_current = || self.current_body() && self.presentation_mode() == mode;
            let input_current = || {
                mode_current()
                    && self
                        .input
                        .state()
                        .is_ok_and(|state| state == key.request.input)
                    && anchor.current_document()
            };
            if !mode_current() {
                return Err(ViewError::Revoked.into());
            }
            let gutter = self.gutter.borrow().clone();
            if let Some(gutter) = gutter {
                // The standard ruler is a bounded attachment to this session.
                // Preview clears/hides its pixels; final close settles the shared page ownership.
                gutter
                    .set_mode_visibility(mode == MarkdownPresentationMode::Source, &mode_current)?;
            } else if mode == MarkdownPresentationMode::Source {
                let lines = self
                    .document
                    .borrow()
                    .as_ref()
                    .expect("模式动作只属于 Body 原文档")
                    .borrow()
                    .line_count();
                let gutter = Rc::new(super::super::gutter::paged::NativeMarkdownPagedGutter::new(
                    self.weak.clone(),
                    scroll,
                    &view,
                    lines,
                    self.mtm,
                )?);
                if !mode_current() {
                    return Err(ViewError::Revoked.into());
                }
                *self.gutter.borrow_mut() = Some(gutter.clone());
                gutter.mount()?; // Install ownership before mounting so synchronous clip notifications see the sole gutter owner.
            }
            if !mode_current() {
                return Err(ViewError::Revoked.into());
            }
            let viewport = self
                .viewport
                .borrow()
                .clone()
                .expect("活动正文已装配原视口");
            viewport.synchronize()?;
            if !mode_current() {
                return Err(ViewError::Revoked.into());
            }
            if !input_current() {
                // New input owns its selection and scroll position. Continue this
                // mode's actual layout without restoring the revoked old anchor.
                return self.schedule();
            }
            // Restore the same Source anchor in the original scroll's actual new
            // lane, without a document-start frame before controller binding.
            // Transfer only its bounded weak document identity and coordinates;
            // the synchronous guard keeps this anchor without copying Scene or text.
            self.restore_mode_viewport(anchor.clone())
        })();
        drop(preparation);
        let deferred = self.deferred.replace(false);
        if (deferred || result.is_err()) && self.current_body() {
            // Pre-admission failure may already have revoked the old Scene, and
            // post-admission Native input may revoke position restoration. Both
            // resume this owner's actual layout without replaying mode or text actions.
            self.schedule()?;
        }
        result
    }
}
