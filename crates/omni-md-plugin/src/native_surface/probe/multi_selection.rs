//! 实际 Find 多匹配到明确 Next/All 的产品回归；公共命令调用不作为物理键盘验收。

use super::*;

/// 在既有真实probe owner lease内观察旧产品delete:，不以独立NSTextView的行为代替产品合同。
pub(super) fn observe_delete_baseline(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    fixture: &str,
) -> Result<(), String> {
    if fixture.len() > 64 * 1024 {
        return Err("旧多选删除对照只允许固定64KiB内合成夹具".into());
    }
    let document = Rc::downgrade(surface.document());
    let current = || {
        native_markdown_controller_has_foreground_authority(controller)
            && surface.backend.text_view.ivars().folds.owner_is_current()
            && !surface
                .backend
                .text_view
                .ivars()
                .textkit_graph_teardown
                .get()
            && std::rc::Weak::ptr_eq(&document, &Rc::downgrade(surface.document()))
    };
    let require_current = || {
        current()
            .then_some(())
            .ok_or_else(|| "旧多选删除对照失去原真实owner权限".to_owned())
    };
    require_current()?;
    let (generation, original) = {
        let document = surface.document().borrow();
        if document.is_dirty() || document.can_undo() || document.can_redo() {
            return Err("旧多选删除对照需要原保存root和空历史".into());
        }
        (document.generation(), document.selection())
    };
    let positions: Vec<_> = fixture
        .match_indices('x')
        .take(2)
        .map(|(byte, _)| markdown_probe_utf16_offset(fixture, byte))
        .collect::<Result<_, _>>()?;
    if positions.len() != 2 {
        return Err("旧多选删除对照需要两个真实ASCII x".into());
    }
    let ranges: Vec<_> = positions
        .iter()
        .map(|start| NSRange::new(*start, 1))
        .collect();
    let expected = fixture
        .chars()
        .scan(0_usize, |offset, ch| {
            // 对照仅在固定短夹具上构造oracle；产品路径仍消费原mmap和piece tree。
            let keep = !positions.contains(offset);
            *offset += ch.len_utf16();
            Some((ch, keep))
        })
        .filter_map(|(ch, keep)| keep.then_some(ch))
        .collect::<String>();
    autoreleasepool(|_| {
        let values: Vec<_> = ranges
            .iter()
            .map(|range| {
                // SAFETY: 两个真实ASCII范围已经在固定原夹具内认证，NSValue同步复制该有限值。
                unsafe { objc2_foundation::NSValue::valueWithRange(*range) }
            })
            .collect();
        let array = NSArray::from_retained_slice(&values);
        require_current()?;
        surface.backend.text_view.setSelectedRanges(&array);
        require_current()?;
        surface.backend.text_view.ivars().folds.reset_selection();
        let actual = surface.backend.text_view.selectedRanges();
        if actual.count() != ranges.len() {
            return Err("旧图未安装真实多个选择，不能开始删除对照".into());
        }
        for (index, expected) in ranges.iter().enumerate() {
            let value = actual.objectAtIndex(index);
            // SAFETY: 实际NSTextView selectedRanges公开返回的各NSValue包含NSRange。
            if unsafe { value.rangeValue() } != *expected {
                return Err("旧图实际多选范围与原声明不同".into());
            }
        }
        if surface
            .backend
            .text_view
            .ivars()
            .folds
            .known_selection()
            .is_some()
        {
            return Err("旧图多选错误地保留已证明主方向".into());
        }
        require_current()?;
        // SAFETY: 原真实主线程产品view沿既有delete:入口接收空sender，不发送物理键或新增动作。
        unsafe {
            let _: () = msg_send![&*surface.backend.text_view, delete: Option::<&AnyObject>::None];
        }
        require_current()
    })?;
    let (actual, changes, selection) = {
        let document = surface.document().borrow();
        let changes = document
            .generation()
            .checked_sub(generation)
            .ok_or("旧图多选删除代次倒退")?;
        let actual = small_control_text(&document)?;
        (actual, changes, document.selection())
    };
    let removed_all = actual == expected;
    let unchanged = actual == fixture && changes == 0;
    println!(
        "native_legacy_multi_delete observed=true selected_ranges=2 removed_all={removed_all} unchanged={unchanged} core_transactions={changes} after_anchor={} after_head={} formal=false",
        selection.anchor_utf16, selection.head_utf16,
    );
    if changes > 2 {
        return Err("旧多选删除产生超过两项范围的Core修改，不能猜测Undo次数".into());
    }
    for _ in 0..changes {
        require_current()?;
        let before = surface.document().borrow().generation();
        surface
            .backend
            .text_view
            .restore_history(MarkdownHistoryDirection::Undo);
        require_current()?;
        if surface.document().borrow().generation() != before + 1 {
            return Err("旧图多选删除的真实Core Undo没有提交".into());
        }
    }
    {
        let document = surface.document().borrow();
        if small_control_text(&document)? != fixture || document.is_dirty() || document.can_undo() {
            return Err("旧图多选删除后Undo未完整恢复原合成字节或历史边界".into());
        }
    }
    require_current()?;
    surface
        .backend
        .text_view
        .set_semantic_native_selection(original);
    require_current()?;
    if surface.document().borrow().selection() != original
        || surface.backend.text_view.ivars().folds.known_selection() != Some(original)
    {
        return Err("旧图多选删除对照未恢复原明确选择".into());
    }
    if !removed_all && !unchanged {
        return Err("旧图多选删除出现部分修改；已恢复合成夹具，不能把它记作完整成功".into());
    }
    // 实际Native weak与原mmap仍交给同一session既有关闭门禁；本观察不创建另一图或豁免幸存对象。
    println!(
        "native_legacy_multi_delete restored_bytes=true restored_single_memento=true cut_not_executed=true formal=false"
    );
    Ok(())
}

fn small_control_text(document: &MarkdownDocumentBuffer) -> Result<String, String> {
    let length = document.utf16_len();
    if length > 64 * 1024 {
        return Err("旧多选删除对照超过有限oracle范围".into());
    }
    let mut units = vec![0; length];
    document
        .copy_utf16_code_units(0, &mut units)
        .map_err(|error| error.to_string())?;
    String::from_utf16(&units).map_err(|_| "旧图合成正文不再是合法UTF16".into())
}

/// 选择、已知方向、fold 与输入代次必须成组匹配；多选从不以第一范围伪造主 head。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct State {
    selection: MarkdownTextSelection,
    known: Option<MarkdownTextSelection>,
    fold: u64,
    input: u64,
}

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
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    bar_stamp: Cell<(u64, bool)>,
    bar_entered: Cell<Option<(u64, bool)>>,
    bar_after: Cell<Option<(u64, bool)>>,
    state: Cell<State>,
    entered: Cell<Option<State>>,
    after: Cell<Option<State>>,
    phase: Cell<&'static str>,
}

impl Permission<'_> {
    fn actual(&self) -> State {
        State {
            selection: self.surface.document().borrow().selection(),
            known: self
                .surface
                .backend
                .text_view
                .ivars()
                .folds
                .known_selection(),
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
            return false; // 新输入、正文替换、模式或几何改变都撤销本次原场景，不补领新状态。
        }
        if !self
            .surface
            .find_bar
            .borrow()
            .as_ref()
            .is_some_and(|actual| std::rc::Weak::ptr_eq(&self.bar, &Rc::downgrade(actual)))
        {
            return false; // 实际显示完成后只接受同一栏 owner，Rc 存活本身不能授予后继栏权限。
        }
        let Some(bar_stamp) = multi_selection_find_probe_bar_stamp(self.surface) else {
            return false; // 关闭栏不授予旧场景继续显示或收尾的权限。
        };
        if bar_stamp != self.bar_stamp.get()
            && self.bar_entered.get() != Some(bar_stamp)
            && self.bar_after.get() != Some(bar_stamp)
        {
            return false; // 同一 owner 的 hide→show 也有新代次，不能跨 helper 回采后继续旧动作。
        }
        let state = self.actual();
        if state != self.state.get()
            && self.entered.get() != Some(state)
            && self.after.get() != Some(state)
        {
            return false; // 不把不同允许态的 head、Known 或代次交叉组合。
        }
        let document = self.surface.document().borrow();
        // 只在已有小夹具上核摘要，不复制正文；clean 表示仍共享原保存 root，历史须始终为空。
        document.generation() == self.generation
            && document.utf16_len() == self.length
            && document
                .current_revision()
                .is_ok_and(|actual| actual == self.revision)
            && document.original_revision() == self.revision
            && !document.is_dirty()
            && !document.can_undo()
            && !document.can_redo()
    }

    fn require_current(&self) -> Result<(), String> {
        if self.current() {
            return Ok(());
        }
        // 原票失败时仅补充同一组Rust标量；不调用Native getter，也不把观察到的后态变成新权限。
        let view = &self.surface.backend.text_view;
        let storage = &self.surface.backend.text_storage;
        let epochs = [
            self.controller.find_input_intent(),
            self.controller.workspace_query_intent(),
            self.controller.ivars().header_layout_revision.get(),
        ];
        let document = self.surface.document().borrow();
        let document_facts = (
            document.generation() == self.generation,
            document.utf16_len() == self.length,
            document
                .current_revision()
                .is_ok_and(|actual| actual == self.revision),
            document.original_revision() == self.revision,
            !document.is_dirty(),
            !document.can_undo(),
            !document.can_redo(),
        );
        Err(format!(
            "多范围回归原权限已撤销：阶段={} 实际={:?} 原态={:?} 入口态={:?} 目标态={:?} 栏实际={:?} 栏原态={:?} 栏入口={:?} 栏目标={:?} 前台={} 拆图={} fold_owner={} folds_empty={} accepts_edits={} mode_equal={} doc_owner={} bar_owner={} width={:?} geometry={:?} epochs={:?} 原epochs={:?} 文档代次_长度_当前摘要_保存摘要_clean_无Undo_无Redo={:?}",
            self.phase.get(),
            self.actual(),
            self.state.get(),
            self.entered.get(),
            self.after.get(),
            multi_selection_find_probe_bar_stamp(self.surface),
            self.bar_stamp.get(),
            self.bar_entered.get(),
            self.bar_after.get(),
            native_markdown_controller_has_foreground_authority(self.controller),
            view.ivars().textkit_graph_teardown.get(),
            view.ivars().folds.owner_is_current(),
            view.ivars().folds.borrow().is_empty(),
            storage.ivars().accepts_edits.get(),
            storage.presentation_mode() == self.mode,
            WeakMarkdownDocument::ptr_eq(&self.document, &Rc::downgrade(self.surface.document()),),
            self.surface
                .find_bar
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::rc::Weak::ptr_eq(&self.bar, &Rc::downgrade(actual))),
            (storage.ivars().table_projection_width_key.get(), self.width),
            (storage.ivars().table_geometry_epoch.get(), self.geometry),
            epochs,
            self.epochs,
            document_facts,
        )) // 诊断只含固定阶段和合成源坐标，不输出笔记内容或路径。
    }

    fn step<T>(&self, work: impl FnOnce() -> T) -> Result<T, String> {
        native_markdown_table_intent_step(&|| self.current(), work)
            .map_err(|_| format!("多范围原 Native 动作已撤权：{}", self.phase.get()))
    }

    fn finish_pool<T>(&self, result: Result<T, String>) -> Result<T, String> {
        match (result, self.require_current()) {
            (Err(first), Err(after)) => Err(format!("{first}；pool 结束后：{after}")),
            (Err(first), Ok(())) => Err(first), // 保留首因，同时给真正撤权的返回补充原票差异。
            (Ok(_), Err(after)) => Err(after),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    /// 首 Native 前声明唯一 after；命令入口撤旧意图时才额外允许一个精确 entered。
    fn prepare(
        &self,
        selection: MarkdownTextSelection,
        known: Option<MarkdownTextSelection>,
        fold_delta: u64,
        input_delta: u64,
        phase: &'static str,
    ) -> Result<State, String> {
        self.require_current()?;
        if self.after.get().is_some()
            || self.entered.get().is_some()
            || self.bar_after.get().is_some()
        {
            return Err("多范围回归仍有未结算动作".into());
        }
        let before = self.state.get();
        let after = State {
            selection,
            known,
            fold: before
                .fold
                .checked_add(fold_delta)
                .ok_or("多范围 fold 代次耗尽")?,
            input: before
                .input
                .checked_add(input_delta)
                .ok_or("多范围输入代次耗尽")?,
        };
        self.phase.set(phase);
        self.after.set(Some(after));
        self.entered.set((input_delta != 0).then_some(State {
            input: after.input,
            ..before
        }));
        Ok(after)
    }

    fn settle(&self, expected: State) -> Result<(), String> {
        self.require_current()?;
        if self.after.get() != Some(expected) || self.actual() != expected {
            return Err(format!(
                "多范围动作未达到原预声明目标：阶段={} 实际={:?} 预期={expected:?}",
                self.phase.get(),
                self.actual(),
            ));
        }
        self.state.set(expected); // 只消费调用前签出的转换结果，不以实际值重新起票。
        self.after.set(None);
        self.entered.set(None);
        self.require_current()
    }

    fn prepare_bar(&self, visible: bool, phase: &'static str) -> Result<(u64, bool), String> {
        self.require_current()?;
        if self.after.get().is_some() || self.bar_after.get().is_some() {
            return Err("显示栏动作不能与未结算的正文或栏转换交叉".into());
        }
        let before = self.bar_stamp.get();
        // show 每次推进一次；hide 已隐藏时不制造布局转换，其他情况只接受原唯一后继。
        let revision = before
            .0
            .checked_add(u64::from(visible || before.1))
            .ok_or("多范围栏布局代次耗尽")?;
        let after = (revision, visible);
        self.phase.set(phase);
        self.bar_entered.set(Some((revision, before.1)));
        self.bar_after.set(Some(after));
        Ok(after)
    }

    fn settle_bar(&self, after: (u64, bool)) -> Result<(), String> {
        self.require_current()?;
        if self.bar_after.get() != Some(after)
            || multi_selection_find_probe_bar_stamp(self.surface) != Some(after)
        {
            return Err("Find 栏没有完成原预声明的实际显示或隐藏转换".into());
        }
        self.bar_stamp.set(after);
        self.bar_entered.set(None);
        self.bar_after.set(None);
        self.require_current()
    }

    fn show(&self) -> Result<Rc<NativeMarkdownFindBar>, String> {
        let after = self.prepare_bar(true, "actual_find_bar_show")?;
        let result = autoreleasepool(|_| {
            prepare_multi_selection_find_probe(self.surface, self.controller, &|| self.current())
        });
        let bar = self.finish_pool(result)?;
        self.settle_bar(after)?;
        if !std::rc::Weak::ptr_eq(&self.bar, &Rc::downgrade(&bar)) {
            return Err("实际显示 Find 栏不是首 Native 前固定的原 owner".into());
        }
        Ok(bar)
    }

    fn hide(&self) -> Result<(), String> {
        let after = self.prepare_bar(false, "actual_find_bar_hide")?;
        let result = autoreleasepool(|_| {
            finish_list_marker_find_probe(self.surface, self.controller, &|| self.current())
        });
        self.finish_pool(result)?;
        self.settle_bar(after)
    }

    fn select(&self, selection: MarkdownTextSelection) -> Result<(), String> {
        let after = self.prepare(
            selection,
            Some(selection),
            u64::from(self.state.get().selection != selection),
            0,
            "semantic_selection",
        )?;
        let result = autoreleasepool(|_| {
            self.step(|| {
                self.surface
                    .backend
                    .text_view
                    .set_semantic_native_selection(selection)
            })
        });
        self.finish_pool(result)?;
        self.settle(after)?;
        self.single(selection)
    }

    fn all(&self) -> Result<(), String> {
        self.require_current()?;
        let selection = MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: self.length,
        };
        let original = self
            .surface
            .backend
            .text_view
            .ivars()
            .folds
            .capture_selection()
            .map_err(|_| "标准全选没有原选择票据")?;
        let after = self.prepare(
            selection,
            Some(selection),
            u64::from(self.state.get().selection != selection),
            1,
            "standard_all",
        )?;
        let result = autoreleasepool(|_| {
            let input =
                ProtocolObject::<dyn NSTextInputClient>::from_ref(self.surface.backend.text_view());
            self.step(|| {
                // SAFETY: selectAll: 是已注册标准 selector；原产品 view 在主线程同步接收，无逃逸参数。
                unsafe { input.doCommandBySelector(sel!(selectAll:)) };
            })
        });
        self.finish_pool(result)?;
        self.settle(after)?;
        if original.current() {
            return Err("标准全选没有撤销原选择 serial 票据".into()); // Core 范围相同也必须形成新的明确方向事实。
        }
        self.single(selection)
    }

    fn find_all(&self, bar: &Rc<NativeMarkdownFindBar>) -> Result<(), String> {
        let before = self.state.get();
        let after = self.prepare(before.selection, None, 1, 0, "find_select_all")?;
        let result = autoreleasepool(|_| {
            let request = execute_multi_selection_find_probe(
                self.surface,
                self.controller,
                bar,
                self.epochs[0],
                MarkdownFindAction::SelectAll,
                None,
                &|| self.current(),
            )?;
            if request.is_some() {
                return Err("Find 多匹配错误地产生了定位完成收据".into());
            }
            Ok(())
        });
        self.finish_pool(result)?;
        self.settle(after)?;
        self.multiple(before.selection)
    }

    fn next(&self, bar: &Rc<NativeMarkdownFindBar>, x: usize) -> Result<(), String> {
        let selection = MarkdownTextSelection {
            anchor_utf16: x,
            head_utf16: x + 1,
        };
        let after = self.prepare(
            selection,
            Some(selection),
            u64::from(self.state.get().selection != selection),
            0,
            "find_next_from_multi",
        )?;
        let result = autoreleasepool(|_| {
            execute_multi_selection_find_probe(
                self.surface,
                self.controller,
                bar,
                self.epochs[0],
                MarkdownFindAction::Next,
                Some(x..x + 1),
                &|| self.current(),
            )?
            .ok_or_else(|| "多范围后的真实 Next 没有原定位请求".to_owned())
        });
        let request = self.finish_pool(result)?;
        self.settle(after)?;
        self.single(selection)?;
        // 只运行一次已调度的生产有界 executor；原 Rc 的真实终点含 prepared 与实际可见性门禁。
        let result = autoreleasepool(|_| {
            self.step(|| {
                self.surface
                    .backend
                    .text_view
                    .run_scheduled_table_layout_update()
            })
        });
        self.finish_pool(result)?;
        if request.probe_completed() != Some(true) {
            return Err(format!(
                "多范围 Next 原请求未在一次真实 executor 完成：{:?}",
                request.probe_completed()
            ));
        }
        self.single(selection)?;
        drop(request); // 原请求只含纯收据和 Weak；不保留临时 context 或第二个 Native 图。
        self.require_current()
    }

    fn multiple(&self, before: MarkdownTextSelection) -> Result<(), String> {
        if self.surface.document().borrow().selection() != before
            || self
                .surface
                .backend
                .text_view
                .ivars()
                .folds
                .known_selection()
                .is_some()
        {
            return Err("Find 多范围伪造了 Core 主 head 或没有清除 Known".into());
        }
        let result = autoreleasepool(|_| {
            let state = self
                .step(|| {
                    self.surface
                        .backend
                        .text_view
                        .native_ordered_selection_state()
                })?
                .map_err(|_| "无法观察实际 Native 多范围")?;
            if state != NativeMarkdownOrderedSelection::Multiple {
                return Err(format!("Find 多匹配没有产生实际 Multiple：{state:?}"));
            }
            match self.step(|| self.surface.backend.text_view.primary_text_selection())? {
                Err(NativeMarkdownFoldError::UnresolvedSelection(capture)) if capture.current() => {
                    Ok(())
                }
                _ => Err("实际多范围错误地取得了已证明的主 head".into()),
            }
        });
        self.finish_pool(result)?;
        self.graph()
    }

    fn single(&self, expected: MarkdownTextSelection) -> Result<(), String> {
        self.require_current()?;
        if self
            .surface
            .backend
            .text_view
            .ivars()
            .folds
            .known_selection()
            != Some(expected)
        {
            return Err("明确新选择未发布相同的 Known 完整方向".into());
        }
        let result = autoreleasepool(|_| {
            let actual = self
                .step(|| {
                    self.surface
                        .backend
                        .text_view
                        .native_ordered_selection_state()
                })?
                .map_err(|_| "明确新选择没有实际有序范围")?;
            let start = expected.anchor_utf16.min(expected.head_utf16);
            let end = expected.anchor_utf16.max(expected.head_utf16);
            if actual != NativeMarkdownOrderedSelection::Single(NSRange::new(start, end - start)) {
                return Err(format!(
                    "明确新选择未收拢实际多范围：actual={actual:?} expected={expected:?}"
                ));
            }
            let captured = self
                .step(|| self.surface.backend.text_view.primary_text_selection())?
                .map_err(|_| "明确新选择缺少实际主选择证明")?;
            if !captured.proven_head || !captured.current() || captured.selection != expected {
                return Err("明确新选择的实际 anchor/head 与 Core 不一致".into());
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
                .ok_or("多范围回归失去原 manager")?;
            let content = self
                .step(|| manager.textContentManager())?
                .ok_or("多范围回归失去原 content")?;
            let view_content = self
                .step(|| self.surface.backend.text_view.textContentStorage())?
                .ok_or("多范围回归失去 view content")?;
            let container = self
                .step(|| manager.textContainer())?
                .ok_or("多范围回归失去原 container")?;
            let view_container = self
                .step(|| {
                    // SAFETY: 活产品 view 在主线程，公开 getter 不创建兼容布局。
                    unsafe { self.surface.backend.text_view.textContainer() }
                })?
                .ok_or("多范围回归失去 view container")?;
            let inverse = self
                .step(|| container.textLayoutManager())?
                .ok_or("多范围回归失去 container 反向 manager")?;
            let observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*view_content);
            let storage = self
                .step(|| observer.textStorage())?
                .ok_or("多范围回归失去原 piece storage")?;
            if !std::ptr::eq(&*manager, &*self.surface.backend.text_layout_manager)
                || !std::ptr::eq::<objc2_app_kit::NSTextContentManager>(
                    &*content,
                    self.surface.backend.text_content_storage.as_super(),
                )
                || !std::ptr::eq(&*view_content, &*self.surface.backend.text_content_storage)
                || !std::ptr::eq(&*container, &*self.surface.backend.text_container)
                || !std::ptr::eq(&*view_container, &*self.surface.backend.text_container)
                || !std::ptr::eq(&*inverse, &*self.surface.backend.text_layout_manager)
                || !std::ptr::eq::<NSTextStorage>(&*storage, self.surface.backend.text_storage())
            {
                return Err("多范围转换更换或拆开了原产品 TextKit 图".into());
            }
            Ok(())
        });
        self.finish_pool(result)
    }

    fn source(&self, before: usize) -> Result<(), String> {
        let result = autoreleasepool(|_| {
            let source = self.step(|| self.surface.backend.text_storage().string())?;
            let mut units = [0u16; 6];
            self.step(|| {
                // SAFETY: 首 Native 前已核 before..before+6 为固定任务前缀；栈数组足够写入全部源 unit。
                unsafe {
                    source.getCharacters_range(
                        NonNull::new(units.as_mut_ptr()).unwrap(),
                        NSRange::new(before, 6),
                    )
                };
            })?;
            if units.into_iter().ne("\n- [x]".encode_utf16()) {
                return Err("多范围回归的实际任务源 unit 已改变".into());
            }
            Ok(())
        });
        self.finish_pool(result)
    }

    /// 收尾仅承认原声明的 before/entered/after；失败仍保留失败，结算不把它提升为成功。
    fn settle_for_cleanup(&self) -> Result<(), String> {
        self.require_current()?;
        let actual = self.actual();
        if let Some(after) = self.after.get() {
            if actual == after {
                self.state.set(after);
            } else if let Some(entered) = self.entered.get()
                && actual == entered
            {
                self.state.set(entered);
            } else if actual != self.state.get() {
                return Err("多范围失败后没有可消费的原声明状态".into());
            }
            self.after.set(None);
            self.entered.set(None);
        }
        if let Some(after) = self.bar_after.get() {
            let actual = multi_selection_find_probe_bar_stamp(self.surface);
            if actual == Some(after) {
                self.bar_stamp.set(after);
            } else if let Some(entered) = self.bar_entered.get()
                && actual == Some(entered)
            {
                self.bar_stamp.set(entered);
            } else if actual != Some(self.bar_stamp.get()) {
                return Err("失败栏转换没有可消费的原声明状态".into());
            }
            self.bar_after.set(None);
            self.bar_entered.set(None);
        }
        self.require_current()
    }
}

/// 沿原 owner lease 验证真实多选；不创建窗口、嵌套 run loop、重试或第二份全文。
pub(super) fn verify(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    fixture: &str,
) -> Result<(), String> {
    if fixture.len() > 64 * 1024
        || fixture
            .chars()
            .filter(|ch| matches!(*ch, 'x' | 'X'))
            .count()
            < 2
    {
        return Err("多范围回归只接受固定小夹具和至少两处字面 x".into());
    }
    let checked_byte = fixture
        .find("- [x] 已完成任务")
        .ok_or("多范围夹具缺少固定任务")?;
    let checked = markdown_probe_utf16_offset(fixture, checked_byte)?;
    let before = checked.checked_sub(1).ok_or("任务前缺少可验证原位置")?;
    let x = checked.checked_add(3).ok_or("任务 x 坐标溢出")?;
    let (generation, revision, length, original) = {
        let document = surface.document().borrow();
        if document.is_dirty() || document.can_undo() || document.can_redo() {
            return Err("多范围回归必须从保存 root 和空历史开始".into());
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
    if length != fixture.encode_utf16().count()
        || before.checked_add(6).is_none_or(|end| end > length)
        || surface.backend.text_view.ivars().folds.known_selection() != Some(original)
    {
        return Err("多范围夹具与原正文或明确选择不符".into());
    }
    let original_bar = surface
        .find_bar
        .borrow()
        .as_ref()
        .map(Rc::downgrade)
        .ok_or("多范围回归需要先前列表回归登记的原 Find 栏")?;
    let original_bar_stamp =
        multi_selection_find_probe_bar_stamp(surface).ok_or("多范围回归的原 Find 栏已经关闭")?;
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
        bar: original_bar,
        bar_stamp: Cell::new(original_bar_stamp),
        bar_entered: Cell::new(None),
        bar_after: Cell::new(None),
        state: Cell::new(State {
            selection: original,
            known: Some(original),
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
        permission.single(original)?;
        permission.source(before)?;
        permission.select(MarkdownTextSelection::caret(before))?;
        let bar = permission.show()?;
        permission.require_current()?;

        // 多选保留原 caret，不从数组捏造主 head；下一真实 Next 仍必须选择任务内部唯一 x。
        permission.find_all(&bar)?;
        permission.next(&bar, x)?;

        // Core 先明确全选，再以 Find 多选清除 Known；最后 All 的 Core 范围相同但 Native 必须收拢。
        permission.all()?;
        permission.find_all(&bar)?;
        let before_all = permission.state.get();
        permission.all()?;
        if permission.state.get().selection != before_all.selection
            || permission.state.get().fold != before_all.fold
        {
            return Err("相同 Core 全选范围的转换意外改变选择或 fold revision".into());
        }
        permission.source(before)
    })();

    let restored = (|| {
        permission.settle_for_cleanup()?;
        permission.hide()?;
        permission.select(original)?;
        permission.source(before)
    })();
    drop(_payload); // 业务 guard 不跨最终收据；析构结束后仍核原 owner/source/input。
    let restored = restored.and_then(|()| permission.require_current());
    match (result, restored) {
        (Err(first), Err(cleanup)) => Err(format!("{first}；恢复：{cleanup}")),
        (Err(first), Ok(())) => Err(first),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Ok(()), Ok(())) => Ok(()),
    }
}
