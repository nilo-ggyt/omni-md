//! 系统已解释的编辑selector沿原permit执行；没有全局键、退出动作或新快捷键别名。

use objc2::{DefinedClass, runtime::Sel, sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSPasteboard, NSPasteboardTypeString};
use omni_markdown_core::{MarkdownEditingCommand, MarkdownTextSelection};

use super::{
    NativeMarkdownPagedEditCommand as Edit, NativeMarkdownPagedHeadAffinity,
    NativeMarkdownPagedInputHistory, NativeMarkdownPagedInputOutcome,
    NativeMarkdownPagedNavigation as Nav, NativeMarkdownPagedNavigationContext,
    NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
    NativeMarkdownPagedWordAnchor as WordAnchor, NativeMarkdownPagedWordSelection as WordSelection,
    Result, not_found,
};
use crate::payload_use::PayloadUseGuard;

#[derive(Clone, Copy)]
/// 指针手势仍只有原槽：单击保caret，双击保首击真实词组种子及原Shift锚点。
pub(super) enum DragAnchor {
    Caret(usize),
    Word {
        head_utf16: usize,
        backward_bias: bool,
        generation: u64,
        extend_anchor: Option<usize>,
    },
}

fn navigation(selector: Sel) -> Option<(Nav, bool)> {
    // 这些都是既有Cocoa文本语义动作；方向与视觉侧仍须由原Source真实Bidi证明。
    for (normal, extending, action) in [
        (
            sel!(moveBackward:),
            sel!(moveBackwardAndModifySelection:),
            Nav::Backward,
        ),
        (
            sel!(moveForward:),
            sel!(moveForwardAndModifySelection:),
            Nav::Forward,
        ),
        (
            sel!(moveLeft:),
            sel!(moveLeftAndModifySelection:),
            Nav::Left,
        ),
        (
            sel!(moveRight:),
            sel!(moveRightAndModifySelection:),
            Nav::Right,
        ),
        (sel!(moveUp:), sel!(moveUpAndModifySelection:), Nav::Up),
        (
            sel!(moveDown:),
            sel!(moveDownAndModifySelection:),
            Nav::Down,
        ),
        (
            sel!(moveWordBackward:),
            sel!(moveWordBackwardAndModifySelection:),
            Nav::WordBackward,
        ),
        (
            sel!(moveWordForward:),
            sel!(moveWordForwardAndModifySelection:),
            Nav::WordForward,
        ),
        (
            sel!(moveWordLeft:),
            sel!(moveWordLeftAndModifySelection:),
            Nav::WordLeft,
        ),
        (
            sel!(moveWordRight:),
            sel!(moveWordRightAndModifySelection:),
            Nav::WordRight,
        ),
        (
            sel!(moveToBeginningOfLine:),
            sel!(moveToBeginningOfLineAndModifySelection:),
            Nav::LineStart,
        ),
        (
            sel!(moveToEndOfLine:),
            sel!(moveToEndOfLineAndModifySelection:),
            Nav::LineEnd,
        ),
        (
            sel!(moveToLeftEndOfLine:),
            sel!(moveToLeftEndOfLineAndModifySelection:),
            Nav::LineLeft,
        ),
        (
            sel!(moveToRightEndOfLine:),
            sel!(moveToRightEndOfLineAndModifySelection:),
            Nav::LineRight,
        ),
        (
            sel!(moveToBeginningOfParagraph:),
            sel!(moveToBeginningOfParagraphAndModifySelection:),
            Nav::ParagraphStart,
        ),
        (
            sel!(moveToEndOfParagraph:),
            sel!(moveToEndOfParagraphAndModifySelection:),
            Nav::ParagraphEnd,
        ),
        // 旧正式路径将这两个系统selector映射为相同Paragraph方向，不另造快捷键或算法。
        (
            sel!(moveParagraphBackward:),
            sel!(moveParagraphBackwardAndModifySelection:),
            Nav::ParagraphStart,
        ),
        (
            sel!(moveParagraphForward:),
            sel!(moveParagraphForwardAndModifySelection:),
            Nav::ParagraphEnd,
        ),
        (
            sel!(moveToBeginningOfDocument:),
            sel!(moveToBeginningOfDocumentAndModifySelection:),
            Nav::DocumentStart,
        ),
        (
            sel!(moveToEndOfDocument:),
            sel!(moveToEndOfDocumentAndModifySelection:),
            Nav::DocumentEnd,
        ),
        (sel!(pageUp:), sel!(pageUpAndModifySelection:), Nav::PageUp),
        (
            sel!(pageDown:),
            sel!(pageDownAndModifySelection:),
            Nav::PageDown,
        ),
    ] {
        if selector == normal {
            return Some((action, false));
        }
        if selector == extending {
            return Some((action, true));
        }
    }
    for (selector_value, action) in [
        (sel!(selectWord:), Nav::SelectWord),
        (sel!(selectLine:), Nav::SelectLine),
        (sel!(selectParagraph:), Nav::SelectParagraph),
    ] {
        if selector == selector_value {
            return Some((action, false));
        }
    }
    None
}

fn edit(selector: Sel) -> Option<Edit> {
    for (selector_value, action) in [
        (sel!(deleteBackward:), Edit::DeleteBackward),
        (sel!(deleteForward:), Edit::DeleteForward),
        (sel!(deleteWordBackward:), Edit::DeleteWordBackward),
        (sel!(deleteWordForward:), Edit::DeleteWordForward),
        (sel!(deleteToBeginningOfLine:), Edit::DeleteToLineStart),
        (sel!(deleteToEndOfLine:), Edit::DeleteToLineEnd),
        (sel!(insertNewline:), Edit::InsertNewline),
        (sel!(insertNewlineIgnoringFieldEditor:), Edit::InsertNewline),
        (sel!(insertTab:), Edit::InsertTab),
        (sel!(insertTabIgnoringFieldEditor:), Edit::InsertTab),
        (sel!(insertBacktab:), Edit::InsertBacktab),
    ] {
        if selector == selector_value {
            return Some(action);
        }
    }
    None
}

/// 只有这两个既有纯滚动action可进typed入口；不把任意Sel强转成Native sender调用。
fn page_scroll(selector: Sel) -> Option<bool> {
    if selector == sel!(scrollPageDown:) {
        Some(true)
    } else if selector == sel!(scrollPageUp:) {
        Some(false)
    } else {
        None
    }
}

/// 同一翻译入口同时供typed调用和系统分派识别；已识别动作的运行失败不代表未知selector。
pub(super) fn recognizes_selector(selector: Sel) -> bool {
    navigation(selector).is_some()
        || edit(selector).is_some()
        || page_scroll(selector).is_some()
        || [
            sel!(undo:),
            sel!(redo:),
            sel!(selectAll:),
            sel!(copy:),
            sel!(cut:),
            sel!(paste:),
            sel!(pasteAsPlainText:),
            sel!(cancelOperation:),
            sel!(delete:),
        ]
        .contains(&selector)
}

impl NativeMarkdownPagedTextView {
    /// typed入口供原菜单/控制器复用；未知selector明确拒绝，只有原Native入口有action转发权限。
    pub fn command(&self, selector: Sel) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        if !recognizes_selector(selector) {
            return Err(NativeMarkdownPagedTextViewError::UnsupportedCommand);
        }
        let origin = self.origin()?;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        if let Some(down) = page_scroll(selector) {
            let request = self.scene_request(&origin)?;
            return origin.delegate.scroll_page(&request, down); // 只接纳原滚动意图，不改Input/目标列，也不重放未知action。
        }
        if selector == sel!(undo:) || selector == sel!(redo:) {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let direction = if selector == sel!(undo:) {
                NativeMarkdownPagedInputHistory::Undo
            } else {
                NativeMarkdownPagedInputHistory::Redo
            };
            let outcome = origin.input.history(&permit, direction, &current)?;
            return self.finish_edit_outcome(&origin, outcome);
        }
        if selector == sel!(selectAll:) {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let outcome = origin.input.set_selection_intent(
                &permit,
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: origin.state.utf16_len,
                },
                None,
                None,
                &current,
            )?;
            return self.finish_outcome(&origin, outcome); // 全选只用canonical长度，不为了选择而layout全文。
        }
        if selector == sel!(copy:) || selector == sel!(cut:) {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let request = self.scene_request(&origin)?;
            origin.delegate.copy_selection(&request)?;
            if !origin.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if selector == sel!(cut:) {
                // 剪贴板已完整成功后才删真实全部选择；不能只删主范围或跨过未选择的正文。
                let outcome = origin.input.delete_selected_ranges(&permit, &current)?;
                return self.finish_edit_outcome(&origin, outcome);
            }
            return Ok(()); // 只复制不制造Core事务；实际剪贴板生产者须完整成功才返回。
        }
        if selector == sel!(paste:) || selector == sel!(pasteAsPlainText:) {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let pasteboard = origin.step(self, NSPasteboard::generalPasteboard)?;
            let text = origin
                .step(self, || {
                    // SAFETY: 框架公开字符串类型常量在进程生命周期有效，不访问用户正文。
                    pasteboard.stringForType(unsafe { NSPasteboardTypeString })
                })?
                .ok_or(NativeMarkdownPagedTextViewError::ClipboardUnavailable)?;
            let commit =
                origin
                    .input
                    .insert_native_isolated(&permit, &text, not_found(), &current)?;
            return self.finish_edit_commit(&origin, commit); // 新增剪贴板文字仍走原有限输入转换/事务上限。
        }
        if selector == sel!(cancelOperation:) && origin.state.marked.is_some() {
            return self.apply_unmark();
        }
        if origin.input.is_cell() {
            use super::NativeMarkdownPagedCellAction as CellAction;
            let action = if selector == sel!(cancelOperation:) {
                Some(CellAction::Cancel)
            } else {
                match edit(selector) {
                    Some(Edit::InsertTab) => Some(CellAction::Next),
                    Some(Edit::InsertBacktab) => Some(CellAction::Previous),
                    Some(Edit::InsertNewline) => Some(CellAction::NextRow),
                    _ => None,
                }
            };
            if let Some(action) = action {
                let permit = origin.input.capture(origin.epoch, &current)?;
                // 合法客户动作可结束原Input；成功后不以旧origin强迫恢复，也不重放为字符输入。
                return origin.delegate.cell_action(action, permit);
            }
        }

        if selector == sel!(delete:) {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let outcome = origin.input.delete_selected_ranges(&permit, &current)?;
            return self.finish_edit_outcome(&origin, outcome);
        }
        if let Some((action @ (Nav::Left | Nav::Right), extend)) = navigation(selector)
            && origin.state.marked.is_none()
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let request = self.scene_request(&origin)?;
            if origin
                .delegate
                .move_visual(&request, action == Nav::Right, extend, permit)?
            {
                return Ok(());
            }
        } // Accepted motions retain intent without requiring a Scene; subsequent ordinary input enters this same queue.
        if let Some((
            action @ (Nav::WordBackward | Nav::WordForward | Nav::WordLeft | Nav::WordRight),
            extend,
        )) = navigation(selector)
            && origin.state.marked.is_none()
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let request = self.scene_request(&origin)?;
            let forward = matches!(action, Nav::WordForward | Nav::WordRight);
            // 这里只转系统动作。仅全源UniformZero能让这两对方向等同；其余源须明确拒绝，不能借逻辑组猜视觉。
            return origin.delegate.move_word(&request, forward, extend, permit);
        }
        if navigation(selector) == Some((Nav::SelectWord, false)) && origin.state.marked.is_none() {
            let selection = origin
                .state
                .directed_selection()
                .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
            let permit = origin.input.capture(origin.epoch, &current)?;
            let request = self.scene_request(&origin)?;
            return origin.delegate.select_word(
                &request,
                WordSelection {
                    head_utf16: selection.head_utf16,
                    backward_bias: false,
                    anchor: None,
                },
                permit,
            ); // 与原groupAt默认右侧相同；BOF/硬行首尾由原GCB决定，不预先layout整词。
        } // marked仍交原系统组合路径，不结束组合或把provisional字符当可查询正文。
        if matches!(
            edit(selector),
            Some(Edit::DeleteWordBackward | Edit::DeleteWordForward)
        ) && origin.state.marked.is_none()
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let ranges = origin.input.selected_ranges(&permit, &current)?;
            if ranges.len() > 1 && ranges.iter().all(|range| !range.is_empty()) {
                // 已知实际选择直接删除全部范围；不因Word selector重算或扩大用户已选内容。
                let outcome = origin.input.delete_selected_ranges(&permit, &current)?;
                return self.finish_edit_outcome(&origin, outcome);
            }
            if ranges.len() > 1 {
                return Err(super::NativeMarkdownPagedInputError::SelectionGeometryRequired.into());
            } // 混合空caret尚无整体词组计划，明确失败，不能只删其中主范围。
            let request = self.scene_request(&origin)?;
            return origin.delegate.delete_word(
                &request,
                edit(selector) == Some(Edit::DeleteWordForward),
                permit,
            ); // Body和Cell都交同一个原producer，不要求当前已具备LineReady。
        } // marked仍走原组合输入/Scene处理，不读取provisional正文或擅自结束IME。
        if (selector == sel!(deleteBackward:) || selector == sel!(deleteForward:))
            && (origin.state.selection_range_count > 1 || origin.state.primary_range.length != 0)
        {
            // 唯一空caret没有选中字节，直接交下方原命令票；完整非空/多选才需要读取范围集合。
            let permit = origin.input.capture(origin.epoch, &current)?;
            let ranges = origin.input.selected_ranges(&permit, &current)?;
            if ranges.iter().all(|range| !range.is_empty()) {
                // 已知完整选择的删除不依赖命中/行几何；尚无scene也不能丢掉已解释的确定输入。
                let outcome = origin.input.delete_selected_ranges(&permit, &current)?;
                return self.finish_edit_outcome(&origin, outcome);
            }
            if ranges.len() > 1 {
                // 混合caret需要每个真实GCB及整体计划；单replacement不能冒充多目标删除。
                return Err(super::NativeMarkdownPagedInputError::SelectionGeometryRequired.into());
            }
        }

        if !origin.input.is_cell() && origin.state.marked.is_none() {
            let source_command = match edit(selector) {
                Some(Edit::InsertNewline) => Some(MarkdownEditingCommand::InsertNewline),
                Some(Edit::InsertTab) => Some(MarkdownEditingCommand::IndentMore),
                Some(Edit::InsertBacktab) => Some(MarkdownEditingCommand::IndentLess),
                Some(Edit::DeleteBackward) => Some(MarkdownEditingCommand::DeleteMarkupBackward),
                _ => None, // 其余位置命令继续消费下方原Scene，不复制导航/删除算法。
            };
            if let Some(command) = source_command {
                // Return/缩进不需要排版。Backspace只预取同Key的普通GCB计划；未Ready不能阻止
                // 已适用的Core标记删除，但源/权限等真实错误不能被标记规则掩盖。
                let backward_scene = if command == MarkdownEditingCommand::DeleteMarkupBackward {
                    match self.scene() {
                        Ok(scene) => Some(scene),
                        Err(NativeMarkdownPagedTextViewError::Pending) => None,
                        Err(cause) => return Err(cause),
                    }
                } else {
                    None
                };
                let ordinary = if let Some(scene) = &backward_scene {
                    if scene.origin.state != origin.state || !scene.current(self) {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    let plan = scene.scene.edit_plan(Edit::DeleteBackward);
                    if !scene.current(self) {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    match plan {
                        Ok(plan) => Ok(plan.replacement),
                        Err(cause @ NativeMarkdownPagedTextViewError::Pending)
                        | Err(cause @ NativeMarkdownPagedTextViewError::InvalidRange) => Err(cause),
                        Err(cause) => return Err(cause),
                    }
                } else {
                    Err(NativeMarkdownPagedTextViewError::Pending)
                };
                let current = |epoch| {
                    epoch == origin.epoch
                        && origin.current(self)
                        && backward_scene
                            .as_ref()
                            .is_none_or(|scene| scene.current(self))
                };
                let permit = origin.input.capture(origin.epoch, &current)?;
                let outcome = origin.input.apply_source_editing_command(
                    &permit,
                    command,
                    ordinary.as_ref().ok().copied(),
                    &current,
                )?;
                return match outcome {
                    Some(outcome) => self.finish_outcome(&origin, outcome),
                    None => match ordinary {
                        Err(cause) => Err(cause), // 未处理且无GCB须报告原Pending/范围错误，不能假称成功。
                        Ok(_) => Err(NativeMarkdownPagedTextViewError::InvalidRange),
                    },
                };
            }
        } // marked与Cell仍沿原输入/客户动作分支，不能在此擅自结束组合或插入正文Tab。

        if let Some((action @ (Nav::DocumentStart | Nav::DocumentEnd), extend)) =
            navigation(selector)
        {
            // 真正的端点已由原Body/Cell局部长度确定，不需要先取得可见scene。非扩选直接
            // 发布新caret；Find多选的未知主方向不能阻止这次明确意图，也不能拿旧head代替。
            let head = if action == Nav::DocumentStart {
                0
            } else {
                origin.state.utf16_len
            };
            let anchor = if extend {
                origin
                    .state
                    .directed_selection()
                    .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                    .anchor_utf16
            } else {
                head
            }; // 扩选仍保留真实anchor，不从多选历史memento猜出一个活动端。
            let permit = origin.input.capture(origin.epoch, &current)?;
            let outcome = origin.input.set_selection_intent(
                &permit,
                MarkdownTextSelection {
                    anchor_utf16: anchor,
                    head_utf16: head,
                },
                Some(action == Nav::DocumentEnd),
                None,
                &current,
            )?;
            return match outcome {
                NativeMarkdownPagedInputOutcome::Committed(commit) => {
                    self.finish_commit_ref(&origin, &commit)?;
                    // 选择已经提交；后续只有原收据可请求真实reveal。任何拒绝都保留
                    // CommittedEffect事实，不能因为尚未可见而重放选择或制造另一笔历史。
                    origin
                        .delegate
                        .reveal_committed_selection(&commit)
                        .map_err(|cause| {
                            NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                        })
                }
                unchanged => self.finish_outcome(&origin, unchanged),
            };
        }

        if let Some((action @ (Nav::Up | Nav::Down | Nav::PageUp | Nav::PageDown), extend)) =
            navigation(selector)
            && !origin.input.is_cell()
        {
            let request = self.scene_request(&origin)?;
            if origin
                .delegate
                .navigate_vertical(&request, action, extend)?
            {
                return Ok(()); // 原owner已接纳同一意图；离屏时仍未提交选择，不能重放这个系统动作。
            }
        }

        // 其他位置动作只消费同一现成scene，不能把尚未Ready的GCB/Bidi结果改成scalar近似。
        if navigation(selector).is_some() || edit(selector).is_some() || selector == sel!(delete:) {
            let scene = self.scene()?;
            if scene.origin.state != origin.state || !origin.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if let Some((action, extend)) = navigation(selector) {
                if origin.state.directed_selection().is_none() {
                    return Err(NativeMarkdownPagedTextViewError::Pending);
                } // 多选没有唯一主方向，不能使用历史memento猜本次移动起点。
                let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
                let permit = origin.input.capture(origin.epoch, &current)?;
                let geometry = scene.scene.navigation_geometry()?;
                if !scene.current(self) || !geometry.permits(&scene.key) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let (head_upstream, goal) =
                    origin.input.navigation_selection_facts(&permit, &current)?;
                let stale_goal = goal.is_some_and(|goal| !geometry.permits_goal(goal));
                let context = NativeMarkdownPagedNavigationContext {
                    geometry: geometry.clone(),
                    // 布局变化不能继承旧换行侧；原owner也必须在Source/样式变化时清同一槽。
                    head_affinity: if stale_goal {
                        None
                    } else {
                        head_upstream.map(NativeMarkdownPagedHeadAffinity::from_upstream)
                    },
                    vertical_goal: goal.filter(|goal| {
                        action.preserves_vertical_goal() && geometry.permits_goal(*goal)
                    }),
                    bidi_side: origin.input.navigation_bidi_side(&permit, &current)?,
                };
                let target = scene.scene.navigate(action, extend, &context)?;
                if !scene.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let after_geometry = scene.scene.navigation_geometry()?;
                if !scene.current(self) || !after_geometry.same(&geometry) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                } // 原计算期间排版变化不能领取新geometry继续签旧目标。
                if target.vertical_goal.is_some_and(|goal| {
                    !geometry.permits_goal(goal)
                        || target.head_affinity.is_none()
                        || !action.preserves_vertical_goal()
                }) {
                    return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
                }
                if target.head_affinity.is_none()
                    || (action.preserves_vertical_goal() && target.vertical_goal.is_none())
                {
                    return Err(NativeMarkdownPagedTextViewError::Pending);
                } // 原producer尚无真实换行侧/起始列时不先提交一个近似移动。
                if action.preserves_vertical_goal()
                    && context.vertical_goal.is_some()
                    && target.vertical_goal != context.vertical_goal
                {
                    return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
                } // 已有目标列只能沿原事实继续；短行的实际head列不能覆盖它。
                if extend && target.selection.anchor_utf16 != origin.state.selection.anchor_utf16 {
                    return Err(NativeMarkdownPagedTextViewError::InvalidRange);
                } // 扩选只能移动原活动端，不能悄悄替换用户已确定的anchor。
                let outcome = if let Some(side) = target.bidi_side {
                    // 真视觉侧与选择在同一原permit内发布；同offset也得到新serial/收据。
                    origin.input.set_visual_selection_intent(
                        &permit,
                        target.selection,
                        target
                            .head_affinity
                            .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                            .is_upstream(),
                        side,
                        &current,
                    )?
                } else {
                    // 逻辑移动、垂直目标或非空collapse没有旧Bidi侧的继承权。
                    origin.input.set_selection_intent(
                        &permit,
                        target.selection,
                        target
                            .head_affinity
                            .map(NativeMarkdownPagedHeadAffinity::is_upstream),
                        target.vertical_goal,
                        &current,
                    )?
                };
                if matches!(action, Nav::Left | Nav::Right) {
                    return match outcome {
                        NativeMarkdownPagedInputOutcome::Committed(commit) => {
                            self.finish_commit_ref(&origin, &commit)?;
                            // 已准备邻行可以进入原reveal续体；只交付原Commit，不重捕选择
                            // 或重放动作。尚无实际行图时navigate已在提交前保持Pending。
                            origin
                                .delegate
                                .reveal_committed_selection(&commit)
                                .map_err(|cause| {
                                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(
                                        cause,
                                    ))
                                })
                        }
                        unchanged => self.finish_outcome(&origin, unchanged),
                    };
                }
                return self.finish_outcome(&origin, outcome);
            }
            let permit = origin.input.capture(origin.epoch, &current)?;
            let action = edit(selector).ok_or(NativeMarkdownPagedTextViewError::Pending)?;
            let plan = scene.scene.edit_plan(action)?;
            if !scene.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let commit =
                origin
                    .input
                    .insert_text(&permit, &plan.inserted, plan.replacement, &current)?;
            return self.finish_edit_commit(&origin, commit);
        }

        Err(NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }

    /// 异步词组范围已消费原permit并提交后，只用同一Commit发送原显示通知；不重签编辑before。
    pub(in crate::native_surface) fn finish_word_delete_commit(
        &self,
        commit: &super::NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let origin = self
            .origin()
            .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))?;
        commit
            .check_current(&|epoch| origin.session_current(self, epoch))
            .map_err(|cause| {
                NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause.into()))
            })?;
        self.finish_commit_ref(&origin, commit)?;
        if origin.input.is_cell() {
            origin
                .delegate
                .reveal_committed_selection(commit)
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                })?;
        } // 只借同一收据，原队列才能在Native通知后继续认证这一实际postcommit。
        Ok(())
    }

    /// 只接受系统原action selector，安全Rust入口不能授予任意Sel对象参数调用的ABI权限。
    ///
    /// # Safety
    /// selector必须遵守Cocoa action的单个可选sender对象签名；不能把任意getter/setter送入。
    pub(super) unsafe fn forward_native_action(&self, selector: Sel) -> Result<()> {
        let origin = self.origin()?;
        // SAFETY: 实际主线程view在原同步payload租约内，公开responder getter没有跨Doc借用。
        let next = origin.step(self, || unsafe { self.nextResponder() })?;
        if let Some(next) = next {
            // SAFETY: selector来自系统公开语义入口；同一responder链只尝试原selector一次。
            origin.step(self, || unsafe {
                next.tryToPerform_with(selector, Some(self))
            })?;
        } // CommandF/插件返回等仍由既有控制器处理，本view不注册或改写这些动作。
        Ok(())
    }

    pub(super) fn mouse(&self, event: &NSEvent, dragging: bool) {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let result = (|| {
            let origin = self.origin()?;
            if origin.state.marked.is_some() {
                // IME拥有组合期间的鼠标；不在它提交后给同一个旧点击补签较新的scene。
                let (_, context) = self.foreground_context()?;
                context.0.handleEvent(event);
                if !origin.session_current(self, origin.epoch) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                return Ok(());
            }
            let scene = self.scene()?;
            if !origin.current(self) || scene.origin.state != origin.state {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let window = scene
                .step(self, || self.window())?
                .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
            let focused = scene.step(self, || window.makeFirstResponder(Some(self)))?;
            if !focused {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let point = scene.step(self, || event.locationInWindow())?;
            let local = scene.step(self, || self.convertPoint_fromView(point, None))?;
            let flags = scene.step(self, || event.modifierFlags())?;
            let clicks = scene.step(self, || event.clickCount())?;
            let clicks = usize::try_from(clicks)
                .map_err(|_| NativeMarkdownPagedTextViewError::InvalidGeometry)?;
            let extend = dragging || flags.contains(NSEventModifierFlags::Shift);
            let pointer = self.ivars().drag_anchor.get();
            if (!dragging && clicks == 2)
                || (dragging && matches!(pointer, Some(DragAnchor::Word { .. })))
            {
                let (head_utf16, backward_bias) = scene.scene.word_hit(local)?;
                if !scene.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let anchor = if dragging {
                    let Some(DragAnchor::Word {
                        head_utf16: first,
                        backward_bias: first_bias,
                        generation,
                        extend_anchor,
                    }) = pointer
                    else {
                        unreachable!("本手势已确认原Word指针槽");
                    };
                    if generation != origin.state.generation {
                        self.ivars().drag_anchor.set(None);
                        return Err(NativeMarkdownPagedTextViewError::Revoked); // 正文编辑后旧点击坐标失权，不把它映射到新文稿。
                    }
                    Some(match extend_anchor {
                        Some(anchor) => WordAnchor::Selection(anchor),
                        None => WordAnchor::Word {
                            head_utf16: first,
                            backward_bias: first_bias,
                        },
                    })
                } else {
                    let extend_anchor = if extend {
                        Some(
                            origin
                                .state
                                .directed_selection()
                                .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                                .anchor_utf16,
                        )
                    } else {
                        None
                    }; // Shift只消费原有向选择锚点，不能从多选主范围或历史memento猜一个方向。
                    self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                        head_utf16,
                        backward_bias,
                        generation: origin.state.generation,
                        extend_anchor,
                    }));
                    extend_anchor.map(WordAnchor::Selection)
                };
                let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
                let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
                return scene.origin.delegate.select_word(
                    &scene.key.request,
                    WordSelection {
                        head_utf16,
                        backward_bias,
                        anchor,
                    },
                    permit,
                );
                // 真实Down即时登记原种子，mouseup只清同槽；迟到Ready不重装指针或保留NSEvent。
            }
            let anchor = if dragging {
                match pointer {
                    Some(DragAnchor::Caret(anchor)) => Some(anchor),
                    _ => None,
                }
            } else if extend {
                // 未知多选主方向不能由历史memento猜出；仅明确的原方向允许延伸。
                scene
                    .origin
                    .state
                    .directed_selection()
                    .map(|selection| selection.anchor_utf16)
            } else {
                None
            };
            let target = scene.scene.mouse_selection(local, clicks, anchor, extend)?;
            if !scene.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let current = |epoch| epoch == scene.origin.epoch && scene.origin.current(self);
            let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
            let outcome = scene
                .origin
                .input
                .set_selection_intent(&permit, target, None, None, &current)?;
            self.finish_outcome(&scene.origin, outcome)?;
            if !dragging {
                self.ivars()
                    .drag_anchor
                    .set(Some(DragAnchor::Caret(target.anchor_utf16)));
            }
            Ok(())
        })();
        self.record(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_visual_word_and_line_selectors_do_not_collapse_into_logical_motion() {
        assert_eq!(
            navigation(sel!(moveWordLeft:)),
            Some((Nav::WordLeft, false))
        );
        assert_eq!(
            navigation(sel!(moveWordRightAndModifySelection:)),
            Some((Nav::WordRight, true))
        );
        assert_ne!(
            navigation(sel!(moveWordLeft:)),
            navigation(sel!(moveWordBackward:))
        );
        assert_ne!(
            navigation(sel!(moveWordRight:)),
            navigation(sel!(moveWordForward:))
        );
        assert_eq!(
            navigation(sel!(moveToLeftEndOfLine:)),
            Some((Nav::LineLeft, false))
        );
        assert_eq!(
            navigation(sel!(moveToRightEndOfLineAndModifySelection:)),
            Some((Nav::LineRight, true))
        );
        assert_ne!(
            navigation(sel!(moveToLeftEndOfLine:)),
            navigation(sel!(moveToBeginningOfLine:))
        );
    }

    #[test]
    fn legacy_paragraph_aliases_share_original_direction_without_capturing_host_actions() {
        assert_eq!(
            navigation(sel!(moveParagraphBackward:)),
            navigation(sel!(moveToBeginningOfParagraph:))
        );
        assert_eq!(
            navigation(sel!(moveParagraphForwardAndModifySelection:)),
            navigation(sel!(moveToEndOfParagraphAndModifySelection:))
        );
        for unrelated in [
            sel!(performFindPanelAction:),
            sel!(cancelOperation:),
            sel!(terminate:),
        ] {
            assert_eq!(navigation(unrelated), None);
            assert_eq!(edit(unrelated), None);
        } // 插件查找、返回和应用退出仍属于原responder链，本层不新增键或冒领Host动作。
    }
}
