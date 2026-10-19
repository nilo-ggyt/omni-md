//! 标准选择动作沿同一原输入票计算、发布和安装；不为 Native 回调补签较新的正文状态。

use super::*;
use objc2_app_kit::{NSTextSelectionNavigationDestination, NSTextSelectionNavigationDirection};

/// 系统已经解释好的语义动作共用一个权限和发布入口，不在这里重新绑定物理按键。
#[derive(Clone, Copy)]
enum SelectionAction {
    Motion(NativeMarkdownSelectionMotion),
    Expansion(NativeMarkdownSelectionExpansion),
}

/// 只有同一原动作仍有效时才把标记文本导航交回 IME；返回途中析构不能给旧动作续权。
pub(super) struct NativeMarkdownSelectionImeForward {
    capture: NativeMarkdownSelectionCapture,
    input_epoch: u64,
}

impl NativeMarkdownSelectionImeForward {
    pub(super) fn current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        self.capture.current()
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == self.input_epoch
    }
}

impl PieceBackedMarkdownTextView {
    /// 标准方向动作只改变选择；输入法仍拥有其标记文本中的导航，不接管候选交互。
    pub(super) fn run_semantic_selection_motion(
        &self,
        motion: NativeMarkdownSelectionMotion,
    ) -> Option<NativeMarkdownSelectionImeForward> {
        self.run_semantic_selection_action(SelectionAction::Motion(motion))
    }

    /// 标准全选、选词和选行沿同一状态入口执行；完整范围由明确动作或原图导航给出。
    pub(super) fn run_semantic_selection_expansion(
        &self,
        expansion: NativeMarkdownSelectionExpansion,
    ) -> Option<NativeMarkdownSelectionImeForward> {
        self.run_semantic_selection_action(SelectionAction::Expansion(expansion))
    }

    fn run_semantic_selection_action(
        &self,
        action: SelectionAction,
    ) -> Option<NativeMarkdownSelectionImeForward> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            return None; // 关闭后的系统空壳不能重建选择或延后任务。
        }
        self.ivars().folds.invalidate_native_install();
        let initial_capture = self.ivars().folds.capture_selection().ok()?;
        let input_epoch = self.ivars().table_input_intent_epoch.get().checked_add(1)?;
        self.cancel_table_continuity_for_new_input();
        let initial = || {
            initial_capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch
        };
        let Ok(Some(storage)) =
            native_markdown_table_intent_step(&initial, || self.ivars().text_storage.load())
        else {
            return None;
        };
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super());
        let Ok(marked) = native_markdown_table_intent_step(&initial, || input.hasMarkedText())
        else {
            return None;
        };
        if marked {
            return Some(NativeMarkdownSelectionImeForward {
                capture: initial_capture,
                input_epoch,
            }); // 不读取或重写 IME 标记范围；super 只在返回后的原票仍有效时执行一次。
        }
        // 文档首尾是源坐标的绝对边界，与旧 caret 的软换行侧无关。普通首尾动作和全选
        // 自行明确新方向；扩选仍须保留原 anchor，不能借绝对目标猜测未知选择的活动端。
        let document_end = match action {
            SelectionAction::Motion(motion)
                if motion.destination == NSTextSelectionNavigationDestination::Document =>
            {
                match motion.direction {
                    NSTextSelectionNavigationDirection::Backward => Some(false),
                    NSTextSelectionNavigationDirection::Forward => Some(true),
                    _ => None, // 视觉方向不冒充源文档首尾，仍交原图计算。
                }
            }
            _ => None, // 词、行、段落和相对移动依旧需要实际布局。
        };
        let replaces_direction = match action {
            SelectionAction::Expansion(expansion) => !expansion.requires_known_head(),
            SelectionAction::Motion(motion) => document_end.is_some() && !motion.extending,
        };
        let captured = if replaces_direction {
            // 明确的新方向允许替换未知或多范围；旧 Native 多选不能冒充单 before。
            let Ok(Ok(native_before)) = native_markdown_table_intent_step(&initial, || {
                self.native_ordered_selection_state_for_capture(&initial_capture)
            }) else {
                return None;
            };
            let document = storage.document()?;
            let before = document.borrow().selection();
            let known = self.ivars().folds.known_selection() == Some(before);
            let captured = initial_capture.clone().finish(before, known).ok()?;
            drop(document);
            match native_before {
                NativeMarkdownOrderedSelection::Single(range) => {
                    captured.with_observed_native_before(range).ok()?
                }
                NativeMarkdownOrderedSelection::Multiple => captured,
            }
        } else {
            // 相对动作必须知道原活动端；未知非空方向不能成为折叠或历史里的猜测 head。
            let Ok(Ok(captured)) =
                native_markdown_table_intent_step(&initial, || self.primary_text_selection())
            else {
                return None;
            };
            captured
        };
        let document = storage.document()?;
        if !captured.belongs_to(&document) || !captured.current() {
            return None;
        }
        let identity = Rc::downgrade(&document);
        let (generation, length) = {
            let document = document.borrow();
            (document.generation(), document.utf16_len())
        };
        drop(document); // 原动作只保 Weak 与标量；Native 导航期间不另保正文 root。
        let width = storage.ivars().table_projection_width_key.get();
        let geometry_epoch = storage.ivars().table_geometry_epoch.get();
        let mode = storage.presentation_mode();
        let current = || {
            captured.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch
                && storage.ivars().accepts_edits.get()
                && storage.presentation_mode() == mode
                && storage.ivars().table_projection_width_key.get() == width
                && storage.ivars().table_geometry_epoch.get() == geometry_epoch
                && std::rc::Weak::ptr_eq(&identity, &storage.ivars().document.borrow())
                && identity.upgrade().is_some_and(|document| {
                    let document = document.borrow();
                    document.generation() == generation && document.utf16_len() == length
                })
        };
        let target = (|| -> Result<_, String> {
            if let SelectionAction::Expansion(expansion) = action {
                // 全选只用有限源长度；词、视觉行和段落才临时取得原生导航图。
                let graph = expansion
                    .requires_known_head()
                    .then(|| NativeMarkdownSelectionGraph::load(self, &current))
                    .transpose()?;
                return selection_expansion_target(
                    expansion,
                    graph.as_ref(),
                    Some(&captured),
                    length,
                    &current,
                )
                .map(|target| (target, None));
            }
            let SelectionAction::Motion(motion) = action else {
                unreachable!("expansion handled above");
            };
            if let Some(at_end) = document_end {
                // 两个真实端点无需布局正文，也不消费旧换行侧；EOF 取上游，BOF 取下游。
                // 空文档只有同一个 BOF，不建立无意义的上游侧或垂直目标列。
                let head = if at_end { length } else { 0 };
                let selection = if motion.extending {
                    MarkdownTextSelection {
                        anchor_utf16: captured.selection.anchor_utf16,
                        head_utf16: head,
                    }
                } else {
                    MarkdownTextSelection::caret(head)
                };
                return Ok((
                    NativeMarkdownSelectionTarget {
                        selection,
                        head_upstream: at_end && length != 0,
                        line_offset: 0.0,
                    },
                    None,
                ));
            }
            let graph = NativeMarkdownSelectionGraph::load(self, &current)?;
            let vertical = motion.destination == NSTextSelectionNavigationDestination::Character
                && matches!(
                    motion.direction,
                    NSTextSelectionNavigationDirection::Up
                        | NSTextSelectionNavigationDirection::Down
                );
            let head_upstream = captured
                .head_is_upstream()
                .ok_or("当前插入位置的换行侧尚未确定")?;
            let column = if vertical {
                if let Some(goal) = captured
                    .vertical_goal()
                    .filter(|goal| goal.width_key == width && goal.geometry_epoch == geometry_epoch)
                {
                    Some(NativeMarkdownSelectionColumn {
                        line_offset: goal.line_offset,
                        container_x: goal.container_x,
                    })
                } else {
                    Some(graph.column_for_head(
                        self,
                        captured.selection.head_utf16,
                        head_upstream,
                        length,
                        &current,
                    )?)
                }
            } else {
                None // 横向、按词和边界动作建立新的目标列，结束此前的垂直列保持序列。
            };
            let target = graph.move_selection(
                captured.selection,
                motion,
                head_upstream,
                column.map(|column| column.line_offset),
                length,
                &current,
            )?;
            // 短行返回的实际 head 列不能覆盖原目标列；下一次垂直动作仍尝试原横向位置。
            let goal = column.and_then(|column| {
                NativeMarkdownVerticalGoal::new(
                    column.line_offset,
                    column.container_x,
                    width,
                    geometry_epoch,
                )
            });
            Ok((target, goal))
        })(); // graph 与所有临时 Native 结果先结束，再检查原动作是否仍可发布。
        if !current() {
            return None;
        }
        let Ok((target, goal)) = target else {
            return None; // 缺布局或无法证明坐标时不把原选择变成猜测结果，也不重复整个动作。
        };
        let valid_boundaries = identity.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document
                .byte_offset_for_utf16(target.selection.anchor_utf16)
                .is_ok()
                && document
                    .byte_offset_for_utf16(target.selection.head_utf16)
                    .is_ok()
        });
        if !valid_boundaries || !current() {
            return None; // UTF16 的数值范围合法还不够，两端都必须是 canonical Unicode 边界。
        }
        let Ok(captured) = captured.with_selection(target.selection, true) else {
            return None;
        };
        let captured = captured
            .with_head_side(Some(target.head_upstream))
            .with_vertical_goal(goal);
        if let Some(committed) = storage.update_active_selection(captured) {
            // 这份收据在纯发布时生成；属性通知返回后只能使用它，不重新捕获当前 fold 或 generation。
            let is_current = || {
                self.ivars().table_input_intent_epoch.get() == input_epoch
                    && storage.ivars().table_projection_width_key.get() == width
                    && storage.presentation_mode() == mode
            };
            if self.apply_committed_selection_if(&committed, is_current)
                && matches!(action, SelectionAction::Motion(_))
            {
                // setter 只安装选择；标准移动还须跟随新 head。仅请求零长 caret，不能为扩选布局整个范围。
                // 全选等展开动作不强制跳至全文末尾，保持用户当前阅读位置。
                let current = || committed.current() && is_current();
                let _ = native_markdown_table_intent_step(&current, || {
                    self.scrollRangeToVisible(NSRange::new(target.selection.head_utf16, 0))
                });
            }
        }
        None
    }
}
