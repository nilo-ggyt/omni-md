//! 单格提交共用已验证源计划；字段确认、Fold发布与后继激活沿同一个原动作权限。

use super::*;
use omni_markdown_core::markdown_document_buffer::MarkdownTableWindowTarget;

/// 纯提交后果只带原编辑标量和pre-notify票；不保留字段、storage或正文强owner。
pub(in crate::native_surface) struct NativeMarkdownTableCellCommit {
    pub(in crate::native_surface) changed: bool,
    pub(in crate::native_surface) edit: Option<MarkdownHistoryTextEdit>,
    pub(in crate::native_surface) committed: Option<Rc<NativeMarkdownCommittedSelection>>,
    // 只有原 Shift-click 要求且真实字段方向已核准时返回；坐标来自同一次序列化与 verified plan。
    pub(in crate::native_surface) field_anchor: Option<usize>,
}

/// 只处理已准入的一格原始草稿；标记来自 Core，UTF16 选择必须位于真实 scalar 边界。
fn format_table_cell_input(
    source: &str,
    selection: NSRange,
    action: MarkdownFormatAction,
) -> Result<(String, std::ops::Range<usize>), String> {
    let (opening, closing, placeholder) = action.inline_markers().ok_or("该格式需要正文块位置")?;
    let end = selection
        .location
        .checked_add(selection.length)
        .ok_or("格内选择溢出")?;
    let mut units = 0;
    let mut start_byte = None;
    let mut end_byte = None;
    for (byte, scalar) in source.char_indices() {
        // 从同一有限字段逐 scalar 换算两个真实端点；不把 UTF16 索引误作 UTF8 字节索引。
        if units == selection.location {
            start_byte = Some(byte);
        }
        if units == end {
            end_byte = Some(byte);
        }
        units += scalar.len_utf16();
    }
    if units == selection.location {
        start_byte = Some(source.len()); // 文本尾 caret 是合法边界。
    }
    if units == end {
        end_byte = Some(source.len());
    }
    let (start_byte, end_byte) = start_byte.zip(end_byte).ok_or("格内选择不在有效文字边界")?;
    let inner = if selection.length == 0 {
        placeholder // 空 caret 沿原命令选择占位文字，不格式化整格。
    } else {
        &source[start_byte..end_byte] // 非空选择只包裹实际选中的原 Unicode 字节。
    };
    let extra = if selection.length == 0 {
        inner.len()
    } else {
        0
    };
    let mut formatted = String::with_capacity(source.len() + opening.len() + closing.len() + extra);
    formatted.push_str(&source[..start_byte]);
    formatted.push_str(opening);
    let inner_start = selection.location + opening.encode_utf16().count();
    formatted.push_str(inner);
    let inner_end = inner_start + inner.encode_utf16().count();
    formatted.push_str(closing);
    formatted.push_str(&source[end_byte..]);
    native_markdown_sanitize_table_cell_input_with_selection(&formatted, inner_start..inner_end)
}

impl PieceBackedMarkdownTextView {
    /// 普通提交和模式前置提交均须拥有活动格的真实窗口；不会同步解析整个表来补权限。
    pub(super) fn commit_verified_table_cell(
        &self,
        mode: Option<&super::super::table_layout::NativeMarkdownModeViewportContext>,
        request: Option<&NativeMarkdownTableCellRequest>,
    ) -> Result<bool, String> {
        self.commit_verified_table_cell_result(mode, request, None, false, &|| true, &|_| {})
            .map(|result| result.changed)
    }

    /// 不同格click沿自身原权限提交；后续只能消费此次返回的同一提交收据和编辑映射。
    pub(in crate::native_surface) fn commit_table_cell_for_click(
        &self,
        origin_current: &impl Fn() -> bool,
        on_selection_committed: &impl Fn(&Rc<NativeMarkdownCommittedSelection>),
    ) -> Result<NativeMarkdownTableCellCommit, String> {
        self.commit_verified_table_cell_result(
            None,
            None,
            None,
            false,
            origin_current,
            on_selection_committed,
        )
    }

    /// 原活动字段的跨格 Shift 要求同一次提交返回精确 anchor；外方不能指定最终源范围。
    pub(super) fn commit_table_cell_for_shift_click(
        &self,
        origin_current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownTableCellCommit, String> {
        self.commit_verified_table_cell_result(None, None, None, true, origin_current, &|_| {})
    }

    /// 格内行内格式与未提交草稿共用一次真实单格计划；原字段选择不落回旧正文 caret。
    pub(in crate::native_surface) fn format_table_cell_for_menu(
        &self,
        intent: &NativeMarkdownWorkspaceEditIntent,
        action: MarkdownFormatAction,
    ) -> Result<(), String> {
        self.commit_verified_table_cell_result(
            None,
            None,
            Some((intent, action)),
            false,
            &|| intent.current(), // 原菜单的正文/窗口权限跨到结束尾部，不以已辞任的 field editor 续权。
            &|receipt| intent.accept_table_commit(receipt),
        )
        .map(|_| ())
    }

    fn commit_verified_table_cell_result(
        &self,
        mode: Option<&super::super::table_layout::NativeMarkdownModeViewportContext>,
        request: Option<&NativeMarkdownTableCellRequest>,
        format: Option<(&NativeMarkdownWorkspaceEditIntent, MarkdownFormatAction)>,
        map_field_anchor: bool,
        origin_current: &impl Fn() -> bool,
        on_selection_committed: &impl Fn(&Rc<NativeMarkdownCommittedSelection>),
    ) -> Result<NativeMarkdownTableCellCommit, String> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let pending_tail = RefCell::new(None); // 正常字段结束、body after 和全部 Native claim 析构共用同原 owner。
        let epoch = self.ivars().table_input_intent_epoch.get();
        let fold = self.ivars().folds.revision();
        let selection_capture = match format {
            Some((intent, _)) => intent.source_capture(), // 格内菜单必须消费首 getter 前的原正文 before。
            None => self
                .ivars()
                .folds
                .capture_selection()
                .map_err(|error| error.to_string())?,
        };
        let Some(state) = self.ivars().active_table_cell.borrow().clone() else {
            if format.is_some() {
                return Err("原表格单元格已经结束，格式操作已取消".into());
            } // 不能把已丢失的格内命令降为正文格式或成功空操作。
            return Ok(NativeMarkdownTableCellCommit {
                changed: false,
                edit: None,
                committed: None,
                field_anchor: None,
            });
        };
        let authority = state
            .authority
            .as_ref()
            .map(Rc::clone)
            .ok_or("表格单元格源权限尚未就绪，草稿将保持打开")?;
        let committed: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>> = RefCell::new(None);
        let ending = Cell::new(false);
        let committed_edit = RefCell::new(None);
        let field_anchor = Cell::new(None);
        let current = || {
            if !origin_current()
                || self.ivars().textkit_graph_teardown.get()
                || self.ivars().table_input_intent_epoch.get() != epoch
            {
                return false;
            }
            if !ending.get() && format.is_some_and(|(intent, _)| !intent.table_input_current(self))
            {
                return false; // 结束阶段开始前必须保持原真实格内选择；之后由同一源票约束 abort/隐藏。
            }
            if request
                .is_some_and(|request| !self.ivars().pending_table_cell_activation.owns(request))
            {
                return false; // 同epoch的局部也必须仍拥有原槽，不能在另一关闭/模式路径清槽后继续。
            }
            if mode.is_some_and(|mode| mode.native_step(self, || ()).is_err()) {
                return false;
            }
            if let Some(receipt) = committed.borrow().as_ref() {
                return receipt.current();
            }
            selection_capture.current()
                && self.ivars().folds.read_is_current(fold)
                && authority.document.upgrade().is_some_and(|document| {
                    authority
                        .source_range(&document.borrow(), state.coordinate)
                        .ok()
                        .as_ref()
                        == Some(&state.source_utf16_range)
                })
                && (ending.get()
                    || self
                        .ivars()
                        .active_table_cell
                        .borrow()
                        .as_ref()
                        .is_some_and(|active| {
                            active.coordinate == state.coordinate
                                && active.source_utf16_range == state.source_utf16_range
                        }))
        };
        let expired = || "表格输入或所属窗口已改变，草稿将保持打开".to_owned();
        let storage =
            native_markdown_table_intent_step(&current, || self.ivars().text_storage.load())
                .map_err(|_| expired())?
                .ok_or_else(expired)?;
        let captured_current = current;
        let current =
            || captured_current() && request.is_none_or(|request| request.current(self, &storage)); // 每个后续Native/计划/提交都守原width/mode/root；自身提交只由原pre-notify票和一代key更新放行。
        let identity = storage.ivars().document.borrow().clone();
        if !std::rc::Weak::ptr_eq(&identity, &authority.document)
            || request.is_some_and(|request| !request.current(self, &storage))
        {
            return Err(expired());
        }
        let editor =
            native_markdown_table_intent_step(&current, || self.existing_table_cell_editor())
                .map_err(|_| expired())?
                .ok_or_else(expired)?;
        let mutation = if mode.is_none() {
            native_markdown_table_intent_step(&current, || self.capture_table_mutation_anchor())
                .map_err(|_| expired())?
        } else {
            None
        }; // 模式已有自己的原源锚点，不先用另一锚点移动视口。
        let actual = native_markdown_table_intent_step(&current, || editor.currentEditor())
            .map_err(|_| expired())?
            .and_then(|value| value.downcast::<NSTextView>().ok());
        let click_selection = if map_field_anchor {
            Some(
                state
                    .field_selection
                    .ok_or("原格内选择方向尚未明确，草稿将保持打开；请先在格内定位光标")?,
            )
        } else {
            None // 普通提交与格式沿既有选择契约，不额外解释字段方向。
        };
        let check_click_selection = || -> Result<(), String> {
            let Some(selection) = click_selection else {
                return Ok(());
            };
            let actual = actual.as_ref().ok_or("原表格字段已结束，草稿将保持打开")?;
            let ranges = native_markdown_table_intent_step(&current, || actual.selectedRanges())
                .map_err(|_| expired())?;
            let count = native_markdown_table_intent_step(&current, || ranges.count())
                .map_err(|_| expired())?;
            let range = native_markdown_table_intent_step(&current, || actual.selectedRange())
                .map_err(|_| expired())?;
            drop(ranges);
            if !current()
                || count != 1
                || native_markdown_table_field_selection(None, range, Some(selection), false)
                    .is_none()
            {
                return Err("原格内选择已改变，草稿将保持打开".into());
            }
            Ok(())
        };
        check_click_selection()?; // 任何 IME 确认之前先核原字段范围，未知外来选择不被提交。
        if let Some(actual) = actual.as_ref() {
            if let Some((intent, _)) = format {
                intent.table_selection_for(self, actual)?; // IME 确认前先核原实际 editor 与子选区。
            }
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**actual);
            if native_markdown_table_intent_step(&current, || input.hasMarkedText())
                .map_err(|_| expired())?
            {
                self.expected_table_field_operation(
                    &editor,
                    actual,
                    NativeMarkdownTableFieldOperation::Unmark,
                    &current,
                    || input.unmarkText(),
                )?;
            }
            if native_markdown_table_intent_step(&current, || input.hasMarkedText())
                .map_err(|_| expired())?
            {
                return Err("表格单元格的输入尚未完成，草稿将保持打开".into());
            }
            self.expected_table_field_operation(
                &editor,
                actual,
                NativeMarkdownTableFieldOperation::Validate,
                &current,
                || editor.validateEditing(),
            )?;
        } else {
            if format.is_some() {
                return Err("原表格字段已不再编辑，草稿将保持打开".into());
            } // 无实际 field editor 就没有格内选择，不能猜一个全格或正文范围。
            native_markdown_table_intent_step(&current, || editor.validateEditing())
                .map_err(|_| expired())?;
        }
        let field_selection = match format {
            Some((intent, _)) => {
                Some(intent.table_selection_for(self, actual.as_ref().ok_or_else(expired)?)?)
            }
            None => None, // 普通提交只使用既有 caret after，不读取无关字段选择。
        }; // unmark/validate 只确认同一文本；若同时移动选区就撤销旧菜单，不借新选择续命。
        check_click_selection()?; // Unmark/Validate 只准确认同一范围；不能借后继新 caret 重签旧点击。
        let (replacement, cell_selection_after, sanitized_anchor) = (|| {
            let value = native_markdown_table_intent_step(&current, || editor.stringValue())
                .map_err(|_| expired())?;
            if native_markdown_table_intent_step(&current, || value.length())
                .map_err(|_| expired())?
                > 64 * 1024
            {
                return Err("表格单元格输入超过预算，草稿将保持打开".to_owned());
            }
            autoreleasepool(|pool| {
                // SAFETY: 实际单格已按UTF16预算准入；借用不跨pool，返回唯一有限replacement。
                let text =
                    native_markdown_table_intent_step(&current, || unsafe { value.to_str(pool) })
                        .map_err(|_| expired())?;
                match (format, field_selection) {
                    (Some((_, action)), Some(selection)) => {
                        let (replacement, selection) =
                            format_table_cell_input(text, selection, action)?;
                        Ok((replacement, Some(selection), None))
                    }
                    _ => {
                        if let Some(selection) = click_selection {
                            let (replacement, mapped) =
                                native_markdown_sanitize_table_cell_input_with_selection(
                                    text,
                                    selection.anchor_utf16..selection.anchor_utf16,
                                )?;
                            Ok((replacement, None, Some(mapped.start))) // 同次实际 escape/trim 输出带回 anchor，不复制字段。
                        } else {
                            Ok((native_markdown_sanitize_table_cell_input(text), None, None)) // 原普通提交保持原序列化。
                        }
                    }
                }
            })
        })()?;
        if let (Some((intent, _)), Some(actual)) = (format, actual.as_ref()) {
            intent.table_selection_for(self, actual)?; // stringValue/UTF16 借用返回后仍是原选区才准入计划。
        }
        check_click_selection()?;
        drop(actual); // 字段局部Nativeclaims析构后重新核原动作，不能继承析构中出现的新输入。
        if !current() {
            return Err(expired());
        }
        let changed = replacement != state.original_source;
        if !changed && let Some(anchor) = sanitized_anchor {
            field_anchor.set(Some(
                state
                    .source_utf16_range
                    .start
                    .checked_add(anchor)
                    .ok_or("表格选择溢出")?,
            ));
        } // 未改草稿仍精确返回同源 anchor；不为选择映射制造字符代次或 Undo。
        if changed {
            let document = authority.document.upgrade().ok_or_else(expired)?;
            let observed = native_markdown_table_intent_step(&current, || {
                self.native_ordered_selection_state_for_capture(&selection_capture)
            })
            .map_err(|_| expired())?
            .map_err(|error| error.to_string())?;
            let selection_origin = match observed {
                NativeMarkdownOrderedSelection::Single(_) => selection_capture.clone(),
                NativeMarkdownOrderedSelection::Multiple => {
                    // 真实 cell 激活已授予该格源位置；用明确 caret 收拢 body Multi，绝不猜其主 head。
                    let before = document.borrow().selection();
                    let caret = {
                        let document = document.borrow();
                        authority.source_range(&document, state.coordinate)?.start
                    };
                    let target = selection_capture
                        .clone()
                        .finish(MarkdownTextSelection::caret(caret), true)
                        .map_err(|error| error.to_string())?;
                    let accepted = Cell::new(true);
                    let receipt = storage
                        .update_active_selection_observed(target, |receipt| {
                            // 同一纯转换 callback 是唯一续接点；保留所有 origin/mode/request 门禁，不领新 capture。
                            *committed.borrow_mut() = Some(Rc::clone(receipt));
                            if let Some(request) = request {
                                *request.committed.borrow_mut() = Some(Rc::clone(receipt));
                            }
                            if let Some(mode) = mode {
                                accepted.set(mode.accept_cell_selection(self, before, receipt));
                            }
                            on_selection_committed(receipt);
                        })
                        .ok_or_else(expired)?;
                    if !accepted.get()
                        || !current()
                        || !self.apply_committed_selection_if(&receipt, current)
                    {
                        return Err("单元格选择已收拢，但原提交动作已撤销；草稿仍保留".into());
                    }
                    receipt
                        .captured_selection()
                        .map_err(|error| error.to_string())?
                        .native_capture()
                        .clone()
                }
            };
            let (plan, baseline, selection_after) = {
                let document = document.borrow();
                authority.source_range(&document, state.coordinate)?;
                let plan = document
                    .plan_verified_table_cell_edit(
                        &authority.window,
                        state.coordinate.row_index,
                        state.coordinate.column_index,
                        &replacement,
                    )
                    .map_err(|_| "无法准备已验证的单元格替换，草稿将保持打开")?;
                let (range, text) = plan.replacement_for(&document).map_err(|_| expired())?;
                // 从计划真实序列化拆出补格前缀；不能把原 raw UTF16 直接加到源起点。
                let prefix = text
                    .strip_suffix(&replacement)
                    .ok_or("单格计划未包含原序列化结果")?;
                let start = range
                    .start
                    .checked_add(prefix.encode_utf16().count())
                    .ok_or("表格位置溢出")?;
                let end = start
                    .checked_add(replacement.encode_utf16().count())
                    .ok_or("表格位置溢出")?;
                if let Some(anchor) = sanitized_anchor {
                    field_anchor.set(Some(start.checked_add(anchor).ok_or("表格选择溢出")?));
                } // implicit-empty 前缀只由本计划补一次；不拿旧 cell.start 猜新锚点。
                let selection_after = cell_selection_after
                    .as_ref()
                    .map(|selection| {
                        Ok::<_, String>(MarkdownTextSelection {
                            anchor_utf16: start
                                .checked_add(selection.start)
                                .ok_or("表格选择溢出")?,
                            head_utf16: start.checked_add(selection.end).ok_or("表格选择溢出")?,
                        })
                    })
                    .transpose()?;
                (plan, start..end, selection_after)
            };
            let mode_accepted = Cell::new(true);
            // 单格字段与正文各有自己的选择；把原票下实际正文 before 传到同一次字符提交。
            let captured =
                self.capture_ordered_native_before(selection_origin, &storage, &current)?;
            let (edit, _, pending) = storage.apply_planned_table_replacement(
                &captured,
                |document| {
                    plan.replacement_for(document)
                        .map(|(range, text)| (range, text, selection_after))
                        .map_err(|_| expired())
                },
                &current,
                |edit, receipt| {
                    // 纯发布早于任何字符通知：旧window失效，后继输入看到的是新基线而不是待重复提交的旧草稿。
                    *committed.borrow_mut() = Some(Rc::clone(receipt));
                    *committed_edit.borrow_mut() = Some(edit.clone());
                    on_selection_committed(receipt); // 同一 pre-notify after 续接原菜单/click，不重领选择。
                    if let Some(request) = request {
                        let mut key = request.key.get();
                        if let (Some(generation), Some(length)) = (
                            key.document_generation.checked_add(1),
                            key.document_length
                                .checked_add_signed(edit.change_in_utf16_length),
                        ) {
                            key.document_generation = generation;
                            key.document_length = length;
                            request.key.set(key);
                            *request.committed.borrow_mut() = Some(Rc::clone(receipt));
                        }
                    }
                    if let Some(active) = self
                        .ivars()
                        .active_table_cell
                        .borrow_mut()
                        .as_mut()
                        .filter(|active| active.coordinate == state.coordinate)
                    {
                        active.source_utf16_range = baseline.clone();
                        active.original_source.clone_from(&replacement);
                        active.authority = None; // 新代次必须重新取得opaque窗口，不能重用g0 metadata。
                    }
                    if let Some(mode) = mode {
                        mode_accepted.set(mode.accept_cell_commit(self, edit, receipt));
                    }
                },
            )?;
            drop(pending_tail.replace(pending));
            if !mode_accepted.get() || !current() {
                return Err("单元格已提交，但后继输入已撤销本次动作；正文与撤销历史已保留".into());
            }
            if mode.is_none() {
                self.arm_table_mutation_anchor(
                    mutation,
                    &edit.replaced_utf16_range,
                    edit.change_in_utf16_length,
                );
            }
        }
        if !current() {
            return Err(expired());
        }
        // 此前已核原字段选择和完整提交结果；从这里开始只结束该动作，不再向 field editor 询问选择。
        // 无字符变化仍有原源/正文选择/epoch权限，不伪造字符收据来跨越自身的 Native 辞任。
        ending.set(true);
        native_markdown_table_intent_step(&current, || editor.abortEditing())
            .map_err(|_| expired())?;
        // abortEditing 的 bool 只表示调用前是否有关联 editor；false 也可能是合法的已结束字段。
        // 只有实际关联已清除才撤格身份；不能用返回 bool 把正常 Return 或已辞任字段误判成失败。
        let actual = native_markdown_table_intent_step(&current, || editor.currentEditor())
            .map_err(|_| expired())?;
        let still_editing = actual.is_some();
        drop(actual); // 临时关联 claim 的析构也必须先结束，再核原尾部权限和实际结果。
        if !current() {
            return Err(expired());
        }
        if still_editing {
            // 实际 editor 仍关联时保留身份和完整草稿；已经发生的字符提交不能重放。
            return Err(if changed {
                "单元格内容已提交，但字段未能结束编辑；草稿将保持打开".into()
            } else {
                "单元格未能结束编辑，草稿将保持打开".into()
            });
        }
        let previous = self.ivars().active_table_cell.borrow_mut().take();
        drop(previous); // 原 epoch 已跨 abort 复核；现在才撤身份，后续 clear 通知不能再次提交草稿。
        native_markdown_table_intent_step(&current, || editor.setHidden(true))
            .map_err(|_| expired())?;
        native_markdown_table_intent_step(&current, || editor.setStringValue(ns_string!("")))
            .map_err(|_| expired())?;
        native_markdown_table_intent_step(&current, || self.setNeedsDisplayInRect(state.frame))
            .map_err(|_| expired())?;
        if changed {
            native_markdown_table_intent_step(&current, || {
                self.invalidate_visible_table_layout_cache()
            })
            .map_err(|_| expired())?;
            native_markdown_table_intent_step(&current, || self.invalidate_visible_body())
                .map_err(|_| expired())?;
            if request.is_none() && mode.is_none() {
                self.schedule_visible_table_layout_update();
                self.finish_table_mutation_continuity();
            }
            let receipt = committed
                .borrow()
                .as_ref()
                .map(Rc::clone)
                .ok_or("单元格提交缺少原收据")?;
            if !self.apply_committed_selection_if(&receipt, current) {
                return Err("单元格已提交，但正文选择的原安装动作已撤销".into());
            } // 字段已正常结束；同一字符收据明确安装 body after，不增加历史或 autosave。
        }
        Ok(NativeMarkdownTableCellCommit {
            changed,
            edit: committed_edit.into_inner(),
            committed: committed.into_inner(),
            field_anchor: field_anchor.get(),
        })
    }

    /// 目标存在才结束当前草稿；提交后的RequiredRow只沿原pre-notify收据推进一代。
    pub(super) fn commit_table_cell_for_request(
        &self,
        request: &NativeMarkdownTableCellRequest,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> Result<(), String> {
        if !request.current(self, storage) {
            return Err("表格导航已撤销".into());
        }
        let original_matches = {
            let action = request.action.borrow();
            let active = self.ivars().active_table_cell.borrow();
            matches!((&*action, active.as_ref()),
                (NativeMarkdownTableCellAction::Navigate { original, .. }, Some(active))
                if original.coordinate == active.coordinate
                    && original.source_utf16_range == active.source_utf16_range)
        };
        if !original_matches {
            return Err("原表格草稿已被后继动作替换".into());
        }
        let changed = self.commit_verified_table_cell(None, Some(request))?;
        if !request.current(self, storage) {
            return Err("表格内容已提交，但导航已经撤销".into());
        }
        let previous = request
            .action
            .replace(NativeMarkdownTableCellAction::Activate);
        drop(previous);
        if changed {
            let document = request.identity.upgrade().ok_or("表格所属文档已关闭")?;
            let next = {
                let document = document.borrow();
                let selection = document.selection();
                document
                    .table_window_request(
                        MarkdownTableWindowTarget::RequiredRow {
                            header_byte_start: request.header_byte_start,
                            row_ordinal: request.target.row_index,
                        },
                        0..0,
                        request.fold_revision,
                        selection.anchor_utf16.min(selection.head_utf16)
                            ..selection.anchor_utf16.max(selection.head_utf16),
                    )
                    .map_err(|_| "无法准备提交后的真实目标行")?
            };
            let ready = request.ready.borrow_mut().take();
            drop(ready);
            *request.core.borrow_mut() = Some(next);
            request.phase.set(NativeMarkdownTableCellPhase::Resolving);
        } else {
            // 同一g0目标已完整Ready，无编辑不制造额外代次或重复Parser请求。
            request.phase.set(NativeMarkdownTableCellPhase::Ready);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_cell_format_preserves_real_unicode_subselection_and_escaped_pipe() {
        // UTF16 位置 2 从“👩”开始；ZWJ 序列与选中 pipe/汉字共同移动，未选中的“甲”不加粗。
        let (source, selection) = format_table_cell_input(
            " 甲👩‍💻|乙 \r\n ",
            NSRange::new(2, 7),
            MarkdownFormatAction::Bold,
        )
        .unwrap();
        assert_eq!(source, "甲**👩‍💻\\|乙**");
        assert_eq!(selection, 3..11); // trim 一单位，opening 两单位，pipe 序列化增加一单位。
    }

    #[test]
    fn inline_cell_format_can_restore_original_source_without_a_character_edit() {
        // 原格为“**甲**”，用户在真实字段删掉标记后选中“甲”再次加粗，结果仍是原 canonical 源。
        // 此输入合法且没有字符差异；结束字段必须沿原动作完成，不能靠多造一次编辑/Undo 取得尾部权限。
        let original = "**甲**";
        let (replacement, selection) =
            format_table_cell_input("甲", NSRange::new(0, 1), MarkdownFormatAction::Bold).unwrap();
        assert_eq!(replacement, original);
        assert_eq!(selection, 2..3);
    }

    #[test]
    fn empty_cell_caret_selects_only_the_original_link_placeholder() {
        let (source, selection) =
            format_table_cell_input("甲乙", NSRange::new(1, 0), MarkdownFormatAction::Link)
                .unwrap();
        assert_eq!(source, "甲[链接文字](https://)乙");
        assert_eq!(selection, 2..6); // 标签占位四单位，不包含 opening、URL 或相邻正文。
    }

    #[test]
    fn cell_serialization_maps_crlf_unicode_trim_and_backslash_parity() {
        let source = "\u{2003}\t甲\r\n乙|丙\\|丁\\\\|戊  ";
        let start = source[..source.find('乙').unwrap()].encode_utf16().count();
        let end = source.trim_end().encode_utf16().count();
        let (serialized, selection) =
            native_markdown_sanitize_table_cell_input_with_selection(source, start..end).unwrap();
        assert_eq!(serialized, "甲 乙\\|丙\\|丁\\\\\\|戊");
        assert_eq!(selection, 2..serialized.encode_utf16().count());
    }

    #[test]
    fn click_anchor_uses_the_same_escape_trim_and_unicode_output() {
        // caret 在 emoji 后；前导空白被 trim、pipe 真正多写一条 slash，两个变化共同决定实际源锚点。
        let draft = "  甲|😀乙\r\n ";
        let (serialized, anchor) =
            native_markdown_sanitize_table_cell_input_with_selection(draft, 6..6).unwrap();
        assert_eq!(serialized, "甲\\|😀乙");
        assert_eq!(anchor, 5..5);
        // 反向选择的 anchor 可位于右端；producer 接受具体单点，不把 ordered range.start 冒作 anchor。
        let (same, right_anchor) =
            native_markdown_sanitize_table_cell_input_with_selection(draft, 7..7).unwrap();
        assert_eq!(same, serialized);
        assert_eq!(right_anchor, 6..6);
    }

    #[test]
    fn unchanged_click_anchor_preserves_the_real_source_endpoint() {
        let original = "甲\\|😀乙";
        let (serialized, anchor) =
            native_markdown_sanitize_table_cell_input_with_selection(original, 5..5).unwrap();
        assert_eq!(serialized, original); // 相同字符结果不得为锚点映射制造第二份 Undo。
        assert_eq!(anchor, 5..5);
    }

    #[test]
    fn cell_format_rejects_a_selection_inside_a_surrogate_pair() {
        assert!(
            format_table_cell_input("😀", NSRange::new(1, 0), MarkdownFormatAction::Italic,)
                .is_err()
        ); // 不将非法端点扩到整格或相邻 scalar。
        assert!(native_markdown_sanitize_table_cell_input_with_selection("😀", 1..1).is_err());
        // 直接序列化入口同样拒绝代理项中点；不能只靠格式调用者提前拦截。
    }

    #[test]
    fn cell_serialization_keeps_empty_trimmed_selection_and_plain_entry_identical() {
        let source = "\u{2003}\r\n\t ";
        let (serialized, selection) =
            native_markdown_sanitize_table_cell_input_with_selection(source, 1..4).unwrap();
        assert_eq!(
            serialized,
            native_markdown_sanitize_table_cell_input(source)
        );
        assert!(serialized.is_empty());
        assert_eq!(selection, 0..0); // 全部空白都由实际 trim 删除，合法原选区成为真实空 caret。
    }

    #[test]
    fn every_inline_cell_command_keeps_unselected_prefix_and_suffix() {
        for action in [
            MarkdownFormatAction::Bold,
            MarkdownFormatAction::Italic,
            MarkdownFormatAction::Underline,
            MarkdownFormatAction::Strikethrough,
            MarkdownFormatAction::InlineCode,
            MarkdownFormatAction::InlineFormula,
            MarkdownFormatAction::Superscript,
            MarkdownFormatAction::Link,
        ] {
            // 同一真实中部选区走所有现有行内动作；提取选择后仍为原“中😀”而非整格。
            let (source, selection) =
                format_table_cell_input("前中😀后", NSRange::new(1, 3), action).unwrap();
            assert!(source.starts_with('前') && source.ends_with('后'));
            let units: Vec<_> = source.encode_utf16().collect();
            assert_eq!(String::from_utf16(&units[selection]).unwrap(), "中😀");
        }
    }
}
