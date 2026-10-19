//! 同一真实输入尾声中的有限故障注入；只验证旧显示权限不可因选择 ABA 而复活。

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultKind {
    ForeignRange,
    MultipleRanges,
    ReaderReentry,
    ReaderFailure,
}

/// 外部故障 driver 的原请求只保存标量与 Weak；它不借用待撤销收据为第二次 A 续权。
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionFault {
    kind: FaultKind,
    document: WeakMarkdownDocument,
    generation: u64,
    bytes: usize,
    units: usize,
    epoch: u64,
    after: MarkdownTextSelection,
    a: NSRange,
    b: NSRange,
    c: NSRange,
    owner: RefCell<Option<Rc<NativeMarkdownPendingSelectionOwner>>>,
    injected: Cell<bool>,
    armed: Cell<bool>,
    seam_seen: Cell<bool>,
    observations: Cell<usize>,
    error: RefCell<Option<String>>,
}

impl NativeMarkdownPendingSelectionFault {
    fn fail(&self, error: impl Into<String>) {
        if self.error.borrow().is_none() {
            self.error.replace(Some(error.into())); // 保留首个失败；不能由后续 A 覆盖真实拒绝原因。
        }
    }

    /// 这个权限先于公开 insertText 声明，仅容许它唯一的字符后态；不依赖 Known 或旧 serial。
    fn driver_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        !view.ivars().textkit_graph_teardown.get()
            && self.document.upgrade().is_some_and(|document| {
                let document = document.borrow();
                document.generation() == self.generation
                    && document.byte_len() == self.bytes
                    && document.utf16_len() == self.units
                    && document.selection() == self.after
                    && !document.has_prepared_text_input()
            })
    }

    fn step<T>(
        &self,
        view: &PieceBackedMarkdownTextView,
        work: impl FnOnce() -> T,
    ) -> Result<T, String> {
        native_markdown_table_intent_step(&|| self.driver_current(view), work)
            .map_err(|_| "pending 故障 driver 的原字符后态已撤销".to_owned())
    }

    /// getter 使用 driver 的原有限后态守门；它观察真实 superclass，不领取 semantic 新 capture。
    fn raw(&self, view: &PieceBackedMarkdownTextView) -> Result<(bool, usize, NSRange), String> {
        self.step(view, || {
            autoreleasepool(|_| {
                let ranges: Option<Retained<NSArray<objc2_foundation::NSValue>>> =
                    self.step(view, || {
                        // SAFETY: 同一存活正文视图的公开 nullable getter；无正文复制、手工通知或虚拟结果。
                        unsafe { msg_send![super(view), selectedRanges] }
                    })?;
                let present = ranges.is_some();
                let count = match &ranges {
                    Some(ranges) => self.step(view, || ranges.count())?,
                    None => 0, // 实际 nil 独立保留，不能作为本测试的 reader-failure 注入结果。
                };
                let range = self.step(view, || view.super_selected_range())?;
                drop(ranges); // 外层原 driver 门禁还覆盖 claim 析构和 autoreleasepool drain。
                Ok((present, count, range))
            })
        })?
    }

    /// 真正 setter 属于外部故障动作；原 receipt 失效后仍允许已声明的第二个 A，以证明不能复活。
    fn set_ranges(
        &self,
        view: &PieceBackedMarkdownTextView,
        values: &[NSRange],
    ) -> Result<(), String> {
        self.step(view, || {
            autoreleasepool(|_| {
                let values: Vec<_> = values
                    .iter()
                    .map(|range| {
                        self.step(view, || {
                            // SAFETY: NSValue 只封装首 Native 前声明的有限 NSRange 标量，不借用外部缓冲。
                            unsafe { objc2_foundation::NSValue::valueWithRange(*range) }
                        })
                    })
                    .collect::<Result<_, _>>()?;
                let references: Vec<_> = values.iter().map(|value| &**value).collect();
                let ranges = self.step(view, || NSArray::from_slice(&references))?;
                self.step(view, || {
                    // SAFETY: 每个坐标都在首 Native 前从 canonical ASCII 边界选定；仅向实际 super 发一次 setter。
                    unsafe {
                        let _: () = msg_send![super(view), setSelectedRanges: &*ranges];
                    }
                })?;
                drop(ranges);
                drop(values);
                Ok(())
            })
        })?
    }

    fn require_rejected(
        &self,
        view: &PieceBackedMarkdownTextView,
        expected_count: usize,
        expected_raw: NSRange,
        phase: &'static str,
    ) -> Result<(), String> {
        let (present, count, raw) = self.raw(view)?;
        let owner = self
            .owner
            .borrow()
            .as_ref()
            .map(Rc::clone)
            .ok_or("pending 故障缺少原 owner")?;
        let known = view.ivars().folds.known_selection();
        if !present
            || count != expected_count
            || raw != expected_raw
            || owner.current()
            || owner.committed.current()
            || known.is_some()
        {
            return Err(format!(
                "pending 故障未终态拒绝：kind={:?} phase={phase} present={present} count={count}/{expected_count} raw={raw:?}/{expected_raw:?} owner={} receipt={} known={known:?}",
                self.kind,
                owner.current(),
                owner.committed.current()
            ));
        }
        self.observations.set(self.observations.get() + 1);
        eprintln!(
            "omni_pending_fault kind={:?} phase={phase} count={count} raw={raw:?} receipt_current=false known=false id={} source={} pending_phase={:?}",
            self.kind,
            owner.id,
            owner.source,
            owner.phase.get()
        );
        Ok(())
    }

    fn run(&self, view: &PieceBackedMarkdownTextView) -> Result<(), String> {
        match self.kind {
            FaultKind::ForeignRange => {
                self.set_ranges(view, &[self.b])?;
                self.require_rejected(view, 1, self.b, "foreign_b")?;
                self.set_ranges(view, &[self.a])?;
                self.require_rejected(view, 1, self.a, "return_a")?;
            }
            FaultKind::MultipleRanges => {
                self.set_ranges(view, &[self.b, self.c])?;
                self.require_rejected(view, 2, self.b, "multiple_bc")?;
                self.set_ranges(view, &[self.a])?;
                self.require_rejected(view, 1, self.a, "multiple_return_a")?;
            }
            FaultKind::ReaderReentry | FaultKind::ReaderFailure => {
                // 只有这次实际 setter 引发的 delegate 才能消费 one-shot seam；之前输入通知不能抢走它。
                self.armed.set(true);
                self.set_ranges(view, &[self.b])?;
                if !self.seam_seen.get() || self.armed.get() {
                    return Err("pending 读取故障 seam 未由真实 delegate 消费".into());
                }
                let raw = if self.kind == FaultKind::ReaderReentry {
                    self.a
                } else {
                    self.b
                };
                self.require_rejected(view, 1, raw, "reader_return")?;
                self.set_ranges(view, &[self.a])?;
                self.require_rejected(view, 1, self.a, "reader_return_a")?;
            }
        }
        Ok(())
    }

    /// 产品 hook 位于真实 superclass 输入返回后、原 guard 清理与最终成功计数之前。
    pub(in crate::native_surface) fn inject_after_super(
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        transaction: &Rc<NativeMarkdownInputTransaction>,
    ) {
        let plan = storage
            .ivars()
            .pending_selection_fault
            .borrow()
            .as_ref()
            .map(Rc::clone);
        let Some(plan) = plan else {
            return;
        }; // 默认产品输入不增加 getter、诊断或副作用。
        if plan.injected.replace(true) {
            return;
        }
        let result = (|| -> Result<(), String> {
            if transaction.epoch != plan.epoch || !plan.driver_current(view) {
                return Err("pending 故障没有命中原输入唯一后态".into());
            }
            let committed = transaction.committed().ok_or("pending 故障输入尚未提交")?;
            let owner = storage
                .ivars()
                .pending_selection
                .borrow()
                .as_ref()
                .map(Rc::clone)
                .ok_or("pending 故障输入未持有原 owner")?;
            if !Rc::ptr_eq(&owner.committed, &committed)
                || !owner.current()
                || owner.source_hits(plan.b) != 0
                || owner.source_hits(plan.c) != 0
                || owner.source_hits(plan.a) == 0
            {
                return Err("pending 故障来源不是原固定集合外的 B/C 与集合内的 A".into());
            }
            plan.owner.replace(Some(owner)); // 原实际 Rc；不复制 Committed，也不捕获当前 Core 新票。
            plan.run(view)
        })();
        if let Err(error) = result {
            plan.fail(error);
        }
    }

    /// 返回 false 明确表示一次 reader failure；不得伪造 nullable getter 曾经返回 nil。
    pub(in crate::native_surface) fn before_read(
        view: &PieceBackedMarkdownTextView,
        owner: &Rc<NativeMarkdownPendingSelectionOwner>,
    ) -> bool {
        let plan = owner.fault.as_ref().and_then(std::rc::Weak::upgrade);
        let Some(plan) = plan else {
            return true;
        }; // 默认 None 只读纯 Weak；不增加任何 Native getter。
        if !plan.armed.get()
            || !plan
                .owner
                .borrow()
                .as_ref()
                .is_some_and(|original| Rc::ptr_eq(original, owner))
        {
            return true; // 早期输入通知及其他 owner 均不能消费这个外部 driver 的 seam。
        }
        plan.armed.set(false);
        plan.seam_seen.set(true);
        match plan.kind {
            FaultKind::ReaderReentry => {
                // 外层 sample guard 仍在栈上；真实 C setter 的同步 delegate 必须在递归 getter 前撤权。
                let result = (|| -> Result<(), String> {
                    plan.set_ranges(view, &[plan.c])?;
                    plan.require_rejected(view, 1, plan.c, "reading_reentry_c")?;
                    plan.set_ranges(view, &[plan.a])?;
                    plan.require_rejected(view, 1, plan.a, "reading_reentry_a")
                })();
                if let Err(error) = result {
                    plan.fail(error);
                }
                true // 原 reader 随后的 original-owner 门禁必须拒绝，不能因 A 已恢复而继续 getter。
            }
            FaultKind::ReaderFailure => false,
            FaultKind::ForeignRange | FaultKind::MultipleRanges => {
                plan.fail("不应为非读取场景消费 reader seam");
                false
            }
        }
    }
}

fn counts(surface: &NativeMarkdownEditorSurface) -> [u64; 4] {
    let state = surface.backend.text_storage.ivars();
    [
        state.character_process_count.get(),
        state.input_commit_count.get(),
        state.input_callback_success_count.get(),
        state.autosave_enqueue_count.get(),
    ]
}

fn require_single(
    surface: &NativeMarkdownEditorSurface,
    expected: MarkdownTextSelection,
) -> Result<(), String> {
    let captured = surface
        .backend
        .text_view
        .primary_text_selection()
        .map_err(|error| error.to_string())?;
    let actual = surface
        .backend
        .text_view
        .native_ordered_selection_state_for_capture(captured.native_capture())
        .map_err(|error| error.to_string())?;
    let range = NSRange::new(
        expected.anchor_utf16.min(expected.head_utf16),
        expected.anchor_utf16.abs_diff(expected.head_utf16),
    );
    if captured.selection != expected
        || !captured.current()
        || surface.backend.text_view.ivars().folds.known_selection() != Some(expected)
        || actual != NativeMarkdownOrderedSelection::Single(range)
    {
        return Err("pending 故障的合法重建没有真实 Known/Single 完整选择".into());
    }
    Ok(())
}

/// 每个场景只增加一笔实际字符编辑；标准 selectAll 重建显示后，一次真实 Undo 必须恢复原 root。
pub(super) fn verify(surface: &NativeMarkdownEditorSurface) -> Result<(), String> {
    if std::env::var_os("OMNI_NATIVE_PROBE_PENDING_SELECTION_FAULT").is_none() {
        return Ok(());
    }
    let saved_selection = surface
        .backend
        .text_view
        .primary_text_selection()
        .map_err(|error| error.to_string())?
        .selection;
    for kind in [
        FaultKind::ForeignRange,
        FaultKind::MultipleRanges,
        FaultKind::ReaderReentry,
        FaultKind::ReaderFailure,
    ] {
        surface
            .backend
            .text_view
            .set_semantic_native_selection(MarkdownTextSelection::caret(0));
        require_single(surface, MarkdownTextSelection::caret(0))?;
        let (generation, revision, bytes, units, dirty, undo, b, c) = {
            let document = surface.document().borrow();
            let mut candidates = [None; 2];
            let mut found = 0;
            for offset in 2..document.utf16_len().min(4096) {
                // 只读两个 canonical ASCII scalar；插入起点为零，后态坐标精确右移一单元。
                // 两范围之间至少留一个单元，避免 AppKit 正常合并相邻选择而让本场景并非实际 Multiple。
                if candidates[0]
                    .is_some_and(|first: NSRange| offset < first.location + first.length)
                {
                    continue;
                }
                if document
                    .utf16_code_unit_at(offset)
                    .is_ok_and(|unit| (0x21..=0x7e).contains(&unit))
                    && document.byte_offset_for_utf16(offset).is_ok()
                    && document.byte_offset_for_utf16(offset + 1).is_ok()
                {
                    candidates[found] = Some(NSRange::new(offset + 1, 1));
                    found += 1;
                    if found == 2 {
                        break;
                    }
                }
            }
            (
                document.generation(),
                document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                document.byte_len(),
                document.utf16_len(),
                document.is_dirty(),
                document.can_undo(),
                candidates[0].ok_or("pending 故障夹具没有 ASCII B")?,
                candidates[1].ok_or("pending 故障夹具没有 ASCII C")?,
            )
        };
        let storage = &surface.backend.text_storage;
        let plan = Rc::new(NativeMarkdownPendingSelectionFault {
            kind,
            document: Rc::downgrade(surface.document()),
            generation: generation.checked_add(1).ok_or("pending 故障代次溢出")?,
            bytes: bytes.checked_add(1).ok_or("pending 故障字节数溢出")?,
            units: units.checked_add(1).ok_or("pending 故障 UTF16 长度溢出")?,
            epoch: storage
                .ivars()
                .input_transaction_epoch
                .get()
                .checked_add(1)
                .ok_or("pending 故障输入代次溢出")?,
            after: MarkdownTextSelection::caret(1),
            a: NSRange::new(1, 0),
            b,
            c,
            owner: RefCell::new(None),
            injected: Cell::new(false),
            armed: Cell::new(false),
            seam_seen: Cell::new(false),
            observations: Cell::new(0),
            error: RefCell::new(None),
        });
        if storage.ivars().pending_selection_fault.borrow().is_some() {
            return Err("pending 故障 driver 槽已由其他请求占用".into());
        }
        storage
            .ivars()
            .pending_selection_fault
            .replace(Some(Rc::clone(&plan)));
        let before = counts(surface);
        let incoming = NSString::from_str("x");
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.backend.text_view());
        // SAFETY: 小合成夹具的真实公开输入；原请求提前声明唯一字符、after、g+1、epoch+1 与 B/C。
        unsafe {
            client.insertText_replacementRange(&incoming, NSRange::new(0, 0));
        }
        drop(incoming);
        let removed = {
            let mut slot = storage.ivars().pending_selection_fault.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &plan))
            {
                slot.take()
            } else {
                None
            }
        };
        drop(removed); // 只清原 Rc；成功、注入失败或产品拒绝均不留下诊断 owner。
        if let Some(error) = plan.error.borrow().as_ref() {
            return Err(error.clone());
        }
        let after = counts(surface);
        let owner = plan
            .owner
            .borrow()
            .as_ref()
            .map(Rc::clone)
            .ok_or("pending 故障没有保存真实提交 Rc")?;
        if !plan.injected.get()
            || plan.observations.get() < 2
            || !plan.driver_current(&surface.backend.text_view)
            || owner.current()
            || owner.committed.current()
            || surface
                .backend
                .text_view
                .ivars()
                .folds
                .known_selection()
                .is_some()
            || after[0] <= before[0]
            || after[1] != before[1] + 1
            || after[2] != before[2]
            || after[3] != before[3] + 1
            || storage.ivars().input_transaction.borrow().is_some()
            || storage.ivars().pending_selection.borrow().is_some()
            || surface.document().borrow().has_prepared_text_input()
        {
            return Err(format!(
                "pending 故障没有唯一字符提交与清理终点：kind={kind:?} before={before:?} after={after:?}"
            ));
        }
        // 旧输入和 pending 都已结束；只有这次新的标准产品全选动作可以重建 Known。
        // SAFETY: selectAll: 是正文已注册的标准产品动作；旧输入及 pending 均已证明清空。
        unsafe {
            client.doCommandBySelector(sel!(selectAll:));
        }
        require_single(
            surface,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: units + 1,
            },
        )?;
        if owner.current() || owner.committed.current() {
            return Err("标准全选错误复活了旧 pending 收据".into());
        }
        // SAFETY: 一次标准 Undo 恢复唯一插入；若故障 setter 写入了额外历史，原 revision 将无法恢复。
        unsafe {
            client.doCommandBySelector(sel!(undo:));
        }
        let restored = {
            let document = surface.document().borrow();
            document.generation() == generation + 2
                && document
                    .current_revision()
                    .is_ok_and(|actual| actual == revision)
                && document.byte_len() == bytes
                && document.utf16_len() == units
                && document.is_dirty() == dirty
                && document.can_undo() == undo
                && document.can_redo()
                && !document.has_prepared_text_input()
        };
        let after_undo = counts(surface);
        require_single(surface, MarkdownTextSelection::caret(0))?;
        if !restored
            || owner.current()
            || owner.committed.current()
            || after_undo[1] != after[1]
            || after_undo[2] != after[2]
            || after_undo[3] != after[3] + 1
        {
            return Err(format!(
                "pending 故障 Undo 未恢复原 root：kind={kind:?} restored={restored} counts={after:?}/{after_undo:?}"
            ));
        }
        eprintln!(
            "omni_pending_fault kind={kind:?} restored=true generation_steps=2 input_commit_delta=1 callback_success_delta=0 observations={}",
            plan.observations.get()
        );
    }
    // 原 root 已恢复；这是新的显式产品选择目标，旧 owner 不会参与后继场景。
    surface
        .backend
        .text_view
        .set_semantic_native_selection(saved_selection);
    require_single(surface, saved_selection)
}
