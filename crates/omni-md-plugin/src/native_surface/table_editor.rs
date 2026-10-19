//! Owns the one reusable native table-cell draft and semantic spreadsheet navigation.
//!
//! Commit failures preserve the visible draft; cancel restores canonical display. Cell identity is
//! resolved again after edits rather than retaining stale byte offsets or an entire table body.

use super::*;

mod click;
pub(super) use click::NativeMarkdownTableCellPointer;
mod cell_text_layout;
pub(super) use cell_text_layout::NativeMarkdownTableCellTextLayout;
mod commit;
#[cfg(feature = "native-markdown-runtime-probe")]
mod observation;
mod row_request;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use observation::{
    NativeMarkdownTableCellOutcome, NativeMarkdownTableCellReceipt,
    NativeMarkdownTableInsertionObservation,
};
use row_request::*;
pub(super) use row_request::{
    NativeMarkdownPendingTableCell, NativeMarkdownTableCellAction,
    NativeMarkdownTableCellAuthority, NativeMarkdownTableCellIntent, NativeMarkdownTableCellPoll,
};
#[path = "table_navigation/field_input.rs"]
mod field_input;
pub(super) use field_input::{
    NativeMarkdownTableFieldExpectation, NativeMarkdownTableFieldOperation,
};

impl PieceBackedMarkdownTextView {
    /// Resolves a mouse point against already-built visible-cell geometry without reparsing source.
    pub(super) fn visible_table_cell_at_point(
        &self,
        point: NSPoint,
    ) -> Option<NativeMarkdownVisibleTableCellLayout> {
        // 读取native key时不借cache；宽度/高度代次变化后旧rect立即失去命中权限。
        let request_key = self.current_table_layout_request_key()?;
        let storage = self.ivars().text_storage.load()?;
        let identity = storage.ivars().document.borrow().clone();
        let cache = self.ivars().visible_table_layout_cache.borrow();
        let cache = cache.as_ref()?;
        if cache.request_key != request_key
            || !std::rc::Weak::ptr_eq(&cache.document_identity, &identity)
        {
            // 同generation的另一文档或旧geometry均不能激活原canonical范围。
            return None;
        }
        cache
            .tables
            .iter()
            .flat_map(|table| table.cells.iter())
            .find(|cell| NSPointInRect(point, cell.frame))
            .cloned()
    }

    /// 点击只沿原矩阵权限提交前格；新格须重新取得同一点击源范围的已验证窗口。
    pub(super) fn activate_table_cell(
        &self,
        cell: NativeMarkdownVisibleTableCellLayout,
        event: &NSEvent,
    ) {
        self.activate_table_cell_from_original_click(cell, event);
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// 验收沿生产行请求观察实际激活；普通插表操作直接交接自身提交收据，不重领输入意图。
    fn request_table_cell_activation_observed(
        &self,
        coordinate: NativeMarkdownTableCellCoordinate,
        observer: Option<Rc<NativeMarkdownTableCellReceipt>>,
    ) -> bool {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        self.cancel_table_continuity_for_new_input();
        let epoch = self.ivars().table_input_intent_epoch.get();
        let fold = self.ivars().folds.revision();
        match self.start_required_table_cell_observed(
            coordinate,
            NativeMarkdownTableCellAction::Activate,
            NativeMarkdownTableCellIntent::uncommitted(epoch, fold),
            None,
            &|| true,
            observer,
        ) {
            Ok(()) => true, // 接受的是异步行请求；此时没有字段或绘制完成权限。
            Err(error) => {
                if self.ivars().table_input_intent_epoch.get() == epoch
                    && self.ivars().folds.read_is_current(fold)
                {
                    present_native_markdown_error(&error);
                }
                false
            }
        }
    }

    /// Resolves a pending semantic cell against the current cache without keeping stale source offsets.
    pub(super) fn activate_pending_table_cell(&self) -> bool {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let Some(request) = self.ivars().pending_table_cell_activation.request() else {
            return false;
        };
        if request.phase.get() == NativeMarkdownTableCellPhase::Selecting {
            return true; // 原票正在真实正文扩选；布局回调不能把它重新变成全格编辑。
        }
        let activated = self.activate_pending_table_cell_inner();
        // 内层cache、cell、storage、window等Native局部全部析构后，才签发原请求唯一成功终点。
        if activated && request.activation_is_current(self) {
            // 只复制刚核准的原证据；clear后不能再要求owns(slot)，也不能从当前源重领权限。
            let identity = request.identity.clone();
            let key = request.key.get();
            let committed = request.committed.borrow().as_ref().map(Rc::clone);
            self.ivars()
                .pending_table_cell_activation
                .clear_if_same(&request);
            // 外层Rc使clear不会最终析构request；此处无Native，仍以原owner/Weak/key/提交票复核。
            let still_current = !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == request.input_epoch
                && committed.as_ref().map_or_else(
                    || self.ivars().folds.read_is_current(request.fold_revision),
                    |receipt| receipt.current(),
                )
                && identity.upgrade().is_some_and(|document| {
                    let document = document.borrow();
                    document.generation() == key.document_generation
                        && document.utf16_len() == key.document_length
                })
                && self
                    .ivars()
                    .visible_table_layout_cache
                    .borrow()
                    .as_ref()
                    .is_some_and(|cache| {
                        std::rc::Weak::ptr_eq(&cache.document_identity, &identity)
                            && cache.request_key.text_view_width == key.text_view_width
                            && cache.request_key.presentation_mode == key.presentation_mode
                    });
            if still_current {
                #[cfg(all(
                    feature = "native-markdown-runtime-probe",
                    feature = "native-table-row-append"
                ))]
                if let Some(pointer) = request.pointer() {
                    NativeMarkdownTableAppendWorkload::observe_pointer_ready(self, &pointer, false);
                }
                #[cfg(feature = "native-markdown-runtime-probe")]
                request.finish_activated_observer();
            }
            still_current
        } else {
            false // executor原失败出口将终结同槽；不能由slot消失推断字段成功。
        }
    }

    fn activate_pending_table_cell_inner(&self) -> bool {
        // 有限草稿/属性副本在同步原生操作中仍有效，租约未结束时拒绝重入关闭。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        // 最早捕获本事件意图；后续slot清空不等于撤销了这个局部cell。
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        let Some(request) = self.ivars().pending_table_cell_activation.request() else {
            return false;
        };
        let coordinate = request.target;
        let Some(authority) = request.authority() else {
            return false; // Pending不是literal失败，也不允许凭整数坐标激活。
        };
        let Some(key) = self.current_table_layout_request_key() else {
            return false;
        };
        let Some(storage) = self.ivars().text_storage.load() else {
            return false;
        };
        let identity = storage.ivars().document.borrow().clone();
        let is_current = || {
            request.current(self, &storage)
                && self.table_input_intent_is_current(&storage, &identity, key, input_epoch)
                && storage.ivars().table_geometry_epoch.get() == key.table_geometry_epoch
        };
        if !is_current() {
            // key getter重入、新owner或已失效高度都不能继续复制旧格。
            return false;
        }
        // 先取局部Rc再clone cell，retain/drop不跨cache槽借用。
        let cache = self
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .filter(|cache| {
                cache.request_key == key
                    && std::rc::Weak::ptr_eq(&cache.document_identity, &identity)
            })
            .map(Rc::clone);
        let cell = cache
            .as_ref()
            .and_then(|cache| {
                cache
                    .tables
                    .iter()
                    .find(|table| table.table_utf16_start == coordinate.table_utf16_start)
            })
            .and_then(|table| {
                table
                    .cells
                    .iter()
                    .find(|cell| cell.coordinate == coordinate)
            })
            .cloned();
        let Some(cell) = cell else {
            // Keep the pending semantic intent until a real visible viewport supplies matching geometry.
            return false;
        };
        if !is_current() || self.ivars().pending_table_cell_activation.get() != Some(coordinate) {
            // cell的Retained clone也可重入；不能清掉后继事件刚发布的pending。
            return false;
        }
        self.table_edit_diagnostic("activation_before_rect_scroll");
        let Ok(visible) = native_markdown_table_intent_step(&is_current, || self.visibleRect())
        else {
            // 新意图已经撤权；清过的pending不重新排队，局部cell直接释放。
            return false;
        };
        let rect = cell.frame;
        let Some(origin) = native_markdown_minimal_cell_scroll_origin(rect, visible) else {
            return false;
        };
        let (x, y) = (origin.x, origin.y);
        if (x - visible.origin.x).abs() > 0.01 || (y - visible.origin.y).abs() > 0.01 {
            // 已可见格完全不滚；离屏相邻格只移动必要距离，超高格显示其起点。
            let Ok(Some(scroll)) =
                native_markdown_table_intent_step(&is_current, || self.enclosingScrollView())
            else {
                // 旧窗口已消失或getter内撤权，不把局部rect用于新图。
                return false;
            };
            let Ok(clip) = native_markdown_table_intent_step(&is_current, || scroll.contentView())
            else {
                // 容器查询可重入，新事件接管后不再执行旧scroll。
                return false;
            };
            if native_markdown_table_intent_step(&is_current, || {
                clip.scrollToPoint(NSPoint::new(x.max(0.0), y.max(0.0)))
            })
            .is_err()
                || native_markdown_table_intent_step(&is_current, || {
                    scroll.reflectScrolledClipView(&clip)
                })
                .is_err()
                || native_markdown_table_intent_step(&is_current, || self.invalidate_visible_body())
                    .is_err()
            {
                // 任一步重入即结束，不reflect后再强拉回，也不激活旧cell。
                return false;
            }
        }
        self.table_edit_diagnostic("activation_after_rect_scroll");
        let Ok(Some(after)) = native_markdown_table_intent_step(&is_current, || {
            self.current_table_layout_request_key()
        }) else {
            // 滚动后的真实key仍须属于同owner/代次/宽度/语义权限。
            return false;
        };
        if after.document_generation != key.document_generation
            || after.text_view_width != key.text_view_width
            || after.presentation_mode != key.presentation_mode
            || after.table_geometry_epoch != key.table_geometry_epoch
        {
            // viewport可以按最小rect变化，正文和几何合同不能变化。
            return false;
        }
        let semantic = request
            .identity
            .upgrade()
            .and_then(|document| authority.source_range(&document.borrow(), coordinate).ok());
        if semantic.as_ref() != Some(&cell.source_utf16_range) || !is_current() {
            return false; // 原Ready、实际矩阵与当前格必须指向同一Present/ImplicitEmpty位置。
        }
        let action = request.action.borrow().clone();
        let pointer = match action {
            NativeMarkdownTableCellAction::ActivateOriginalClick { pointer, .. } => {
                let Some(pointer) = pointer else {
                    return false;
                }; // 纯源窗口测试不签发真实鼠标激活权。
                Some(pointer)
            }
            _ => None, // Tab/明确命令仍保留原全格选择语义。
        };
        if let Some(pointer) = &pointer
            && pointer.extending
            && !(cell.source_utf16_range.start..=cell.source_utf16_range.end)
                .contains(&pointer.anchor.anchor_utf16)
        {
            // 跨格原 anchor 仍属于正文；不能截为新格起点或建立一份无法表达该选择的字段。
            let original_event = pointer.latest_event();
            return match self.install_table_pointer_body_selection(&request, pointer) {
                Ok(()) => true,
                Err(error) => {
                    if original_event
                        .as_ref()
                        .is_some_and(|event| pointer.owns_event(event))
                    {
                        pointer.cancel();
                        self.fail_required_table_cell(&request, storage, &error);
                    }
                    false // 实际失败归还同槽；不能把 Selecting 留到后续输入伪装为仍可推进。
                }
            };
        }
        let activated = self.begin_table_cell_editing_with_intent(
            cell,
            Rc::clone(&authority),
            pointer,
            &is_current,
        );
        activated && is_current()
    }

    fn begin_table_cell_editing_with_intent(
        &self,
        cell: NativeMarkdownVisibleTableCellLayout,
        authority: Rc<NativeMarkdownTableCellAuthority>,
        pointer: Option<Rc<NativeMarkdownTableCellPointer>>,
        is_current: &impl Fn() -> bool,
    ) -> bool {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let original_pointer_event = pointer.as_ref().and_then(|pointer| pointer.latest_event());
        let result = (|| {
            let storage =
                native_markdown_table_intent_step(is_current, || self.ivars().text_storage.load())?
                    .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 原单格源请求尚未开始；只采内核标量，不为诊断读取或保存单元格内容。
            super::probe_call_diagnostic::allocation_audit_point("field_source_begin");
            let source = native_markdown_table_intent_step(is_current, || {
                storage.table_cell_source(&cell.source_utf16_range)
            })?
            .ok_or(NativeMarkdownStableTableFailure::InvalidSourceAssociation)?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 原请求及其文档借用已结束；有限源值沿既有载荷继续，不额外复制。
            super::probe_call_diagnostic::allocation_audit_point("field_source_return");
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 将渲染载荷取得与源取得分开观察，不改变两个原步骤的权限和顺序。
            super::probe_call_diagnostic::allocation_audit_point("field_render_begin");
            let render =
                native_markdown_table_intent_step(is_current, || storage.render_payload())?
                    .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 原render槽借用在getter返回前结束；采样不保活第二份会话载荷。
            super::probe_call_diagnostic::allocation_audit_point("field_render_return");
            let editor = self.table_cell_editor_with_intent(is_current)?;
            // 所有原生字符串和setter各自前后复核；未发布draft不因失败覆盖新active。
            let label = native_markdown_table_intent_step(is_current, || {
                NSString::from_str(&format!(
                    "表格单元格，第 {} 行、第 {} 列",
                    cell.coordinate.row_index + 1,
                    cell.coordinate.column_index + 1
                ))
            })?;
            let text =
                native_markdown_table_intent_step(is_current, || NSString::from_str(&source))?;
            native_markdown_table_intent_step(is_current, || editor.setFrame(cell.text_frame))?;
            native_markdown_table_intent_step(is_current, || {
                editor.setAccessibilityLabel(Some(&label))
            })?;
            native_markdown_table_intent_step(is_current, || {
                editor.setAccessibilityHelp(Some(ns_string!(
                    "按制表键（Tab）或换挡加制表键（Shift-Tab）切换单元格，按回车移到下一行，按退出键（Esc）取消当前单元格草稿。"
                )))
            })?;
            native_markdown_table_intent_step(is_current, || editor.setStringValue(&text))?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 只观察原值setter已返回的进程标量；不重读草稿，也不触发额外布局。
            super::probe_call_diagnostic::allocation_audit_point("field_value_set_return");
            let alignment = match cell.alignment {
                MarkdownTableAlignment::Left => NSTextAlignment::Left, // 普通列采用前导对齐。
                MarkdownTableAlignment::Center => NSTextAlignment::Center, // 保留delimiter的居中合同。
                MarkdownTableAlignment::Right => NSTextAlignment::Right, // 保留delimiter的右对齐合同。
            };
            native_markdown_table_intent_step(is_current, || editor.setAlignment(alignment))?;
            let font = if cell.is_header {
                &render.palette.table_header_font // 表头使用同一会话已持有字形。
            } else {
                &render.palette.normal_font // 正文不创建第二套字体缓存。
            };
            native_markdown_table_intent_step(is_current, || editor.setFont(Some(font)))?;
            // 最后一个native返回后仍有权才发布draft；发布本身不调用native或保留旧RefMut。
            native_markdown_table_intent_step(is_current, || ())?;
            let previous =
                self.ivars()
                    .active_table_cell
                    .replace(Some(NativeMarkdownTableCellEditorState {
                        authority: Some(Rc::clone(&authority)),
                        coordinate: cell.coordinate,
                        source_utf16_range: cell.source_utf16_range.clone(),
                        frame: cell.frame,
                        original_source: source,
                        field_selection: None, // 后续真实 caret/扩选安装成功才发布方向，不能从源文本猜测。
                        pointer: pointer
                            .as_ref()
                            .filter(|pointer| !pointer.ended.get())
                            .map(Rc::clone),
                    }));
            drop(previous);
            native_markdown_table_intent_step(is_current, || editor.setHidden(false))?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 原frame、字体和显隐已完成；标签不代表执行了新的layout或通过功能验收。
            super::probe_call_diagnostic::allocation_audit_point("field_layout_return");
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 与原焦点调用相邻采样，不取得窗口、字段编辑器或正文的额外Native claim。
            super::probe_call_diagnostic::allocation_audit_point("field_focus_begin");
            native_markdown_table_intent_step(is_current, || {
                if let Some(pointer) = &pointer {
                    self.apply_table_field_pointer(
                        &editor,
                        &cell.source_utf16_range,
                        pointer,
                        is_current,
                    )
                } else {
                    self.select_entire_table_field(&editor, is_current)
                }
            })??;
            if let Some(pointer) = &pointer
                && pointer.ended.get()
            {
                pointer.cancel(); // Up 已在 Ready 前发生；选择安装结束即归还原事件，不等待不存在的 Up。
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 原焦点及其真实选择安装返回后记录；失败仍走原意图撤权出口，不补采样冒成功。
            super::probe_call_diagnostic::allocation_audit_point("field_focus_return");
            native_markdown_table_intent_step(is_current, || {
                self.setNeedsDisplayInRect(cell.frame)
            })?;
            Ok::<(), NativeMarkdownStableTableFailure>(())
        })();
        if result.is_err() {
            if let Some(pointer) = &pointer
                && original_pointer_event
                    .as_ref()
                    .is_some_and(|event| pointer.owns_event(event))
            {
                pointer.cancel();
            } // 重入已经交付同 gesture 新事件时，旧失败不能取消其后继选择。
            // 复用或已发布字段可能已被新意图接管；绝不无条件hide/clear/abort/remove或清active。
            self.table_edit_diagnostic("activation_intent_expired_or_unavailable_failed");
        }
        result.is_ok()
    }

    /// 只复制已创建字段的强引用，调用方不持状态借用跨越 AppKit 回调。
    pub(super) fn existing_table_cell_editor(&self) -> Option<Retained<NSTextField>> {
        let owner = self.ivars().cell_editor.borrow().as_ref().map(Rc::clone);
        // Native retain可重入写slot；本地Rc保活同一字段且RefCell借用已经结束。
        owner.map(|owner| Retained::clone(&owner))
    }

    /// 首次显式编辑才创建字段；未发布candidate局部drop，已发布/复用字段撤权时保持原状态。
    fn table_cell_editor_with_intent(
        &self,
        is_current: &impl Fn() -> bool,
    ) -> Result<Retained<NSTextField>, NativeMarkdownStableTableFailure> {
        if let Some(editor) =
            native_markdown_table_intent_step(is_current, || self.existing_table_cell_editor())?
        {
            // 已有字段保留当前草稿及标准共享 field editor 关系。
            return Ok(editor);
        }
        // 构造和安装子视图可能重入；不在原生调用期间借用 ivar。
        let mtm = MainThreadMarker::new()
            .expect("Markdown table cell editor must be created on the AppKit main thread");
        #[cfg(feature = "native-markdown-runtime-probe")]
        // 仅首次创建分支记录原alloc之前；复用已有字段不冒记一次新对象构造。
        super::probe_call_diagnostic::allocation_audit_point("field_create_begin");
        let allocated = native_markdown_table_intent_step(is_current, || NSTextField::alloc(mtm))?;
        let editor = native_markdown_table_intent_step(is_current, || {
            NSTextField::initWithFrame(
                allocated,
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            )
        })?;
        native_markdown_table_intent_step(is_current, || editor.setEditable(true))?;
        native_markdown_table_intent_step(is_current, || editor.setSelectable(true))?;
        native_markdown_table_intent_step(is_current, || editor.setBezeled(false))?;
        // 可见文字按完整格宽折行；Return仍由现有delegate导航，不改canonical输入/历史算法。
        native_markdown_table_intent_step(is_current, || editor.setUsesSingleLineMode(false))?;
        native_markdown_table_intent_step(is_current, || {
            editor.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping)
        })?;
        if let Some(cell) = native_markdown_table_intent_step(is_current, || editor.cell())? {
            // cell不水平滚动/截断，保证共享fieldeditor按当前宽度换行。
            native_markdown_table_intent_step(is_current, || cell.setWraps(true))?;
            native_markdown_table_intent_step(is_current, || cell.setScrollable(false))?;
        }
        native_markdown_table_intent_step(is_current, || editor.setDrawsBackground(true))?;
        let background = native_markdown_table_intent_step(is_current, || {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.13, 0.20, 0.31, 1.0)
        })?;
        native_markdown_table_intent_step(is_current, || {
            editor.setBackgroundColor(Some(&background))
        })?;
        let foreground = native_markdown_table_intent_step(is_current, || {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.91, 0.93, 0.97, 1.0)
        })?;
        native_markdown_table_intent_step(is_current, || editor.setTextColor(Some(&foreground)))?;
        let font =
            native_markdown_table_intent_step(is_current, || NSFont::systemFontOfSize(14.0))?;
        native_markdown_table_intent_step(is_current, || editor.setFont(Some(&font)))?;
        native_markdown_table_intent_step(is_current, || {
            editor.setFocusRingType(NSFocusRingType::None)
        })?;
        native_markdown_table_intent_step(is_current, || editor.setHidden(true))?;
        // SAFETY: 主线程 view 拥有字段，NSTextField 的 delegate 为借用关系。
        native_markdown_table_intent_step(is_current, || unsafe {
            editor.setDelegate(Some(ProtocolObject::from_ref(self)))
        })?;
        // 此字段和实际 cell 是本项目显式创建角色，标准控件仍必须实际释放。
        let ledger = self
            .ivars()
            .owned_ledger
            .upgrade()
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        native_markdown_table_intent_step(is_current, || {
            ledger.include_control("editor.table.field", &editor)
        })?
        .map_err(|_| NativeMarkdownStableTableFailure::CoverageOutsideBudget)?;
        // 先发布唯一字段，再触发层级回调；重入只能复用这个已发布实例。
        let claim = native_markdown_table_intent_step(is_current, || editor.clone())?;
        let owner = Rc::new(claim); // 纯Rust局部owner先建立，slot变更不调用Native retain。
        if self.ivars().cell_editor.borrow().is_some() {
            // 构造内已有另一字段先发布，丢弃未发布candidate，不替换后继字段。
            return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
        }
        *self.ivars().cell_editor.borrow_mut() = Some(owner);
        native_markdown_table_intent_step(is_current, || self.addSubview(&editor))?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        // 全部原配置与挂载正常返回后才记完成，日志不保留字段或扩张Native资源生命周期。
        super::probe_call_diagnostic::allocation_audit_point("field_create_return");
        Ok(editor)
    }

    /// Commits one cell only after canonical acceptance; failure preserves the visible draft and focus.
    pub(super) fn commit_active_table_cell(&self) -> Result<bool, String> {
        self.commit_active_table_cell_impl(None)
    }

    /// 仅模式前置操作消费真实单cell observed receipt；其他命令不重新取得Mode权限。
    pub(super) fn commit_active_table_cell_with_mode(
        &self,
        mode: &super::table_layout::NativeMarkdownModeViewportContext,
    ) -> Result<bool, String> {
        self.commit_active_table_cell_impl(Some(mode))
    }

    fn commit_active_table_cell_impl(
        &self,
        mode: Option<&super::table_layout::NativeMarkdownModeViewportContext>,
    ) -> Result<bool, String> {
        self.commit_verified_table_cell(mode, None)
    }

    /// Reports a failed cell commit and prevents a native command from discarding its pending draft.
    pub(super) fn commit_cell_before_command(&self) -> bool {
        match self.commit_active_table_cell() {
            // An unchanged but successfully ended field is also safe for the next command.
            Ok(_) => true,
            // No subsequent activation/navigation/presentation command may replace this visible draft.
            Err(error) => {
                present_native_markdown_error(&error);
                false
            }
        }
    }

    /// 取消当前原生单元格草稿；保留canonical，并在实际abort后结束原客户自有历史。
    pub(super) fn cancel_active_table_cell(&self) {
        // 有限草稿/属性副本在同步原生操作中仍有效，租约未结束时拒绝重入关闭。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        self.cancel_table_continuity_for_new_input(); // Escape先撤所有旧行请求，不能由旧命令重开字段。

        let editor = self.existing_table_cell_editor();
        let controller = self.ivars().controller.get().and_then(Weak::load);
        let history = controller
            .as_ref()
            .map(|controller| controller.ivars().borrowed.borrow().clone());
        let end_ticket = history
            .as_ref()
            .zip(editor.as_ref())
            .and_then(|(history, editor)| history.capture_control_abort(editor));

        self.ivars().pending_table_cell_activation.set(None);
        let state = self.ivars().active_table_cell.borrow_mut().take();
        if let Some(editor) = editor {
            // 先撤活动状态再真实abort；此公开取消路径不保证正常controlTextDidEndEditing通知。
            let aborted = editor.abortEditing();
            if aborted
                && let Some(ticket) = end_ticket
                && let Some(history) = history
                && let Some(controller) = &controller
            {
                // 只消费abort前原客户身份；内部仍检查实际辞任与owned manager，错误保持首因收据。
                let _ = history.finished_aborted_control(
                    ticket,
                    &editor,
                    &controller.ivars().undo,
                    &controller.ivars().owned,
                );
            } // 失败abort不授予清历史权；同步didEnd已完成时，原弱票只作无操作。
            editor.setHidden(true);
            editor.setStringValue(ns_string!(""));
        }
        if let Some(state) = state {
            // 仍从未改的canonical投影重画旧单元格，不把已取消的field值写回正文。
            self.setNeedsDisplayInRect(state.frame);
        }
        if let Some(window) = self.window() {
            // 沿原取消交互把焦点交回正文；不新增按键或改为应用退出。
            window.makeFirstResponder(Some(self));
        }
    }

    /// Commits the current cell and resolves spreadsheet-style movement against newly parsed semantics.
    pub(super) fn navigate_active_table_cell(&self, navigation: NativeMarkdownTableNavigation) {
        self.navigate_active_table_cell_observed(
            navigation,
            #[cfg(feature = "native-markdown-runtime-probe")]
            None,
        );
    }

    fn navigate_active_table_cell_observed(
        &self,
        navigation: NativeMarkdownTableNavigation,
        #[cfg(feature = "native-markdown-runtime-probe")] observer: Option<
            Rc<NativeMarkdownTableCellReceipt>,
        >,
    ) -> bool {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        self.cancel_table_continuity_for_new_input();
        let epoch = self.ivars().table_input_intent_epoch.get();
        let fold = self.ivars().folds.revision();
        let Some(active) = self.ivars().active_table_cell.borrow().clone() else {
            return false;
        };
        let Some(authority) = active.authority.as_ref().map(Rc::clone) else {
            present_native_markdown_error("当前格源权限尚未就绪，草稿将保持打开");
            return false;
        };
        let target = authority
            .document
            .upgrade()
            .ok_or("表格所属文档已关闭".to_owned())
            .and_then(|document| {
                authority.navigation(&document.borrow(), active.coordinate, navigation)
            });
        let target = match target {
            Ok(omni_markdown_core::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigationTarget::Available(target)
                | omni_markdown_core::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigationTarget::RequiredRow(target)) => target,
            Ok(omni_markdown_core::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigationTarget::TableStart) => return false,
            Err(error) => {
                present_native_markdown_error(&error);
                return false; // 当前格也必须已验证，不能从过期整数坐标推算一个新目标。
            }
        };
        let coordinate = NativeMarkdownTableCellCoordinate {
            table_utf16_start: active.coordinate.table_utf16_start,
            row_index: target.row_ordinal,
            column_index: target.column_index,
        };
        let action = NativeMarkdownTableCellAction::Navigate {
            original: active,
            direction: navigation,
        };
        if let Err(error) = self.start_required_table_cell_observed(
            coordinate,
            action,
            NativeMarkdownTableCellIntent::uncommitted(epoch, fold),
            Some(authority.header_byte_start),
            &|| true,
            #[cfg(feature = "native-markdown-runtime-probe")]
            observer,
        ) {
            if self.ivars().table_input_intent_epoch.get() == epoch
                && self.ivars().folds.read_is_current(fold)
            {
                present_native_markdown_error(&error);
            }
            false
        } else {
            true // 只接受请求，成功终点仍须等真实字段建立。
        }
    }

    /// Moves the reusable field when matching viewport geometry is available, preserving any uncached draft.
    pub(super) fn reposition_active_table_cell_editor(&self) {
        // 有限草稿/属性副本在同步原生操作中仍有效，租约未结束时拒绝重入关闭。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        let Some(active) = self.ivars().active_table_cell.borrow().clone() else {
            // No reusable field draft exists to navigate, move, or replace.
            return;
        };
        let Some(request_key) = self.current_table_layout_request_key() else {
            // 空壳/未就绪geometry不能改变活动草稿的位置。
            return;
        };
        let frame = self
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.request_key == request_key)
            .and_then(|cache| {
                cache.tables.iter().find_map(|table| {
                    (table.table_utf16_start == active.coordinate.table_utf16_start).then(|| {
                        table
                            .cells
                            .iter()
                            .find(|cell| cell.coordinate == active.coordinate)
                    })?
                })
            })
            .map(|cell| (cell.frame, cell.text_frame));
        let Some((frame, content_frame)) = frame else {
            // Missing/offscreen geometry never discards a draft or guesses its visual placement.
            return;
        };
        if let Some(editor) = self.existing_table_cell_editor() {
            // Move only the reusable native shell after ending the layout-cache borrow above.
            editor.setFrame(content_frame);
        }
        if let Some(active) = self.ivars().active_table_cell.borrow_mut().as_mut() {
            // Update finite cancellation/damage metadata after native geometry has settled.
            active.frame = frame;
        }
    }

    /// Releases derived table geometry and optionally commits the active cell before changing presentation.
    pub(super) fn invalidate_visible_table_layouts(
        &self,
        end_cell_editing: bool,
    ) -> Result<(), String> {
        // 有限草稿/属性副本在同步原生操作中仍有效，租约未结束时拒绝重入关闭。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

        if end_cell_editing {
            // A failed cell edit vetoes presentation changes while retaining the visible draft.
            self.commit_active_table_cell()?;
        }
        // Only an accepted transition can release pending semantic coordinates and derived geometry.
        self.ivars().pending_table_cell_activation.set(None);
        self.invalidate_visible_table_layout_cache();
        self.invalidate_visible_body();
        Ok(())
    }

    /// 插表只提交一次正文历史；第一格选择、滚动与激活始终消费这份原提交收据。
    pub(super) fn insert_default_markdown_table(&self) {
        #[cfg(feature = "native-markdown-runtime-probe")]
        let _observation = self.probe_table_insertion_scope(); // 未进入原 request 的失败也必须有一次真实终点。
        if !self.commit_cell_before_command() || !self.commit_composition_before_command() {
            return; // 前置草稿/组合输入拒绝时，原工具栏动作不得继续改写正文。
        }
        let result = (|| -> Result<(), String> {
            let pending_tail = RefCell::new(None); // 必须早于 storage 等 Native claims，不能让返回解构顺序提前清 owner。
            self.cancel_table_continuity_for_new_input(); // 这是新的明确插表意图，旧行/模式/Find 定位不可续接。
            let input_epoch = self.ivars().table_input_intent_epoch.get();
            let capture = self
                .ivars()
                .folds
                .capture_selection()
                .map_err(|error| error.to_string())?;
            let storage = selection_native_step(&capture, || self.ivars().text_storage.load())
                .map_err(|error| error.to_string())?
                .ok_or("笔记编辑器已关闭")?;
            let identity = storage.ivars().document.borrow().clone();
            let width = storage.ivars().table_projection_width_key.get();
            let mode = storage.presentation_mode();
            let context = || {
                !self.ivars().textkit_graph_teardown.get()
                    && self.ivars().table_input_intent_epoch.get() == input_epoch
                    && storage.ivars().accepts_edits.get()
                    && storage.presentation_mode() == mode
                    && storage.ivars().table_projection_width_key.get() == width
                    && std::rc::Weak::ptr_eq(&identity, &storage.ivars().document.borrow())
            };
            let captured =
                self.capture_ordered_native_before(capture.clone(), &storage, &|| {
                    context() && capture.current()
                })?;
            let native_before = self
                .native_ordered_selection_state_for_capture(&capture)
                .map_err(|error| error.to_string())?;
            let NativeMarkdownOrderedSelection::Single(selection) = native_before else {
                return Err("插入表格需要单一正文选择".into());
            };
            let selection_end = selection
                .location
                .checked_add(selection.length)
                .ok_or("表格替换范围溢出")?;
            let document = storage.document().ok_or("笔记文档已关闭")?;
            let (needs_prefix, needs_suffix) = {
                let document = document.borrow();
                if !captured.current_in(&document) || selection_end > document.utf16_len() {
                    return Err("插入表格对应的正文选择已变更".into());
                }
                // 只检查两端相邻单元；不读取、复制或解析全文来决定块间空行。
                let prefix = selection.location > 0
                    && !document
                        .utf16_code_unit_at(selection.location - 1)
                        .is_ok_and(|unit| unit == b'\n' as u16 || unit == b'\r' as u16);
                let suffix = selection_end < document.utf16_len()
                    && !document
                        .utf16_code_unit_at(selection_end)
                        .is_ok_and(|unit| unit == b'\n' as u16 || unit == b'\r' as u16);
                (prefix, suffix)
            };
            let markdown = native_markdown_default_table_source(needs_prefix, needs_suffix);
            let table_start = selection
                .location
                .checked_add(usize::from(needs_prefix) * 2)
                .ok_or("表格位置溢出")?;
            let first = table_start.checked_add(2).ok_or("首格位置溢出")?;
            let (_, committed, pending) = storage.apply_table_insertion(
                &captured,
                selection,
                &markdown,
                MarkdownTextSelection::caret(first),
                &context,
            )?;
            drop(pending_tail.replace(pending));
            let current = || context() && committed.current();
            if !self.apply_committed_selection_if(&committed, current) {
                return Err("表格已插入，但原选择显示动作已被新输入撤销".into());
            }
            native_markdown_table_intent_step(&current, || {
                self.invalidate_visible_table_layout_cache()
            })
            .map_err(|_| "插表显示意图已撤销")?;
            native_markdown_table_intent_step(&current, || {
                self.scrollRangeToVisible(NSRange::new(first, 0))
            })
            .map_err(|_| "插表滚动意图已撤销")?;
            self.start_required_table_cell(
                NativeMarkdownTableCellCoordinate {
                    table_utf16_start: table_start,
                    row_index: 0,
                    column_index: 0,
                },
                NativeMarkdownTableCellAction::Activate,
                NativeMarkdownTableCellIntent {
                    input_epoch,
                    fold_revision: self.ivars().folds.revision(),
                    committed: Some(Rc::clone(&committed)),
                },
                None,
                &current,
            )?; // 同一原收据交给原 RequiredRow executor，不另领新激活权限。
            native_markdown_table_intent_step(&current, || {
                self.schedule_visible_table_layout_update()
            })
            .map_err(|_| "插表排版意图已撤销")?;
            native_markdown_table_intent_step(&current, || self.invalidate_visible_body())
                .map_err(|_| "插表重绘意图已撤销")?;
            Ok(())
        })();
        if let Err(error) = result {
            present_native_markdown_error(&error);
        }
    }
}
