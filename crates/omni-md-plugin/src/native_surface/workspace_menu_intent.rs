//! 正文菜单只消费点击时的原文档与真实正文或格内选择；排队期间的新输入撤销原动作。

use super::*;

/// 菜单绑定点击时的正文owner和实际输入客户，不把后继同位置的格或正文当作原客户。
pub(super) struct NativeMarkdownWorkspacePagedEditIntent {
    owner: usize,
    document: WeakMarkdownDocument,
    body: std::rc::Weak<super::paged_body::NativeMarkdownPagedBody>,
    input: super::paged_input::NativeMarkdownPagedInputState,
    cell: Option<NativeMarkdownWorkspacePagedCellIntent>,
}

/// Weak保留实际草稿owner身份，避免取消后重开同一格时因地址和相同选区复用旧菜单。
struct NativeMarkdownWorkspacePagedCellIntent {
    body: std::rc::Weak<super::paged_body::NativeMarkdownPagedBody>,
    input: super::paged_input::NativeMarkdownPagedInputState,
}
impl NativeMarkdownWorkspacePagedEditIntent {
    /// 原source capture只读Rust身份，先于sender getter；不创建Native Weak或保活Body。
    pub(super) fn capture(controller: &NativeMarkdownController) -> Option<Self> {
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            let editor = surface.editor.as_ref()?;
            if !std::ptr::eq(&*surface.controller, controller)
                || !native_markdown_controller_has_foreground_authority(controller)
                || surface.pending_note_switch.is_some()
                || surface.pending_file_mutation.is_some()
            {
                return None;
            }
            let cell = match editor.body.active_cell_body() {
                Some(body) => {
                    // view()只借现存Rust owner；捕获过程中不创建Native Weak、不调用sender getter。
                    Some(NativeMarkdownWorkspacePagedCellIntent {
                        input: body.view().ok()?.input_state().ok()?,
                        body: Rc::downgrade(&body),
                    })
                }
                None => None,
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

    /// 同名文档重开、同caret重新选择、IME变化或旧owner撤权均不能恢复这次菜单权限。
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
                                (None, None) => true,
                                (Some(original), Some(actual)) => {
                                    std::rc::Weak::ptr_eq(&original.body, &Rc::downgrade(&actual))
                                        && actual.view().is_ok_and(|view| {
                                            view.input_state()
                                                .is_ok_and(|state| state == original.input)
                                        })
                                }
                                _ => false, // 新开、结束或重开草稿都撤销旧菜单，不重新捕获客户。
                            }
                    })
            })
        })
    }

    /// 原格提交只允许更新输入事实；窗口、文稿和分页owner的归属在提交前后保持同一张弱票。
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

    /// 明确的正文命令直接进原Input；public command仍会把已接纳导航后的后继命令排在原队列。
    pub(super) fn perform(&self, action: isize) -> Result<(), String> {
        if !self.current() {
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
            return Err("正文菜单对应的输入已经改变".into());
        }
        // 原字段的历史与八项行内格式都留在原草稿，不能先提交而改掉历史或丢失有向子选区。
        let local = self.cell.is_some() && matches!(action, 22 | 23 | 103..=108 | 113..=114);
        let mut committed = None;
        let target = if local {
            body.active_cell_body().ok_or("原单元格已经关闭")?
        } else {
            // 块格式和插表保持旧交互：结束原格后作用于同一正文；未完成或marked失败直接保留草稿。
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
            // 自己的提交已推进输入代次，只消费该次after；Native重入不得让旧菜单捕获后继caret。
            let current = || {
                ACTIVE_NATIVE_MARKDOWN_SURFACE
                    .with_borrow(|active| active.as_ref().is_some_and(|surface| self.owns(surface)))
                    && body.active_cell_body().is_none()
                    && view.session_is_current()
            };
            match committed.outcome {
                super::paged_input::NativeMarkdownPagedInputOutcome::Committed(after) => {
                    after.check_current(&|_| current()).map_err(|cause| {
                        format!("单元格已提交，后续正文命令因输入改变而取消：{cause}")
                    })?;
                }
                super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(after) => {
                    if !current() || !view.input_state().is_ok_and(|state| state == after) {
                        return Err("单元格已结束，后续正文命令因输入改变而取消".into());
                    }
                }
            }
        } else if !self.current() {
            return Err("原菜单对应的输入已经改变".into());
        }
        match action {
            22 => view.command(sel!(undo:)),
            23 => view.command(sel!(redo:)),
            118 => view.insert_default_table(),
            100..=117 => view.apply_format(native_markdown_format_action(action)?),
            _ => return Err("该动作不是正文编辑命令".into()),
        }
        .map_err(|cause| cause.to_string())
    }
}
