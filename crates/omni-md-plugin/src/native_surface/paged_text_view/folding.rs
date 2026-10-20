//! 原 Source 菜单和行号动作沿唯一输入链改变有限 fold 锚点，不创建文字或 Undo 事务。

use super::*;

/// 菜单到队首才取当前行；行号只持同一 Scene 的弱权限值，不保活正文、Reader 或 Native 图。
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
    /// 原“折叠/展开当前段落”菜单只提交显示意图，菜单到达次序仍由现存输入屏障保留。
    pub(in crate::native_surface) fn toggle_current_source_fold(&self) -> Result<()> {
        self.perform_source_fold(NativeMarkdownPagedSourceFoldAction::ToggleCurrent)
    }

    /// 行号按已发布 Scene 的实际 canonical 行触发；排队后也不能跨输入或布局发布补签。
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

    /// 原“展开全部”撤有限显示锚点，正文、保存位置和历史游标保持原值。
    pub(in crate::native_surface) fn expand_source_folds(&self) -> Result<()> {
        self.perform_source_fold(NativeMarkdownPagedSourceFoldAction::ExpandAll)
    }

    /// 公开入口先结束真实系统组合，再捕唯一当前输入票；没有另一个折叠焦点或文本客户。
    fn perform_source_fold(&self, action: NativeMarkdownPagedSourceFoldAction) -> Result<()> {
        self.commit_composition_before_release()?;
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_link_intent();
        let origin = self.origin()?;
        self.source_fold_at(&origin, action)
    }

    /// 原 Native 菜单槽只执行一次；interpret 标记仅属于这个受控队首调用，不借全局 depth。
    pub(in crate::native_surface) fn perform_queued_source_fold(
        &self,
        action: NativeMarkdownPagedSourceFoldAction,
    ) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback = PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.cancel_link_intent();
            if self.origin()?.state.marked.is_some() {
                self.apply_unmark()?;
            } // 已排在前面的真实 key 可能刚建立组合，沿原队首提交通知结束后再捕折叠 before。
            let mut origin = self.origin()?;
            origin.interpreted = true;
            self.source_fold_at(&origin, action)
        })
    }

    /// 先保到达顺序，再从本步原 Doc/选择求真实行；折叠收据经原 committed 通知使旧图失效。
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
                } // 旧行号不随文字编辑猜新的目标；原用户意图已经失去这张 canonical 权限。
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
                // 旧菜单按 NSTextView.selectedRange 的有序起点定位当前行；反向选择、全选也沿此契约，
                // 不能用有方向 head 把菜单动作移到选择末端或把单个未知方向范围误判为多选。
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

    /// 原链全部结算后才显示普通业务拒绝；它不会撤销健康正文或已提交后继字符。
    pub(in crate::native_surface) fn reject_source_fold(
        &self,
        cause: NativeMarkdownPagedTextViewError,
    ) {
        crate::native_surface::present_native_markdown_error(&cause.to_string());
        self.failure(cause);
    }
}
