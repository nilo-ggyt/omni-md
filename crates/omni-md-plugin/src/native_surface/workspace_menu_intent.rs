//! Body menus consume the clicked document and actual body/cell selection; intervening input revokes queued actions.

use super::*;

/// Bind to the clicked body owner and actual input client, never a later body or cell occupying the same coordinates.
pub(super) struct NativeMarkdownWorkspacePagedEditIntent {
    owner: usize,
    document: WeakMarkdownDocument,
    body: std::rc::Weak<super::paged_body::NativeMarkdownPagedBody>,
    input: super::paged_input::NativeMarkdownPagedInputState,
    cell: Option<NativeMarkdownWorkspacePagedCellIntent>,
}

/// Preserve the actual draft's weak identity so reopening the same cell/selection cannot reuse an old menu intent.
struct NativeMarkdownWorkspacePagedCellIntent {
    body: std::rc::Weak<super::paged_body::NativeMarkdownPagedBody>,
    input: super::paged_input::NativeMarkdownPagedInputState,
}
impl NativeMarkdownWorkspacePagedEditIntent {
    /// Capture original Rust identity before sender getters, without constructing native weak handles or keeping the body alive.
    pub(super) fn capture(controller: &NativeMarkdownController) -> Option<Self> {
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            let editor = surface.editor.as_ref()?;
            if !std::ptr::eq(&*surface.controller, controller)
                || !native_markdown_controller_has_foreground_authority(controller)
                || surface.pending_note_switch.is_some()
                || surface.pending_file_mutation.is_some()
            {
                // A foreign, closing, or handoff owner grants no menu authority even if an editor is still present.
                return None;
            }
            let cell = match editor.body.active_cell_body() {
                Some(body) => {
                    // view() borrows the existing Rust owner; capture creates no native weak reference and calls no sender getter.
                    Some(NativeMarkdownWorkspacePagedCellIntent {
                        input: body.view().ok()?.input_state().ok()?,
                        body: Rc::downgrade(&body),
                    })
                }
                None => None, // A body-only intent cannot later acquire authority over a newly opened cell draft.
            };
            Some(Self {
                owner: std::ptr::from_ref(controller).addr(),
                document: Rc::downgrade(editor.document()),
                body: Rc::downgrade(&editor.body),
                input: editor.text_view.input_state().ok()?,
                cell,
            })
        })
    }

    /// Reopen, reselection at the same caret, IME changes, or revoked ownership cannot restore this captured menu authority.
    pub(super) fn current(&self) -> bool {
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            active.as_ref().is_some_and(|surface| {
                self.owns(surface)
                    && surface.editor.as_ref().is_some_and(|editor| {
                        editor
                            .text_view
                            .input_state()
                            .is_ok_and(|state| state == self.input)
                            && match (&self.cell, editor.body.active_cell_body()) {
                                (None, None) => true, // Both capture and execution still target the original body client.
                                (Some(original), Some(actual)) => {
                                    // Same cell coordinates are insufficient; require its exact draft owner and complete input state.
                                    std::rc::Weak::ptr_eq(&original.body, &Rc::downgrade(&actual))
                                        && actual.view().is_ok_and(|view| {
                                            view.input_state()
                                                .is_ok_and(|state| state == original.input)
                                        })
                                }
                                _ => false, // Opening, finishing, or replacing a draft revokes the menu without recapturing a client.
                            }
                    })
            })
        })
    }

    /// A cell commit may update input facts only; window, document, and paged-body ownership retain the original weak ticket.
    fn owns(&self, surface: &NativeMarkdownWindowSurface) -> bool {
        self.owner == std::ptr::from_ref(&*surface.controller).addr()
            && native_markdown_controller_has_foreground_authority(&surface.controller)
            && surface.pending_note_switch.is_none()
            && surface.pending_file_mutation.is_none()
            && surface.editor.as_ref().is_some_and(|editor| {
                std::rc::Weak::ptr_eq(&self.document, &Rc::downgrade(editor.document()))
                    && std::rc::Weak::ptr_eq(&self.body, &Rc::downgrade(&editor.body))
            })
    }

    /// Send explicit body commands to original Input; its public command queue still orders work after already accepted navigation.
    pub(super) fn perform(&self, action: isize) -> Result<(), String> {
        if !self.current() {
            // Refuse before acquiring a temporary body owner when the original document or selection has changed.
            return Err("正文菜单对应的笔记或选择已经改变".into());
        }
        let body = ACTIVE_NATIVE_MARKDOWN_SURFACE
            .with_borrow(|active| {
                active
                    .as_ref()?
                    .editor
                    .as_ref()
                    .map(|editor| Rc::clone(&editor.body))
            })
            .ok_or("正文编辑器已经关闭")?;
        if !self.current() {
            // Revalidate the captured input after acquiring the body; a temporary owner cannot bless a replacement client.
            return Err("正文菜单对应的输入已经改变".into());
        }
        // Keep history and six inline formats in the original draft, preserving its undo history and directed subselection.
        let local = self.cell.is_some() && matches!(action, 22 | 23 | 103..=108);
        let mut committed = None;
        let target = if local {
            // Local history/format commands act on the existing cell body without prematurely committing it.
            body.active_cell_body().ok_or("原单元格已经关闭")?
        } else {
            // Block formats/table insertion retain the existing commit-then-body interaction; pending or marked failure preserves the draft.
            if self.cell.is_some() {
                committed = Some(
                    body.commit_active_cell()
                        .map_err(|cause| cause.to_string())?
                        .ok_or("原单元格已经改变，正文命令已取消")?,
                );
            }
            Rc::clone(&body)
        };
        let view = target.view().map_err(|cause| cause.to_string())?;
        if let Some(committed) = committed {
            // This commit advanced input generation; consume its exact after-ticket rather than capturing a successor caret after reentry.
            let current = || {
                ACTIVE_NATIVE_MARKDOWN_SURFACE
                    .with_borrow(|active| active.as_ref().is_some_and(|surface| self.owns(surface)))
                    && body.active_cell_body().is_none()
                    && view.session_is_current()
            };
            match committed.outcome {
                super::paged_input::NativeMarkdownPagedInputOutcome::Committed(after) => {
                    // A changed commit supplies its own exact completion authority; check it against the original owners.
                    after.check_current(&|_| current()).map_err(|cause| {
                        format!("单元格已提交，后续正文命令因输入改变而取消：{cause}")
                    })?;
                }
                super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(after) => {
                    // Even unchanged commit requires the same client and full input state before a body action.
                    if !current() || !view.input_state().is_ok_and(|state| state == after) {
                        return Err("单元格已结束，后续正文命令因输入改变而取消".into());
                    }
                }
            }
        } else if !self.current() {
            // Without this action's commit ticket, only the unchanged original input can authorize execution.
            return Err("原菜单对应的输入已经改变".into());
        }
        match action {
            // Use the real input commands and format policy; unsupported menu tags cannot become arbitrary selectors.
            22 => view.command(sel!(undo:)),
            23 => view.command(sel!(redo:)),
            118 => view.insert_default_table(),
            100..=112 | 115..=116 => view.apply_format(native_markdown_format_action(action)?),
            _ => return Err("该动作不是正文编辑命令".into()),
        }
        .map_err(|cause| cause.to_string())
    }
}
