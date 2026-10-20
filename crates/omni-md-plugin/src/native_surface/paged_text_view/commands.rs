//! 系统已解释的编辑selector沿原permit执行；没有全局键、退出动作或新快捷键别名。

use objc2::{ClassType, DefinedClass, runtime::Sel, sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSPasteboard, NSPasteboardTypeString};
use omni_markdown_core::{MarkdownEditingCommand, MarkdownTextSelection};

use super::{
    NativeMarkdownPagedEditCommand as Edit, NativeMarkdownPagedHeadAffinity,
    NativeMarkdownPagedInputHistory, NativeMarkdownPagedInputOutcome,
    NativeMarkdownPagedNavigation as Nav, NativeMarkdownPagedNavigationContext,
    NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
    NativeMarkdownPagedWordAnchor as WordAnchor, NativeMarkdownPagedWordMotion,
    NativeMarkdownPagedWordSelection as WordSelection, Result, not_found,
};
use crate::payload_use::PayloadUseGuard;

#[derive(Clone, Copy)]
/// 指针手势仍只有原槽：单击保caret，双击保首击真实词组种子及原Shift锚点。
pub(super) enum DragAnchor {
    Caret {
        anchor_utf16: usize,
        generation: u64,
    },
    Word {
        anchor: WordAnchor,
        generation: u64,
    },
}

/// 同一Down的有限物理事实；不持系统事件、Native对象或正文，移交时跟随原手势。
#[derive(Clone, Copy)]
pub(super) struct PointerMotion {
    pub down: objc2_foundation::NSPoint,
    pub dragged: bool,
    pub clicks: usize,
}

impl DragAnchor {
    /// 原实际发布状态只给新canonical坐标与代次；词种子必须已经经过同趟sanitizer真实映射。
    /// 转换不读取Source、不换Input票，分别供真正移交和后继选择收据更新使用。
    fn from_published_selection(
        state: super::NativeMarkdownPagedInputState,
        word_anchor: Option<WordAnchor>,
    ) -> Result<Self> {
        if !state.selection_is_directed {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        match word_anchor {
            Some(anchor) => {
                let position = match anchor {
                    WordAnchor::Selection(position) => position,
                    WordAnchor::Word { head_utf16, .. } => head_utf16,
                    WordAnchor::Group {
                        from_utf16,
                        to_utf16,
                        origin_utf16,
                    } => {
                        if from_utf16 > to_utf16
                            || to_utf16 > state.utf16_len
                            || origin_utf16 < from_utf16
                            || origin_utf16 > to_utf16
                        {
                            return Err(NativeMarkdownPagedTextViewError::InvalidRange);
                        }
                        from_utf16
                    }
                };
                if position > state.utf16_len {
                    return Err(NativeMarkdownPagedTextViewError::InvalidRange);
                }
                Ok(Self::Word {
                    anchor,
                    generation: state.generation,
                })
            }
            None => Ok(Self::Caret {
                anchor_utf16: state.selection.anchor_utf16,
                generation: state.generation,
            }),
        }
    }
}

#[derive(Clone)]
/// 真实child提交后仅为Cocoa仍发往旧view的Drag/Up保留弱目的地；不续旧草稿读取或解释旧事件。
pub(super) struct CellPointerForward {
    pub(super) parent: std::rc::Weak<objc2::rc::Retained<NativeMarkdownPagedTextView>>,
    pub(super) intent: std::rc::Weak<()>,
}

/// 原mousedown只记真实命中与弱Scene票；拖动沿原caret选择，mouseup才允许提交一次链接意图。
pub(super) struct PressedLink {
    key: super::NativeMarkdownPagedSceneKey,
    part: std::ops::Range<usize>,
    index: usize,
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
        self.command_internal(selector, false)
    }

    /// 只有NSTextInputContext的原doCommand或队首菜单进入此解释入口；公开命令始终先过屏障。
    pub(super) fn command_from_input_context(&self, selector: Sel) -> Result<()> {
        self.command_internal(selector, true)
    }

    fn command_internal(&self, selector: Sel, from_input_context: bool) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_cell_pointer_intent();
        self.cancel_link_intent(); // 菜单或已解释按键都排在原click之后，旧外部激活不能越过它。
        if !recognizes_selector(selector) {
            return Err(NativeMarkdownPagedTextViewError::UnsupportedCommand);
        }
        let mut origin = self.origin()?;
        origin.interpreted = from_input_context
            && self.ivars().interpreting_key.get()
            && self.ivars().input_callback_depth.get() == 1;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        if !from_input_context && origin.delegate.has_native_input_barrier() {
            let permit = origin.input.capture(origin.epoch, &current)?;
            if !origin.delegate.defer_native_command(selector, &permit)? {
                return Err(NativeMarkdownPagedTextViewError::Revoked); // 原屏障已失权，不能捕后继状态续权。
            }
            return Ok(());
        }
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
            if selector == sel!(paste:) && !origin.input.is_cell() {
                // 正文维持文件优先；单元格只接收剪贴板文字，不能因同时存在文件URL而误入附件事务。
                // 文件I/O延到同一Word队首，不能阻塞Native粘贴回调。
                let paths = origin.step(self, || {
                    crate::native_surface::workspace_actions::native_markdown_pasteboard_file_paths(&pasteboard)
                })?.map_err(NativeMarkdownPagedTextViewError::AttachmentImport)?;
                if !paths.is_empty() {
                    return self.queue_attachment_paths(paths, None, origin.state);
                }
            }
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
            let context = self
                .ivars()
                .context
                .borrow()
                .clone()
                .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
            // 保原Esc接受已显示组合的交互，只结束该客户的系统转换，不扩为取消正文/退出格。
            // 协议unmarkText自身仍仅local-unmark；这里是业务命令，不能留下旧IME缓冲继续提交。
            return self.finish_composition_in_context(
                &origin.input,
                &origin.delegate,
                &context,
                origin.epoch,
            );
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

        if (selector == sel!(deleteBackward:) || selector == sel!(deleteForward:))
            && origin.state.marked.is_none()
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            if origin
                .delegate
                .defer_grapheme_delete(selector == sel!(deleteForward:), &permit)?
            {
                return Ok(()); // 原移动尚未完成，不能按旧caret删除并取消此前已接纳的选择意图。
            }
        } // 输入法标记仍走同步原NSTextInputClient协议，不将系统候选排入异步字符队列。

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
            let motion = match action {
                Nav::WordBackward => NativeMarkdownPagedWordMotion::Backward,
                Nav::WordForward => NativeMarkdownPagedWordMotion::Forward,
                Nav::WordLeft => NativeMarkdownPagedWordMotion::Left,
                Nav::WordRight => NativeMarkdownPagedWordMotion::Right,
                _ => unreachable!("这个分支只解释原四种 Word selector"),
            }; // 跨原队列保留四向含义；RTL 的屏幕左右不能在传输层丢失后再由 Core 猜回。
            return origin.delegate.move_word(&request, motion, extend, permit);
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
                    cell: None,
                },
                None,
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
                if command != MarkdownEditingCommand::DeleteMarkupBackward {
                    let permit = origin.input.capture(origin.epoch, &current)?;
                    if origin.delegate.defer_source_command(command, &permit)? {
                        return Ok(()); // 等前一步真实选择后再解释原语义，不能在旧caret续行并取消已接纳移动。
                    }
                } // Backspace已由上方完整GCB队列保序，marked与Cell从未进入这段Source入口。
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
                    Some(outcome) => self.finish_edit_outcome(&origin, outcome),
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
            let permit = origin.input.capture(origin.epoch, &current)?;
            if origin.state.marked.is_none()
                && origin.delegate.defer_document_motion(
                    action == Nav::DocumentEnd,
                    extend,
                    &permit,
                )?
            {
                return Ok(()); // 先前已接纳文字和移动必须先完成，不能拿旧anchor/长度立即接管原链。
            }
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
                    self.finish_owned_commit(&origin, commit, true) // 同一原收据显示；解释中不会取消后继event。
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
                            self.finish_owned_commit(&origin, commit, true)
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

    /// 父Body在移焦或读父Scene前把本次实际事件交正在组合的子格；不产生第二个解释队列。
    pub(in crate::native_surface) fn handle_marked_mouse(&self, event: &NSEvent) -> Result<bool> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = self.origin()?;
        if origin.state.marked.is_none() {
            return Ok(false);
        }
        let (_, context) = self.foreground_context()?;
        context.0.handleEvent(event);
        if !origin.session_current(self, origin.epoch) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(true) // IME可以同步改变marked/selection；这次事件已经交付，不再给父正文重放。
    }

    /// 父Body只交已解释的真实格点击；子view用本次raw范围转换局部端点，不重播系统事件。
    /// false只表示原Scene尚未Ready；任何Source/输入/资源错误仍返回，已接纳的词选择不重试。
    pub(in crate::native_surface) fn apply_cell_pointer(
        &self,
        pointer: &super::NativeMarkdownPagedCellPointer,
        raw_range: std::ops::Range<usize>,
    ) -> Result<bool> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = self.origin()?;
        if !origin.input.is_cell() || origin.state.marked.is_some() {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        } // 组合输入仍归原InputContext；父点击不能越过候选或借普通Body输入伪装活动格。
        // 双击只查询已签出的真实 raw/head/侧，不借字形；不能为了等首帧而阻塞原首词组。
        // 三击仍必须等实际 Scene 的完整段落证明，不能把可见后缀当成整段。
        let request = self.scene_request(&origin)?;
        // 父端点仍属于原canonical raw格，子输入只接受局部坐标；边界外的Shift由父owner提交后续接。
        // 不截断端点来“适配”草稿，也不把离屏部分复制成临时字符串。
        let local = |position: usize| {
            position
                .checked_sub(raw_range.start)
                .filter(|position| *position <= raw_range.end.saturating_sub(raw_range.start))
                .filter(|position| *position <= origin.state.utf16_len)
                .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)
        };
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let head = local(pointer.hit.head_utf16)?;
        let anchor = pointer.anchor.map(local).transpose()?;
        // 同一Scene/输入代次约束后续一次提交；current可以重入，不能跨Native保Doc或Reader租约。
        // 原输入许可在词组producer或实际选择中恰好消费一次，失败不会改签同一鼠标事件。
        let current = |epoch| {
            epoch == origin.epoch
                && origin.current(self)
                && self
                    .scene_request(&origin)
                    .is_ok_and(|actual| actual.same(&request))
        };
        if pointer.clicks == 2 {
            let permit = origin.input.capture(origin.epoch, &current)?;
            // 拖词保首击词种子，Shift拖词保原有向anchor；二者由原UBA分别解释，不能混为caret。
            // 所有坐标都从同一次raw范围换算，跨格端点应已由父owner接管，不能在这里钳到格边。
            let anchor = pointer
                .word_anchor
                .map(|anchor| -> Result<WordAnchor> {
                    Ok(match anchor {
                        WordAnchor::Selection(position) => WordAnchor::Selection(local(position)?),
                        WordAnchor::Word {
                            head_utf16,
                            backward_bias,
                            ..
                        } => WordAnchor::Word {
                            head_utf16: local(head_utf16)?,
                            backward_bias,
                            cell: None,
                        },
                        WordAnchor::Group {
                            from_utf16,
                            to_utf16,
                            origin_utf16,
                        } => WordAnchor::Group {
                            from_utf16: local(from_utf16)?,
                            to_utf16: local(to_utf16)?,
                            origin_utf16: local(origin_utf16)?,
                        },
                    })
                })
                .transpose()?;
            origin.delegate.select_word(
                &request,
                WordSelection {
                    head_utf16: head,
                    backward_bias: pointer.hit.backward_bias,
                    anchor,
                    cell: None,
                },
                Some(pointer.intent.clone()),
                permit,
            )?;
            return Ok(true); // 原CellSource/UBA已接纳同一词请求，等待其结果不再重送本次点击。
        }
        let scene = match self.scene() {
            Ok(scene) => scene,
            Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(false),
            Err(cause) => return Err(cause),
        };
        if scene.origin.state != origin.state || !scene.key.request.same(&request) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        // 单击只交真实命中端点；三击使用子Source已认证段落，软换行不等同于段落边界。
        // 段落尚未进入当前窗口时保持等待，不能把已显示的后缀当成完整段落。
        let target = match pointer.clicks {
            0 | 1 => MarkdownTextSelection {
                anchor_utf16: anchor.unwrap_or(head),
                head_utf16: head,
            },
            3 => match scene
                .scene
                .paragraph_selection(head, pointer.hit.head_affinity)
            {
                Ok(selection) => selection,
                Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(false),
                Err(cause) => return Err(cause),
            },
            _ => return Err(NativeMarkdownPagedTextViewError::UnsupportedCommand),
        };
        // 原亲和与本次选择同时发布；之后的预览样式重排不能抹掉本次显式头侧。
        // 这里只改变选择，不为点按制造正文历史，也不提前宣告文件已保存。
        let outcome = origin.input.set_selection_intent(
            &permit,
            target,
            Some(if pointer.clicks == 3 {
                true
            } else {
                pointer.hit.head_affinity.is_upstream()
            }),
            None,
            &current,
        )?;
        self.finish_outcome(&origin, outcome)?;
        // 不装载子view的drag槽：真实Down仍由原父view拥有，Up后迟到Ready仅完成终点选择。
        Ok(true)
    }

    /// 只把已解释的真实格命中交唯一父Body；来源、选择许可与Down寿命在同一同步栈核对。
    /// 词种子和Shift锚点沿原指针槽读取，Native挂载重入后也不覆盖后继Down/Up。
    fn hand_cell_pointer(
        &self,
        scene: &super::SceneUse,
        hit: super::NativeMarkdownPagedCellHit,
        clicks: usize,
        extend: bool,
        dragging: bool,
    ) -> Result<bool> {
        let origin = &scene.origin;
        let pointer = self.ivars().drag_anchor.get();
        let word = clicks == 2;
        // 链接已在上方接管；这里只传本次真实命中及原选择种子，不重播Down或合成坐标。
        let anchor = if dragging {
            match pointer {
                Some(DragAnchor::Caret {
                    anchor_utf16,
                    generation,
                }) if generation == origin.state.generation => Some(anchor_utf16),
                Some(DragAnchor::Caret { .. }) => {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Some(DragAnchor::Word {
                    anchor: WordAnchor::Selection(anchor),
                    ..
                }) => Some(anchor),
                Some(DragAnchor::Word { .. }) => None,
                None => return Err(NativeMarkdownPagedTextViewError::Revoked),
            }
        } else if extend {
            Some(
                origin
                    .state
                    .directed_selection()
                    .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                    .anchor_utf16,
            )
        } else {
            None
        };
        // 手势若已发生正文变更就撤旧词种子；Source offset相等也不代表仍是原字符。
        // Shift种子与词种子保持原角色，交给child或父跨格生产者后再做坐标域转换。
        let word_anchor = if word && dragging {
            let Some(DragAnchor::Word { anchor, generation }) = pointer else {
                unreachable!("原Word手势已确认");
            };
            if generation != scene.origin.state.generation {
                self.cancel_cell_pointer_intent();
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            Some(anchor)
        } else {
            anchor.filter(|_| word).map(WordAnchor::Selection)
        };
        // 只领取当前View这一张手势身份；raw来源随payload同步交付，View不长期保存它。
        // 下一锚点预先按真实命中准备，委托若拒绝不写入；Native移交完成后还要复核原票。
        let intent = self.claim_cell_pointer_intent();
        let next_anchor = if word {
            DragAnchor::Word {
                anchor: word_anchor.unwrap_or(WordAnchor::Word {
                    head_utf16: hit.head_utf16,
                    backward_bias: hit.backward_bias,
                    cell: hit.word_target,
                }),
                generation: scene.origin.state.generation,
            }
        } else {
            DragAnchor::Caret {
                anchor_utf16: anchor.unwrap_or(hit.head_utf16),
                generation: origin.state.generation,
            }
        };
        let cell_pointer = super::NativeMarkdownPagedCellPointer {
            hit,
            clicks: if word { 2 } else { clicks },
            extending: extend,
            anchor,
            word_anchor,
            dragging,
            intent: std::rc::Rc::downgrade(&intent),
        };
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // Input许可与Scene同一before；Body如果接纳会取得真正的Row/RequiredRow权限，不能由View造票。
        // 返回true只表示原owner接手，是否已出现编辑框仍由实际Source/CPU帧通知决定。
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        if scene
            .origin
            .delegate
            .cell_pointer(&scene.key.request, cell_pointer, permit)?
        {
            // Native移交可重入；只有原Down尚未Up且没有后继手势替换时才登记拖选种子。
            let owns = self
                .ivars()
                .cell_pointer_intent
                .borrow()
                .as_ref()
                .is_some_and(|active| std::rc::Rc::ptr_eq(active, &intent));
            if !dragging && owns && origin.input.state()?.generation == origin.state.generation {
                self.ivars().drag_anchor.set(Some(next_anchor));
            } // 旧格提交可真实换代；此时后继点等Reopen的已映射首查询，不安装旧位置/旧generation。
            return Ok(true);
        }
        if !dragging {
            self.cancel_cell_pointer_intent();
        } // 非Body客户没有活动格owner，继续原普通Source选择，不把未接纳误报成功。
        Ok(false)
    }

    pub(super) fn mouse(&self, event: &NSEvent, dragging: bool) {
        let forward = self.ivars().cell_pointer_forward.borrow().clone();
        if let Some(forward) = forward {
            // 这是此后真实到达旧child的新回调，尚未读点/修饰键或解释选择；只交一次当前父view主线。
            // 新Down不能继承已移交旧手势，关闭view也不再尝试读取已seal的原Input。
            if dragging
                && let Some(parent) = forward.parent.upgrade()
                && parent.cell_pointer_is_current(&forward.intent)
            {
                parent.handle_pointer(event, true);
            } else {
                self.ivars().cell_pointer_forward.borrow_mut().take();
            }
            return;
        }
        if self.ivars().closed.get() {
            return; // 原Up已经结束弱路由，迟到回调只到物理空view，不能再读已归还Input。
        }
        // 原 Up 已交回槽，或真实转交刚结束而物理拆图还在原主队列中；迟到 Drag 没有新的解释权。
        // 必须在读取 Origin/已 seal 草稿之前结束，不能依赖后继 close 来遮住这段实际生命周期。
        if dragging
            && self.ivars().drag_anchor.get().is_none()
            && self.ivars().cell_pointer_intent.borrow().is_none()
        {
            return;
        }
        self.handle_pointer(event, dragging);
    }

    /// 原回调只解码一次有限终点；逻辑种子仍来自同一 Down，未命中的初 Down 不猜词范围。
    /// Up 准入时仍持原 Rc，之后终点由 Body 明确记录，不能靠 Weak 活性区分结束与取消。
    fn pointer_location(
        &self,
        origin: &super::Origin,
        event: &NSEvent,
        dragging: bool,
        mouse_up: bool,
    ) -> Result<super::NativeMarkdownPagedPointerLocation> {
        use super::NativeMarkdownPagedCellDragAnchor as Anchor;
        let anchor = match self.ivars().drag_anchor.get() {
            Some(DragAnchor::Word { anchor, generation }) if dragging => {
                if generation != origin.state.generation {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Some(Anchor::Word(anchor))
            }
            Some(DragAnchor::Caret {
                anchor_utf16,
                generation,
            }) if dragging => {
                if generation != origin.state.generation {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Some(Anchor::Caret { anchor_utf16 })
            }
            _ => None,
        };
        let window_point = origin.step(self, || event.locationInWindow())?;
        let flags = origin.step(self, || event.modifierFlags())?;
        let clicks = origin.step(self, || event.clickCount())?;
        let clicks = usize::try_from(clicks)
            .map_err(|_| NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        if !window_point.x.is_finite() || !window_point.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let extending = dragging || flags.contains(NSEventModifierFlags::Shift);
        let anchor = if !dragging && extending {
            let anchor = origin
                .state
                .directed_selection()
                .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                .anchor_utf16;
            Some(if clicks == 2 {
                Anchor::Word(WordAnchor::Selection(anchor))
            } else {
                Anchor::Caret {
                    anchor_utf16: anchor,
                }
            })
        } else {
            anchor
        };
        let intent = if dragging || mouse_up {
            self.ivars()
                .cell_pointer_intent
                .borrow()
                .clone()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?
        } else {
            self.claim_cell_pointer_intent()
        };
        Ok(super::NativeMarkdownPagedPointerLocation {
            window_point,
            clicks: if matches!(anchor, Some(Anchor::Word(_))) {
                2
            } else {
                clicks
            },
            extending,
            anchor,
            dragging,
            intent: std::rc::Rc::downgrade(&intent),
            mouse_up,
        })
    }

    /// 有限值在同一已 Ready 的真实 Scene 上只解释空间命中与原选择；同步事件和延后终点共用此主线。
    /// 输入许可仍紧贴实际提交领取，格接纳与正文选择二者只消费一个真实用户动作。
    fn apply_pointer(
        &self,
        scene: &super::SceneUse,
        pointer: &super::NativeMarkdownPagedPointerLocation,
    ) -> Result<()> {
        if pointer.mouse_up && !pointer.dragging {
            // 无实际移动的Up只结束已接纳Down。选择会激活quote/inline源码标记并改变真实几何，
            // 若再命中原屏幕点，会把原caret错误变成邻字选区；首Down仍由原续体完整完成。
            return Ok(());
        }
        let local = scene.step(self, || {
            self.convertPoint_fromView(pointer.window_point, None)
        })?;
        if !local.x.is_finite() || !local.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let local = if pointer.dragging {
            let point = scene.scene.drag_selection_point(local)?;
            if !scene.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            point
        } else {
            local
        }; // 真实拖选用已准备行，普通点击仍保持精确命中；物理点仍由原槽保存。
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        use super::NativeMarkdownPagedCellDragAnchor as Anchor;
        if pointer.clicks == 2 || matches!(pointer.anchor, Some(Anchor::Word(_))) {
            return self.apply_word_pointer(scene, pointer, local);
        }
        let anchor = match pointer.anchor {
            Some(Anchor::Caret { anchor_utf16 }) => Some(anchor_utf16),
            None => None,
            Some(Anchor::Word(_)) => unreachable!("真实Word已交上方原选词消费者"),
        };
        if let Some(hit) = scene.scene.cell_hit(local, false)? {
            let next_anchor = DragAnchor::Caret {
                anchor_utf16: anchor.unwrap_or(hit.head_utf16),
                generation: scene.origin.state.generation,
            };
            let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
            if scene.origin.delegate.cell_pointer(
                &scene.key.request,
                super::NativeMarkdownPagedCellPointer {
                    hit,
                    clicks: pointer.clicks,
                    extending: pointer.extending,
                    anchor,
                    word_anchor: None,
                    dragging: pointer.dragging,
                    intent: pointer.intent.clone(),
                },
                permit,
            )? {
                if !pointer.mouse_up
                    && !pointer.dragging
                    && self.cell_pointer_is_current(&pointer.intent)
                    && scene.origin.input.state()?.generation == scene.origin.state.generation
                {
                    self.ivars().drag_anchor.set(Some(next_anchor));
                }
                return Ok(()); // 真正挂载或跨格提交由同一个父 owner 完成，不再落入旧 Scene。
            }
        }
        let (target, affinity) =
            scene
                .scene
                .mouse_selection(local, pointer.clicks, anchor, pointer.extending)?;
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        if scene.origin.delegate.extend_cell_pointer(
            &scene.key.request,
            super::NativeMarkdownPagedCellExtension::Selection {
                selection: target,
                head_affinity: affinity,
                intent: pointer.intent.clone(),
            },
            permit,
        )? {
            return Ok(());
        }
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        let outcome = scene.origin.input.set_selection_intent(
            &permit,
            target,
            Some(affinity.is_upstream()),
            None,
            &current,
        )?;
        self.finish_outcome(&scene.origin, outcome)?;
        if !pointer.mouse_up && !pointer.dragging && self.cell_pointer_is_current(&pointer.intent) {
            self.ivars().drag_anchor.set(Some(DragAnchor::Caret {
                anchor_utf16: target.anchor_utf16,
                generation: scene.origin.state.generation,
            }));
        } // 已接纳 Up 只完成终点；不能凭异步完成重新装回拖动槽。
        Ok(())
    }

    /// 同步和延后 Word 共用实际 Scene 命中；首组只来自原生产者，目标不得重建旧首词。
    fn apply_word_pointer(
        &self,
        scene: &super::SceneUse,
        pointer: &super::NativeMarkdownPagedPointerLocation,
        local: objc2_foundation::NSPoint,
    ) -> Result<()> {
        use super::NativeMarkdownPagedCellDragAnchor as Anchor;
        let anchor = match pointer.anchor {
            Some(Anchor::Word(anchor)) => Some(anchor),
            None if !pointer.dragging => None,
            _ => return Err(NativeMarkdownPagedTextViewError::Pending),
        };
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        if let Some(hit) = scene.scene.cell_hit(local, true)? {
            let next = anchor.unwrap_or(WordAnchor::Word {
                head_utf16: hit.head_utf16,
                backward_bias: hit.backward_bias,
                cell: hit.word_target,
            });
            let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
            if scene.origin.delegate.cell_pointer(
                &scene.key.request,
                super::NativeMarkdownPagedCellPointer {
                    hit,
                    clicks: 2,
                    extending: pointer.extending,
                    anchor: match anchor {
                        Some(WordAnchor::Selection(anchor)) => Some(anchor),
                        _ => None,
                    },
                    word_anchor: anchor,
                    dragging: pointer.dragging,
                    intent: pointer.intent.clone(),
                },
                permit,
            )? {
                if !pointer.dragging
                    && !pointer.mouse_up
                    && self.cell_pointer_is_current(&pointer.intent)
                    && scene.origin.input.state()?.generation == scene.origin.state.generation
                {
                    self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                        anchor: next,
                        generation: scene.origin.state.generation,
                    }));
                }
                return Ok(());
            }
        }
        let (head_utf16, backward_bias, cell) = scene.scene.word_hit(local)?;
        let selection = WordSelection {
            head_utf16,
            backward_bias,
            anchor,
            cell,
        };
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        if scene.origin.delegate.extend_cell_pointer(
            &scene.key.request,
            super::NativeMarkdownPagedCellExtension::Word {
                selection,
                intent: pointer.intent.clone(),
            },
            permit,
        )? {
            return Ok(());
        }
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        scene.origin.delegate.select_word(
            &scene.key.request,
            selection,
            Some(pointer.intent.clone()),
            permit,
        )?;
        if !pointer.dragging && !pointer.mouse_up && self.cell_pointer_is_current(&pointer.intent) {
            self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                anchor: anchor.unwrap_or(WordAnchor::Word {
                    head_utf16,
                    backward_bias,
                    cell,
                }),
                generation: scene.origin.state.generation,
            }));
        }
        Ok(())
    }

    /// 唯一 Body 续体在真实 Scene 发布后调用；没有保存或重送 Native 事件，也不另起命中生产者。
    /// false 只说明原窗口尚未 Ready；新输入/坐标域或几何改变必须撤销，不能给旧终点补签。
    pub(in crate::native_surface) fn apply_deferred_pointer(
        &self,
        request: &super::NativeMarkdownPagedSceneRequest,
        pointer: &super::NativeMarkdownPagedPointerLocation,
        accepted_up: Option<&super::NativeMarkdownPagedPointerLocation>,
    ) -> Result<bool> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let original_up = accepted_up.is_some_and(|terminal| {
            terminal.mouse_up && std::rc::Weak::ptr_eq(&terminal.intent, &pointer.intent)
        }); // 只消费原Body槽真实接纳的终点；首Down仍保持原角色，不从Weak死亡猜终止原因。
        if !pointer.mouse_up && !original_up && !self.cell_pointer_is_current(&pointer.intent) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let scene = match self.scene() {
            Ok(scene) => scene,
            Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(false),
            Err(cause) => return Err(cause),
        };
        if !scene.key.request.same(request) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        self.apply_pointer(&scene, pointer)?;
        Ok(true)
    }

    /// 子格越界只交本次已解码终点；来源域仍是该 child request，父亲不能把局部种子当 canonical。
    pub(super) fn hand_child_pointer(
        &self,
        origin: &super::Origin,
        pointer: &super::NativeMarkdownPagedPointerLocation,
    ) -> Result<bool> {
        if !origin.input.is_cell() {
            return Ok(false);
        }
        let request = self.scene_request(origin)?;
        let local = origin.step(self, || {
            self.convertPoint_fromView(pointer.window_point, None)
        })?;
        if !local.x.is_finite() || !local.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let viewport = request.viewport;
        if local.x >= viewport.origin.x
            && local.y >= viewport.origin.y
            && local.x < viewport.origin.x + viewport.size.width
            && local.y < viewport.origin.y + viewport.size.height
        {
            return Ok(false);
        }
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        origin
            .delegate
            .drag_cell_pointer_outside(&request, pointer.clone(), permit)
    }

    /// Scene 尚未 Ready 时，原 permit 与已解码终点一起交给已有 Body 槽；拒绝后保持真实 Pending。
    pub(super) fn defer_pointer_location(
        &self,
        origin: &super::Origin,
        pointer: super::NativeMarkdownPagedPointerLocation,
    ) -> Result<()> {
        let request = self.scene_request(origin)?;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if origin.delegate.defer_pointer(&request, pointer, permit)? {
            Ok(())
        } else {
            Err(NativeMarkdownPagedTextViewError::Pending)
        }
    }

    fn handle_pointer(&self, event: &NSEvent, dragging: bool) {
        let drag_intent = dragging
            .then(|| {
                self.ivars()
                    .cell_pointer_intent
                    .borrow()
                    .as_ref()
                    .map(std::rc::Rc::downgrade)
            })
            .flatten(); // 任何Native getter之前抓原Down，后续期限不能借重入新Down补签旧事件。
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let draining_reopen = !dragging && self.cancel_cell_pointer_intent();
        self.cancel_link_intent(); // 后继按下或开始拖动即撤链接权限，但原Caret/Word拖选锚点继续归同一指针槽。
        let result = (|| {
            let origin = self.origin()?;
            if origin.delegate.handle_cell_marked_mouse(event)? {
                return Ok(()); // 活动格候选先消费原事件；父view尚未移焦、取Scene或提交草稿。
            }
            if !origin.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if origin.state.marked.is_some() {
                // IME拥有组合期间的鼠标；不在它提交后给同一个旧点击补签较新的scene。
                let (_, context) = self.foreground_context()?;
                context.0.handleEvent(event);
                if !origin.session_current(self, origin.epoch) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                return Ok(());
            }
            let point = origin.step(self, || event.locationInWindow())?;
            if !point.x.is_finite() || !point.y.is_finite() {
                return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
            }
            if dragging {
                if let Some(mut motion) = self.ivars().pointer_motion.get() {
                    motion.dragged = true; // 真实Drag回调是移动事实，不能由选择或重排推算。
                    self.ivars().pointer_motion.set(Some(motion));
                }
            } else {
                self.ivars().pointer_motion.set(Some(PointerMotion {
                    down: point,
                    dragged: false,
                    clicks: usize::try_from(origin.step(self, || event.clickCount())?)
                        .map_err(|_| NativeMarkdownPagedTextViewError::InvalidGeometry)?,
                }));
            }
            if draining_reopen {
                // 旧来源仍在归槽时，新Down只交自己真实request与有限点；不能丢事件或抢第二个请求。
                let pointer = self.pointer_location(&origin, event, false, false)?;
                return self.defer_pointer_location(&origin, pointer);
            }
            if dragging {
                let pointer = self.pointer_location(&origin, event, true, false)?;
                if self.hand_child_pointer(&origin, &pointer)? {
                    return Ok(());
                }
                if matches!(
                    pointer.anchor,
                    Some(super::NativeMarkdownPagedCellDragAnchor::Word(_))
                ) || self.ivars().drag_anchor.get().is_none()
                {
                    return self.defer_pointer_location(&origin, pointer);
                } // 同一个槽保存原Down和最新Word点，未Ready首组不得被这次目标覆盖。
            }
            let scene = match self.scene() {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    let pointer = self.pointer_location(&origin, event, dragging, false)?;
                    return self.defer_pointer_location(&origin, pointer);
                }
                Err(cause) => return Err(cause),
            };
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
            if !dragging && !extend && clicks == 1 {
                let selected = origin.state.primary_range;
                let inside_selection = if selected.length == 0 {
                    false
                } else {
                    let position = scene.scene.hit(local)?;
                    position >= selected.location && position <= selected.location + selected.length
                }; // 已选文本内的单击仍保原选择/拖动语义，不由链接抢走该手势。
                if !inside_selection && let Some((part, index)) = scene.scene.link_hit(local)? {
                    let anchor = scene.scene.hit(local)?;
                    if !scene.current(self) {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    // 链接 Down 仍是一次真实的选择手势：只登记原身份，不改 canonical 或提前打开目标。
                    // 后续 Drag 可以撤链接权限后沿此 Down 继续，不能等到 Drag 才为旧锚点补造身份。
                    let _intent = self.claim_cell_pointer_intent();
                    *self.ivars().link_intent.borrow_mut() = Some(std::rc::Rc::new(()));
                    *self.ivars().pressed_link.borrow_mut() = Some(PressedLink {
                        key: scene.key.clone(),
                        part,
                        index,
                    });
                    self.ivars().drag_anchor.set(Some(DragAnchor::Caret {
                        anchor_utf16: anchor,
                        generation: origin.state.generation,
                    }));
                    return Ok(()); // 不改canonical选择，原链接publication不会被mousedown自己撤销；拖动仍走下方原选择逻辑。
                }
            }
            let pointer = self.ivars().drag_anchor.get();
            if dragging && pointer.is_none() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            } // Up已清原槽；迟到Drag不能从当前选择新建一次不存在的Down。
            let word = (!dragging && clicks == 2)
                || (dragging && matches!(pointer, Some(DragAnchor::Word { .. })));
            let local = if word && dragging {
                let point = scene.scene.drag_selection_point(local)?;
                if !scene.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                point
            } else {
                local
            };
            if word
                && let Some(hit) = scene.scene.cell_hit(local, true)?
                && self.hand_cell_pointer(
                    &scene,
                    hit,
                    if word { 2 } else { clicks },
                    extend,
                    dragging,
                )?
            {
                return Ok(());
            }
            if (!dragging && clicks == 2)
                || (dragging && matches!(pointer, Some(DragAnchor::Word { .. })))
            {
                let (head_utf16, backward_bias, cell) = scene.scene.word_hit(local)?;
                if !scene.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let anchor = if dragging {
                    let Some(DragAnchor::Word { anchor, generation }) = pointer else {
                        unreachable!("本手势已确认原Word指针槽");
                    };
                    if generation != origin.state.generation {
                        self.ivars().drag_anchor.set(None);
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    } // 正文编辑后旧点击坐标失权，不把它映射到新文稿。
                    Some(anchor)
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
                    };
                    // 原Shift锚点和首击词种子只有一种真实角色，不在同槽保无效的另一组坐标。
                    self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                        anchor: extend_anchor.map_or(
                            WordAnchor::Word {
                                head_utf16,
                                backward_bias,
                                cell,
                            },
                            WordAnchor::Selection,
                        ),
                        generation: origin.state.generation,
                    }));
                    extend_anchor.map(WordAnchor::Selection)
                };
                let selection = WordSelection {
                    head_utf16,
                    backward_bias,
                    anchor,
                    cell,
                };
                let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
                let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
                let intent = self.claim_cell_pointer_intent();
                if scene.origin.delegate.extend_cell_pointer(
                    &scene.key.request,
                    super::NativeMarkdownPagedCellExtension::Word {
                        selection,
                        intent: std::rc::Rc::downgrade(&intent),
                    },
                    permit,
                )? {
                    return Ok(()); // 跨格词拖选先交原owner提交，旧Scene不能消费原permit。
                }
                let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
                return scene.origin.delegate.select_word(
                    &scene.key.request,
                    selection,
                    Some(std::rc::Rc::downgrade(&intent)),
                    permit,
                );
            }
            // 普通 Caret 与延后终点使用同一个真实空间/选择消费者；Word 仍在上方原链完成。
            let pointer = self.pointer_location(&origin, event, dragging, false)?;
            self.apply_pointer(&scene, &pointer)?;
            Ok(())
        })();
        self.record(result);
        if let Some(intent) = drag_intent {
            self.update_drag_scroll(event, &intent); // 只保原Down最后一个有限窗口点，原事件在回调后归还。
        }
    }

    /// 一个实际Down/Drag共用原view唯一手势槽；只保存身份，不保存raw票、Source或系统事件。
    fn claim_cell_pointer_intent(&self) -> std::rc::Rc<()> {
        let active = self.ivars().cell_pointer_intent.borrow().clone();
        active.unwrap_or_else(|| {
            let intent = std::rc::Rc::new(());
            *self.ivars().cell_pointer_intent.borrow_mut() = Some(intent.clone());
            intent
        })
    }

    /// 只核原View仍持这一次Down；同步栈临时Rc即使仍活，也不能在真实Up后给弱票续命。
    pub(in crate::native_surface) fn cell_pointer_is_current(
        &self,
        intent: &std::rc::Weak<()>,
    ) -> bool {
        !self.ivars().closed.get()
            && self
                .ivars()
                .cell_pointer_intent
                .borrow()
                .as_ref()
                .is_some_and(|active| {
                    std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent)
                })
    }

    /// 原Core发布成功后、拆child图之前移动唯一手势Rc；后续真实Drag/Up仍可能被Cocoa发给旧view。
    /// child已seal时不能再借它的Input；只核parent实际成功结果和原槽，Up先到则明确不移交。
    pub(in crate::native_surface) fn transfer_cell_pointer_to(
        &self,
        parent: &std::rc::Rc<objc2::rc::Retained<Self>>,
        after: &NativeMarkdownPagedInputOutcome,
        intent: &std::rc::Weak<()>,
        mapped_word_anchor: Option<WordAnchor>,
    ) -> Result<bool> {
        if !self.cell_pointer_is_current(intent) {
            return Ok(false);
        }
        let origin = parent.origin()?;
        let state = match after {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                commit.check_current(&|epoch| origin.session_current(parent, epoch))?;
                commit.state
            }
            NativeMarkdownPagedInputOutcome::Unchanged(state) => *state,
        };
        if origin.state != state || !origin.current(parent) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let next = DragAnchor::from_published_selection(state, mapped_word_anchor)?;
        // 原parent仍持另一次真实Down时不能覆盖它，也不把旧child票补签给后继动作。
        // 检查和槽移动之间没有Native/current调用，主线程无需锁或第二张手势登记图。
        if parent
            .ivars()
            .cell_pointer_intent
            .borrow()
            .as_ref()
            .is_some_and(|active| !std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent))
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let token = {
            let mut slot = self.ivars().cell_pointer_intent.borrow_mut();
            if slot.as_ref().is_none_or(|active| {
                !std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent)
            }) {
                return Ok(false);
            }
            slot.take().expect("同一原Down已核存在")
        };
        *parent.ivars().cell_pointer_intent.borrow_mut() = Some(token);
        parent.ivars().drag_anchor.set(Some(next));
        parent
            .ivars()
            .pointer_motion
            .set(self.ivars().pointer_motion.take());
        self.ivars().drag_anchor.set(None);
        *self.ivars().cell_pointer_forward.borrow_mut() = Some(CellPointerForward {
            parent: std::rc::Rc::downgrade(parent),
            intent: intent.clone(),
        });
        self.transfer_drag_scroll_to(parent); // 纯手势移交结束后才处理Native期限，重入只看完整的新owner。
        Ok(true) // 手势和同趟mapped种子已原子移交；Native detach之前没有旧局部端点或空槽间隙。
    }

    /// 原词生产者已交付完整首组；只把真实范围写回仍属于同一个 Down 的唯一 Word 槽。
    /// request 是实际选择完成后的同域请求，不能用旧 serial 或 child-local 整数给父槽续权。
    /// Up 后允许原选词完成，但没有存活 Down 时返回 false，不造 Commit 或重新开启 Drag。
    pub(in crate::native_surface) fn accept_pointer_word_group(
        &self,
        request: &super::NativeMarkdownPagedSceneRequest,
        group: std::ops::Range<usize>,
        intent: &std::rc::Weak<()>,
    ) -> Result<bool> {
        if !self.cell_pointer_is_current(intent) {
            return Ok(false);
        }
        let origin = self.origin()?;
        if !self.scene_request(&origin)?.same(request) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if group.start > group.end || group.end > origin.state.utf16_len {
            return Err(NativeMarkdownPagedTextViewError::InvalidRange);
        }
        let Some(DragAnchor::Word { anchor, generation }) = self.ivars().drag_anchor.get() else {
            return Ok(false); // 同一 View 的 Caret 或已结束手势不能被词结果改成 Word。
        };
        if generation != origin.state.generation {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let origin_utf16 = match anchor {
            WordAnchor::Selection(_) => return Ok(true), // Shift 的真实标量锚点始终保持原角色。
            WordAnchor::Word { head_utf16, .. }
                if group.start <= head_utf16 && head_utf16 <= group.end =>
            {
                head_utf16
            }
            WordAnchor::Group {
                from_utf16,
                to_utf16,
                origin_utf16,
            } if group == (from_utf16..to_utf16) => origin_utf16,
            _ => return Err(NativeMarkdownPagedTextViewError::Revoked),
        }; // 首击位置只保原同组方向；真正范围始终来自刚完成的 Core 结果。

        // Native 几何读取可能重入；最后只在实际原票和同一输入仍活时替换两个纯值端点。
        if self.cell_pointer_is_current(intent) && origin.current(self) {
            self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                anchor: WordAnchor::Group {
                    from_utf16: group.start,
                    to_utf16: group.end,
                    origin_utf16,
                },
                generation,
            }));
            return Ok(true);
        }
        Ok(false)
    }

    /// 草稿发布后的真实选择收据交回同一次鼠标；只替换已映射的拖选种子，不制造输入提交。
    /// Up、新Down或键盘已撤原弱票时直接结束，不能在迟到Ready后复活手势。
    pub(in crate::native_surface) fn adopt_cell_pointer_selection(
        &self,
        commit: &super::NativeMarkdownPagedInputCommit,
        intent: &std::rc::Weak<()>,
        word_anchor: Option<WordAnchor>,
    ) -> Result<()> {
        let owns = || self.cell_pointer_is_current(intent);
        if !owns() {
            return Ok(());
        }
        // 当前View必须仍是这张已提交收据的真实输入客户；显示后果失败不能回滚已发布正文。
        // 这里只调整指针元数据，不再发Native坐标通知或触碰原字节。
        let origin = self.origin()?;
        commit.check_current(&|epoch| origin.session_current(self, epoch))?;
        if origin.state != commit.state || !commit.state.selection_is_directed {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let next = DragAnchor::from_published_selection(commit.state, word_anchor)?;
        if owns() && origin.current(self) {
            self.ivars().drag_anchor.set(Some(next));
        } // current可重入，最后一次核验仍属原手势后才允许改变原槽。
        Ok(())
    }

    /// 只撤这次格指针寿命；pending选择可结束，但后继Ready不能重新装载drag锚点。
    pub(super) fn cancel_cell_pointer_intent(&self) -> bool {
        let drag_scroll = self.take_drag_scroll(); // 先提取旧期限；Native invalidate只能在原槽已撤完后发生。
        self.ivars().pointer_motion.set(None);
        drop(self.ivars().cell_pointer_intent.borrow_mut().take());
        self.ivars().drag_anchor.set(None); // 原Down寿命已结束，后继Drag不能借当前选择重建旧种子。
        // 已接纳 Up 以后 View 的 Rc 可以为空；取消仍必须明确撤掉 Body 中尚未落地的终点。
        let delegate = self
            .ivars()
            .delegate
            .borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade);
        let draining = delegate.is_some_and(|delegate| delegate.cancel_deferred_pointer());
        drop(drag_scroll); // 原timer析构可重入，新Down已经有独立槽，不再被旧清理误撤。
        draining
    }

    /// 撤票只结束这个原手势的寿命；异步正文最多持Weak，无法因此保活View/Source/字体图。
    pub(super) fn cancel_link_intent(&self) {
        drop(self.ivars().pressed_link.borrow_mut().take());
        drop(self.ivars().link_intent.borrow_mut().take());
    }

    /// 真正的mouseup才交接单击意图；原mousedown后出现drag、选择/几何或焦点变化均不补签。
    pub(super) fn finish_link_mouse(&self, event: &NSEvent) {
        let forward = self.ivars().cell_pointer_forward.borrow_mut().take();
        if let Some(forward) = forward {
            if let Some(parent) = forward.parent.upgrade()
                && parent.cell_pointer_is_current(&forward.intent)
            {
                parent.complete_pointer(event);
            }
            return; // 真实Up同时归还旧child弱联系；迟到Drag不再有Input或新Down的解释权。
        }
        if self.ivars().closed.get() {
            return;
        }
        self.complete_pointer(event);
    }

    /// Up的唯一解释主线；移交后的旧child不再开自己的链接或访问草稿，仅父view结束同一手势。
    fn complete_pointer(&self, event: &NSEvent) {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let _drag_scroll = self.take_drag_scroll(); // 先撤期限槽，原Up完成后才invalidate旧timer，不能重入误认新Down。
        // Up 自己也有真实终点，不能仅清槽而丢掉快于排版完成的最后一个位置。
        // 准入发生在原 Down 仍由本 View 拥有时；之后撤 Rc 只结束拖动寿命，不撤已接纳终点。
        let pointer_intent = self
            .ivars()
            .cell_pointer_intent
            .borrow()
            .as_ref()
            .map(std::rc::Rc::downgrade);
        let pressed = self.ivars().pressed_link.borrow_mut().take();
        let link_intent = self
            .ivars()
            .link_intent
            .borrow()
            .as_ref()
            .map(std::rc::Rc::downgrade);
        let has_link = pressed.is_some();
        if let Some(pointer_intent) = pointer_intent {
            let result = (|| {
                if !self.cell_pointer_is_current(&pointer_intent) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let origin = self.origin()?;
                if !has_link {
                    let point = origin.step(self, || event.locationInWindow())?;
                    let moved = self
                        .ivars()
                        .pointer_motion
                        .get()
                        .is_none_or(|motion| motion.dragged || motion.down != point);
                    let pointer = self.pointer_location(&origin, event, moved, true)?;
                    if !std::rc::Weak::ptr_eq(&pointer.intent, &pointer_intent)
                        || !self.cell_pointer_is_current(&pointer_intent)
                    {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    if !moved {
                        // 保留原Up的接纳事实供尚在准备的Down/首词组使用，不重新命中已改变的几何。
                        // 无异步owner的独立客户返回false即可；它的同步Down已经决定选择。
                        let request = self.scene_request(&origin)?;
                        let permit = origin.input.capture(origin.epoch, &|epoch| {
                            epoch == origin.epoch && origin.current(self)
                        })?;
                        origin.delegate.defer_pointer(&request, pointer, permit)?;
                    } else if !self.hand_child_pointer(&origin, &pointer)? {
                        if matches!(
                            pointer.anchor,
                            Some(super::NativeMarkdownPagedCellDragAnchor::Word(_))
                        ) || self.ivars().drag_anchor.get().is_none()
                        {
                            self.defer_pointer_location(&origin, pointer)?;
                        } else {
                            match self.scene() {
                                Ok(scene) => self.apply_pointer(&scene, &pointer)?,
                                Err(NativeMarkdownPagedTextViewError::Pending) => {
                                    self.defer_pointer_location(&origin, pointer)?
                                }
                                Err(cause) => return Err(cause),
                            }
                        }
                    }
                }
                if !self.cell_pointer_is_current(&pointer_intent) {
                    // 真正的child提交可能已把同手势移到父view；原Up终点已交给那个唯一owner，
                    // 不再从已seal的旧Input取新origin，也不重放这个系统事件。
                    return Ok(());
                }
                // 上方可以真实发布选择；结束通知只借提交后的当前请求，不给旧 InputState 续权。
                let after = self.origin()?;
                let request = self.scene_request(&after)?;
                after.delegate.finish_cell_pointer(&request)
            })();
            let owned = self.end_pointer_metadata(&pointer_intent);
            if !owned {
                let forward = self.ivars().cell_pointer_forward.borrow().clone();
                if let Some(forward) = forward
                    && std::rc::Weak::ptr_eq(&forward.intent, &pointer_intent)
                {
                    self.ivars().cell_pointer_forward.borrow_mut().take();
                    if let Some(parent) = forward.parent.upgrade() {
                        parent.end_pointer_metadata(&pointer_intent);
                    }
                } // Up栈内发生真实移交也结束原父手势；后续迟到Drag不能借父Rc复活已抬起的鼠标。
            }
            if self.ivars().cell_pointer_intent.borrow().is_none() {
                self.record(result);
            } // Native重入的新Down有自己的错误归属，旧Up不清新元数据或覆盖新客户反馈。
        }
        let Some(pressed) = pressed else {
            return;
        };
        let Some(intent) = link_intent else {
            return;
        };
        let result = (|| {
            if !self.link_intent_current(&pressed.key, &intent)? {
                return Ok(());
            }
            let scene = self.scene()?;
            if !scene.key.same(&pressed.key) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let point = scene.step(self, || event.locationInWindow())?;
            let local = scene.step(self, || self.convertPoint_fromView(point, None))?;
            // 真实mouseDragged已在入口撤票；mouseup离开任何LINK可见墨迹也不得打开原按下目标。
            if !scene
                .scene
                .link_hit(local)?
                .is_some_and(|(_, index)| pressed.part.contains(&index))
            {
                self.retire_link_intent(&intent);
                return Ok(());
            }
            if !self.link_intent_current(&pressed.key, &intent)? {
                self.retire_link_intent(&intent);
                return Ok(());
            }
            scene.scene.follow_link(pressed.index, intent.clone())
        })();
        if result.is_err() {
            self.retire_link_intent(&intent);
        }
        let current_link = self
            .ivars()
            .link_intent
            .borrow()
            .as_ref()
            .map(std::rc::Rc::downgrade);
        if current_link
            .as_ref()
            .is_none_or(|current| std::rc::Weak::ptr_eq(current, &intent))
        {
            self.record(result); // Native清理后再核反馈归属，旧链接不能给重入新Down写旧错误。
        }
    }

    /// 只结束指定的实际手势；先撤全部纯元数据，最后才归还其Native期限，重入的新Down不受旧Up影响。
    fn end_pointer_metadata(&self, intent: &std::rc::Weak<()>) -> bool {
        if !self.cell_pointer_is_current(intent) {
            return false;
        }
        let timer = self.take_drag_scroll();
        self.ivars().drag_anchor.set(None);
        self.ivars().pointer_motion.set(None);
        drop(self.ivars().cell_pointer_intent.borrow_mut().take());
        drop(timer);
        true
    }

    /// 异步目标只借原手势弱票、原Scene/输入/几何和真实firstResponder；任何Native重入后重新核同票。
    pub(in crate::native_surface) fn link_intent_current(
        &self,
        key: &super::NativeMarkdownPagedSceneKey,
        intent: &std::rc::Weak<()>,
    ) -> Result<bool> {
        let owns = || {
            self.ivars()
                .link_intent
                .borrow()
                .as_ref()
                .is_some_and(|active| {
                    std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent)
                })
        };
        if !owns() || self.ivars().closed.get() {
            return Ok(false);
        }
        let origin = self.origin()?;
        let actual = self.scene_request(&origin)?;
        if !actual.same(&key.request) || !origin.delegate.scene_current(key) || !owns() {
            return Ok(false);
        }
        let window = origin.step(self, || self.window())?;
        let Some(window) = window else {
            return Ok(false);
        };
        if !origin.step(self, || window.isKeyWindow())? {
            return Ok(false); // 另一窗口取得键盘焦点时，旧window仍可记住firstResponder，不能据此续权。
        }
        let responder = origin.step(self, || window.firstResponder())?;
        let focused = responder.is_some_and(|responder| {
            std::ptr::eq::<objc2_app_kit::NSResponder>(&*responder, self.as_super().as_super())
        });
        drop(window); // Native临时强claim先归还，析构回调也不能发生在最后同票核验之后。
        Ok(focused && owns() && origin.delegate.scene_current(key) && origin.current(self))
    }

    /// 只消费仍属原click的票；Native重入产生的新手势不能被旧操作的清理出口清空。
    pub(in crate::native_surface) fn retire_link_intent(&self, intent: &std::rc::Weak<()>) {
        let matches = self
            .ivars()
            .link_intent
            .borrow()
            .as_ref()
            .is_some_and(|active| std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent));
        if matches {
            self.cancel_link_intent();
        }
    }

    /// 原真实target准备完成后才激活一次；take在系统副作用之前，失败和退激活均不重放。
    pub(in crate::native_surface) fn finish_link_activation(
        &self,
        key: &super::NativeMarkdownPagedSceneKey,
        intent: &std::rc::Weak<()>,
        destination: &str,
    ) {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let result = (|| {
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            let value = objc2_foundation::NSString::from_str(destination);
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            let url = objc2_foundation::NSURL::URLWithString(&value);
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            let Some(url) = url else {
                return Err(NativeMarkdownPagedTextViewError::LinkOpenFailed);
            };
            let scheme = url.scheme();
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            if scheme.is_none() {
                let fragment = url.fragment();
                if !self.link_intent_current(key, intent)? {
                    return Ok(false);
                }
                if fragment.is_some() {
                    return Err(NativeMarkdownPagedTextViewError::LinkAnchorUnsupported);
                }
                let query = url.query();
                if !self.link_intent_current(key, intent)? {
                    return Ok(false);
                }
                if query.is_some() {
                    return Err(NativeMarkdownPagedTextViewError::RelativeNoteLink(
                        "相对笔记链接不支持查询参数".into(),
                    ));
                }
                let host = url.host();
                if !self.link_intent_current(key, intent)? {
                    return Ok(false);
                }
                if host.is_some() {
                    return Err(NativeMarkdownPagedTextViewError::RelativeNoteLink(
                        "不能通过笔记链接访问网络文件路径".into(),
                    ));
                }
                let path = url.path().map(|path| path.to_string());
                if !self.link_intent_current(key, intent)? {
                    return Ok(false);
                }
                let path = path.filter(|path| !path.is_empty()).ok_or_else(|| {
                    NativeMarkdownPagedTextViewError::RelativeNoteLink(
                        "笔记链接没有有效的相对路径".into(),
                    )
                })?;
                crate::native_surface::queue_native_markdown_relative_note_link(
                    self, key, intent, &path,
                )?;
                return Ok(true); // 队列只借原click弱票；当前栈退出后才允许旧正文保存和资源barrier切换。
            }
            let local_file = url.isFileURL();
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            if local_file {
                return Err(NativeMarkdownPagedTextViewError::RelativeNoteLink(
                    "本地笔记请使用笔记库内的相对路径".into(),
                ));
            } // 本地文件不能绕过原工作区限域、未保存提交及释放流程交给系统另开。
            let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            let scene = self.scene()?;
            scene.scene.validate_ready(&|| scene.current(self))?;
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            self.retire_link_intent(intent);
            if workspace.openURL(&url) {
                Ok(false)
            } else {
                Err(NativeMarkdownPagedTextViewError::LinkOpenFailed)
            }
        })();
        if !matches!(result, Ok(true)) {
            self.retire_link_intent(intent); // 真queued相对导航必须等消费时take；这里先撤会让第一次click永久失权。
        }
        if let Err(cause) = result {
            self.report_link_failure(key, cause);
        }
    }

    /// 仅原工作区owner显示具体链接失败；同时保存原typed原因，不能把不支持的导航伪装成成功。
    pub(in crate::native_surface) fn report_link_failure(
        &self,
        key: &super::NativeMarkdownPagedSceneKey,
        cause: NativeMarkdownPagedTextViewError,
    ) {
        crate::native_surface::present_native_markdown_paged_link_error(self, key, &cause);
        self.failure(cause); // 链接失败不销毁仍可编辑的正文/Source，也不会自动重放系统副作用。
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
