//! 同一产品视图上的公共选择命令回归；selector 调用不是物理键盘输入验收。

use super::*;
use objc2::runtime::Sel;

/// 每个动作只允许首 Native 调用前声明的有限状态，不从返回后的实际值重新签发权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActionState {
    selection: MarkdownTextSelection,
    fold: u64,
    input: u64,
}

/// 夹具只有借用；正文身份只有 Weak，原生图仍由原 surface 独占装配与释放。
struct Permission<'a> {
    surface: &'a NativeMarkdownTextKitEditorSurface,
    controller: &'a Retained<NativeMarkdownController>,
    document: WeakMarkdownDocument,
    generation: u64,
    revision: omni_markdown_core::MarkdownDocumentRevision,
    length: usize,
    mode: MarkdownPresentationMode,
    width: i64,
    geometry: u64,
    epochs: [u64; 3],
    state: Cell<ActionState>,
    entered: Cell<Option<ActionState>>,
    after: Cell<Option<ActionState>>,
    phase: Cell<&'static str>,
}

impl Permission<'_> {
    fn actual_state(&self) -> ActionState {
        ActionState {
            selection: self.surface.document().borrow().selection(),
            fold: self.surface.backend.text_view.ivars().folds.revision(),
            input: self
                .surface
                .backend
                .text_view
                .ivars()
                .table_input_intent_epoch
                .get(),
        }
    }

    fn current(&self) -> bool {
        let view = &self.surface.backend.text_view;
        let storage = &self.surface.backend.text_storage;
        if !native_markdown_controller_has_foreground_authority(self.controller)
            || view.ivars().textkit_graph_teardown.get()
            || !view.ivars().folds.owner_is_current()
            || !view.ivars().folds.borrow().is_empty()
            || !storage.ivars().accepts_edits.get()
            || storage.presentation_mode() != self.mode
            || storage.ivars().table_projection_width_key.get() != self.width
            || storage.ivars().table_geometry_epoch.get() != self.geometry
            || self.controller.find_input_intent() != self.epochs[0]
            || self.controller.workspace_query_intent() != self.epochs[1]
            || self.controller.ivars().header_layout_revision.get() != self.epochs[2]
            || !WeakMarkdownDocument::ptr_eq(
                &self.document,
                &Rc::downgrade(self.surface.document()),
            )
        {
            return false; // 关闭、换图权限、布局条件或额外输入改变时，旧测试不接管后继动作。
        }
        let state = self.actual_state();
        if state != self.state.get()
            && self.entered.get() != Some(state)
            && self.after.get() != Some(state)
        {
            return false; // 选择、fold 和输入代次成组比较，不能从多个允许态交叉拼装权限。
        }
        let document = self.surface.document().borrow();
        // 本函数仅用于已有有限合成夹具；摘要不复制正文，也不进入产品输入热路径。
        // clean 在此 Core 中表示仍共享保存时的 initial root；空 undo/redo 必须持续为空。
        document.generation() == self.generation
            && document.utf16_len() == self.length
            && document
                .current_revision()
                .is_ok_and(|actual| actual == self.revision)
            && document.original_revision() == Some(self.revision)
            && !document.is_dirty()
            && !document.can_undo()
            && !document.can_redo()
    }

    fn require_current(&self) -> Result<(), String> {
        if self.current() {
            return Ok(());
        }
        Err(format!(
            "标准选择回归原权限已失效：阶段={} 实际={:?} 原态={:?} 入口态={:?} 目标态={:?}",
            self.phase.get(),
            self.actual_state(),
            self.state.get(),
            self.entered.get(),
            self.after.get(),
        )) // 只报告固定阶段与合成坐标，不记录正文、路径或搜索内容。
    }

    fn step<T>(&self, work: impl FnOnce() -> T) -> Result<T, String> {
        native_markdown_table_intent_step(&|| self.current(), work)
            .map_err(|_| format!("标准选择回归的原 Native 动作已撤权：{}", self.phase.get()))
    }

    /// 内层结果和整个 pool 都结束后再核权限；失败首因不被后续同类错误覆盖。
    fn finish_pool<T>(&self, result: Result<T, String>) -> Result<T, String> {
        match (result, self.require_current()) {
            (Err(first), _) => Err(first),
            (Ok(_), Err(after)) => Err(after),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn prepare(
        &self,
        selection: MarkdownTextSelection,
        command: bool,
        phase: &'static str,
    ) -> Result<ActionState, String> {
        self.require_current()?;
        if self.entered.get().is_some() || self.after.get().is_some() {
            return Err("标准选择回归尚有未结算的原动作".into()); // 不把前一个失败动作包装成新的起票点。
        }
        {
            let document = self.surface.document().borrow();
            document
                .byte_offset_for_utf16(selection.anchor_utf16)
                .map_err(|_| "标准选择预期 anchor 不是原文 Unicode 边界")?;
            document
                .byte_offset_for_utf16(selection.head_utf16)
                .map_err(|_| "标准选择预期 head 不是原文 Unicode 边界")?;
        }
        let before = self.state.get();
        let after = ActionState {
            selection,
            fold: before
                .fold
                .checked_add(u64::from(before.selection != selection))
                .ok_or("标准选择回归的 fold 代次耗尽")?,
            input: before
                .input
                .checked_add(u64::from(command))
                .ok_or("标准选择回归的输入代次耗尽")?,
        };
        self.phase.set(phase);
        self.after.set(Some(after));
        // 命令先撤旧输入再纯发布新选择；只允许这一个精确入口态，不接受更多代次前进。
        self.entered.set(command.then_some(ActionState {
            input: after.input,
            ..before
        }));
        Ok(after)
    }

    fn settle(&self, after: ActionState) -> Result<(), String> {
        self.require_current()?;
        if self.after.get() != Some(after) || self.actual_state() != after {
            return Err(format!(
                "标准选择未达到首 Native 前声明的完整目标：阶段={} 实际={:?} 预期={after:?}",
                self.phase.get(),
                self.actual_state(),
            ));
        }
        self.state.set(after); // 只消费原先声明的 after；不以实际返回值续权。
        self.entered.set(None);
        self.after.set(None);
        self.verify_selection(after.selection)
    }

    fn select(&self, selection: MarkdownTextSelection) -> Result<(), String> {
        let after = self.prepare(selection, false, "semantic_selection")?;
        let result = autoreleasepool(|_| {
            self.step(|| {
                self.surface
                    .backend
                    .text_view
                    .set_semantic_native_selection(selection)
            })
        });
        self.finish_pool(result)?;
        self.settle(after)
    }

    fn command(
        &self,
        selector: Sel,
        selection: MarkdownTextSelection,
        phase: &'static str,
    ) -> Result<(), String> {
        let after = self.prepare(selection, true, phase)?;
        let result = autoreleasepool(|_| {
            let input =
                ProtocolObject::<dyn NSTextInputClient>::from_ref(self.surface.backend.text_view());
            self.step(|| {
                // SAFETY: selector 仅由下方固定标准命令给出；原产品 view 在主线程同步接收，无逃逸参数。
                unsafe { input.doCommandBySelector(selector) };
            })
        });
        self.finish_pool(result)?;
        self.settle(after)
    }

    fn verify_selection(&self, expected: MarkdownTextSelection) -> Result<(), String> {
        if self
            .surface
            .backend
            .text_view
            .ivars()
            .folds
            .known_selection()
            != Some(expected)
        {
            return Err("标准选择未发布同一完整 Known anchor/head".into());
        }
        let result = autoreleasepool(|_| {
            let actual = self
                .step(|| {
                    self.surface
                        .backend
                        .text_view
                        .native_ordered_selection_state()
                })?
                .map_err(|_| "标准选择无法读取真实有序范围")?;
            let start = expected.anchor_utf16.min(expected.head_utf16);
            let end = expected.anchor_utf16.max(expected.head_utf16);
            if actual != NativeMarkdownOrderedSelection::Single(NSRange::new(start, end - start)) {
                return Err("标准选择的实际 Native 范围与完整 Core 方向不一致".into());
            }
            let captured = self
                .step(|| self.surface.backend.text_view.primary_text_selection())?
                .map_err(|_| "标准选择没有可认证的实际完整主选择")?;
            if !captured.proven_head || !captured.current() || captured.selection != expected {
                return Err("标准选择的实际主选择未保留预期 anchor/head".into());
            }
            Ok(())
        });
        self.finish_pool(result)?;
        self.graph()
    }

    fn graph(&self) -> Result<(), String> {
        let result = autoreleasepool(|_| {
            let manager = self
                .step(|| self.surface.backend.text_view.textLayoutManager())?
                .ok_or("标准选择回归丢失实际 TK2 manager")?;
            let content = self
                .step(|| manager.textContentManager())?
                .ok_or("标准选择回归丢失实际 content manager")?;
            let view_content = self
                .step(|| self.surface.backend.text_view.textContentStorage())?
                .ok_or("标准选择回归丢失 view 的 content storage")?;
            let view_container = self
                .step(|| {
                    // SAFETY: 原 surface 强持有活 view；主线程公开 getter 不创建兼容布局。
                    unsafe { self.surface.backend.text_view.textContainer() }
                })?
                .ok_or("标准选择回归丢失 view 的 container")?;
            let container = self
                .step(|| manager.textContainer())?
                .ok_or("标准选择回归丢失 manager 的 container")?;
            let inverse = self
                .step(|| container.textLayoutManager())?
                .ok_or("标准选择回归丢失 container 的反向 manager")?;
            let observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*view_content);
            let storage = self
                .step(|| observer.textStorage())?
                .ok_or("标准选择回归丢失实际 piece storage")?;
            if !std::ptr::eq(&*manager, &*self.surface.backend.text_layout_manager)
                || !std::ptr::eq::<objc2_app_kit::NSTextContentManager>(
                    &*content,
                    self.surface.backend.text_content_storage.as_super(),
                )
                || !std::ptr::eq(&*view_content, &*self.surface.backend.text_content_storage)
                || !std::ptr::eq(&*view_container, &*self.surface.backend.text_container)
                || !std::ptr::eq(&*container, &*self.surface.backend.text_container)
                || !std::ptr::eq(&*inverse, &*self.surface.backend.text_layout_manager)
                || !std::ptr::eq::<NSTextStorage>(&*storage, self.surface.backend.text_storage())
            {
                return Err("标准选择命令更换或断开了原产品 TextKit 图".into());
            }
            Ok(())
        });
        self.finish_pool(result)
    }

    fn source_edges(&self, id_start: usize) -> Result<(), String> {
        let result = autoreleasepool(|_| {
            let source = self.step(|| self.surface.backend.text_storage().string())?;
            if self.step(|| source.length())? != self.length {
                return Err("标准选择命令改变了实际虚拟字符串长度".into());
            }
            let mut prefix = [0u16; 9];
            self.step(|| {
                // SAFETY: 首 Native 前确认夹具至少 9 units；本栈固定数组恰能容纳已知前缀。
                unsafe {
                    source.getCharacters_range(
                        NonNull::new(prefix.as_mut_ptr()).unwrap(),
                        NSRange::new(0, prefix.len()),
                    );
                }
            })?;
            let mut id = [0u16; 4];
            self.step(|| {
                // SAFETY: 首 Native 前已核固定 (ID) 四 units 在原长度内；输出数组恰有四个位置。
                unsafe {
                    source.getCharacters_range(
                        NonNull::new(id.as_mut_ptr()).unwrap(),
                        NSRange::new(id_start, id.len()),
                    );
                }
            })?;
            if prefix.into_iter().ne("# Probe\n\n".encode_utf16())
                || id.into_iter().ne("(ID)".encode_utf16())
                || self.step(|| source.characterAtIndex(self.length - 1))? != u16::from(b'\n')
            {
                return Err("标准选择命令未保留夹具的实际首尾源 unit".into());
            }
            Ok(())
        });
        self.finish_pool(result)
    }
}

/// 仅在现有 ProbeEditingOwnerLease 和首次字符编辑前运行；失败后也不接管已撤权的新输入。
pub(super) fn verify(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    fixture: &str,
) -> Result<(), String> {
    if !fixture.starts_with("# Probe\n\n| 任务编号 (ID) |") || !fixture.ends_with('\n') {
        return Err("标准选择回归需要原固定首行与末尾 LF 夹具".into());
    }
    // 期望来自字面量源，不调用待测 Graph 推导；这里只借既有夹具，不构造第二份正文。
    let id = fixture.find("(ID)").ok_or("标准选择夹具缺少固定 ID 词")?;
    let id_start = markdown_probe_utf16_offset(fixture, id)?;
    let id_inside = markdown_probe_utf16_offset(fixture, id + 2)?;
    let id_end = markdown_probe_utf16_offset(fixture, id + 3)?;
    let row_end = fixture[9..]
        .find('\n')
        .and_then(|offset| offset.checked_add(10))
        .ok_or("标准选择夹具缺少第二段行尾")?;
    let paragraph_end = markdown_probe_utf16_offset(fixture, row_end)?;
    let (generation, revision, length, original) = {
        let document = surface.document().borrow();
        if document.is_dirty() || document.can_undo() || document.can_redo() {
            return Err("标准选择回归必须在保存 root 与空历史上开始".into());
        }
        (
            document.generation(),
            document
                .current_revision()
                .map_err(|error| format!("无法读取原文档revision：{error}"))?,
            document.utf16_len(),
            document.selection(),
        )
    };
    if length < 10
        || length != fixture.encode_utf16().count()
        || id_start.checked_add(4).is_none_or(|end| end > length)
    {
        return Err("标准选择夹具与原 Core 长度不符".into());
    }
    if surface.backend.text_view.ivars().folds.known_selection() != Some(original) {
        return Err("标准选择回归的原完整选择尚未明确".into()); // 不能把未知旧选择签成恢复目标。
    }
    let permission = Permission {
        surface,
        controller,
        document: Rc::downgrade(surface.document()),
        generation,
        revision,
        length,
        mode: surface.backend.text_storage.presentation_mode(),
        width: surface
            .backend
            .text_storage
            .ivars()
            .table_projection_width_key
            .get(),
        geometry: surface
            .backend
            .text_storage
            .ivars()
            .table_geometry_epoch
            .get(),
        epochs: [
            controller.find_input_intent(),
            controller.workspace_query_intent(),
            controller.ivars().header_layout_revision.get(),
        ],
        state: Cell::new(ActionState {
            selection: original,
            fold: surface.backend.text_view.ivars().folds.revision(),
            input: surface
                .backend
                .text_view
                .ivars()
                .table_input_intent_epoch
                .get(),
        }),
        entered: Cell::new(None),
        after: Cell::new(None),
        phase: Cell::new("initial"),
    };
    let _payload = PayloadUseGuard::enter(&surface.backend.text_view.ivars().payload_use_depth);
    let result = (|| {
        permission.require_current()?;
        permission.verify_selection(original)?;
        permission.source_edges(id_start)?;

        // 独立标准控件已实测 caret4 的三种边界；每个动作随后继续扩选，验证活动端可消费。
        for (selector, expected, phase) in [
            (
                sel!(selectWord:),
                MarkdownTextSelection {
                    anchor_utf16: 2,
                    head_utf16: 7,
                },
                "word_caret",
            ),
            (
                sel!(selectLine:),
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: 8,
                },
                "line_caret",
            ),
            (
                sel!(selectParagraph:),
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: 8,
                },
                "paragraph_caret",
            ),
        ] {
            permission.select(MarkdownTextSelection::caret(4))?;
            permission.command(selector, expected, phase)?;
            permission.command(
                sel!(moveForwardAndModifySelection:),
                MarkdownTextSelection {
                    anchor_utf16: expected.anchor_utf16,
                    head_utf16: expected.head_utf16 + 1,
                },
                "expansion_continue",
            )?;
        }

        // 正反范围跨过首空行；Word 必须保留 Probe 到 ID 的跨度，不能只围绕一个 head 分词。
        for selection in [
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: id_inside,
            },
            MarkdownTextSelection {
                anchor_utf16: id_inside,
                head_utf16: 4,
            },
        ] {
            permission.select(selection)?;
            permission.command(
                sel!(selectWord:),
                MarkdownTextSelection {
                    anchor_utf16: 2,
                    head_utf16: id_end,
                },
                "word_full_span",
            )?;
        }
        // 第二段以固定竖线开始；完整段落选择须到该真实源行的 LF，方向与输入范围先后无关。
        for selection in [
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 10,
            },
            MarkdownTextSelection {
                anchor_utf16: 10,
                head_utf16: 4,
            },
        ] {
            permission.select(selection)?;
            permission.command(
                sel!(selectParagraph:),
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: paragraph_end,
                },
                "paragraph_full_span",
            )?;
        }

        permission.command(
            sel!(selectAll:),
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: length,
            },
            "all",
        )?;
        // 末尾已核实为单 LF；先收缩再伸回，不把全选显示正确误当成后继 anchor 正确。
        permission.command(
            sel!(moveLeftAndModifySelection:),
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: length - 1,
            },
            "all_shrink",
        )?;
        permission.command(
            sel!(moveRightAndModifySelection:),
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: length,
            },
            "all_extend",
        )?;

        permission.select(MarkdownTextSelection {
            anchor_utf16: 7,
            head_utf16: 2,
        })?;
        // Probe 全为 ASCII：逐字符收缩、到达原 anchor，再越过它；每一步都核完整反向或正向事实。
        for head in 3..=8 {
            permission.command(
                sel!(moveRightAndModifySelection:),
                MarkdownTextSelection {
                    anchor_utf16: 7,
                    head_utf16: head,
                },
                "reverse_cross_anchor",
            )?;
        }
        for head in [7, 6] {
            permission.command(
                sel!(moveLeftAndModifySelection:),
                MarkdownTextSelection {
                    anchor_utf16: 7,
                    head_utf16: head,
                },
                "reverse_return",
            )?;
        }
        permission.source_edges(id_start)
    })();

    // 只在原权限仍有效时恢复明确选择；新输入、模式变化或正文变化后不得夺回用户状态。
    let restored = if permission.current() {
        if permission.entered.get().is_some() || permission.after.get().is_some() {
            Err("标准选择失败动作尚未结算，拒绝重新签发恢复权限".into())
        } else {
            permission.select(original)
        }
    } else {
        Err("标准选择回归已撤权，未覆盖后继选择或模式".into())
    };
    match (result, restored) {
        (Err(first), Err(restore)) => Err(format!("{first}；恢复：{restore}")),
        (Err(first), Ok(())) => Err(first),
        (Ok(()), Err(restore)) => Err(restore),
        (Ok(()), Ok(())) => permission.require_current(),
    }
}
