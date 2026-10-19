//! 原字符提交通知的有限选择权限；拒绝状态在同一 owner 生命周期内不可恢复。

use super::*;

#[derive(Clone, Copy, Debug)]
pub(in crate::native_surface) enum NativeMarkdownPendingSelectionPhase {
    Published,
    EditedNotification,
    Attributes,
    EndEditing,
    SuperclassTail,
}

/// 这些坐标只来自原请求或纯 Core 编辑结果；观测到的 Native 值永不加入候选来源。
#[derive(Clone, Copy)]
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionSources {
    pub(in crate::native_surface) source: &'static str,
    pub(in crate::native_surface) replacement: Option<NSRange>,
    pub(in crate::native_surface) caret: Option<NSRange>,
    pub(in crate::native_surface) marked_before: Option<NSRange>,
    pub(in crate::native_surface) marked_after: Option<NSRange>,
    // 只有与原 prepared input 完全匹配的字符 primitive 才声明 superclass 的删除阶段尾点。
    pub(in crate::native_surface) is_input_replacement: bool,
}

impl NativeMarkdownPendingSelectionSources {
    pub(in crate::native_surface) fn canonical(
        source: &'static str,
        edit: &MarkdownHistoryTextEdit,
    ) -> Self {
        let inserted = edit
            .replaced_utf16_range
            .len()
            .checked_add_signed(edit.change_in_utf16_length);
        let caret = inserted
            .and_then(|length| edit.replaced_utf16_range.start.checked_add(length))
            .map(|offset| NSRange::new(offset, 0));
        Self {
            source,
            replacement: Some(NSRange::new(
                edit.replaced_utf16_range.start,
                edit.replaced_utf16_range.len(),
            )),
            caret,
            marked_before: None,
            marked_after: None,
            is_input_replacement: false,
        }
    }
}

/// 两个附加尾点只从原长度和原替换参数推出；实际 getter 不能给这些来源补值。
fn native_markdown_pending_selection_eof_sources(
    before_len: usize,
    after_len: usize,
    sources: NativeMarkdownPendingSelectionSources,
) -> Option<(NSRange, Option<NSRange>)> {
    let replacement = sources.replacement?;
    let replacement_end = replacement.location.checked_add(replacement.length)?;
    if replacement_end > before_len {
        return None; // 原替换必须完整属于首 Native 前的 root，越界参数不取得任何 EOF 权限。
    }
    let caret = sources.caret?;
    if caret.length != 0 {
        return None; // 插入末端是原请求派生的单 caret，不能把任意范围当作输入长度。
    }
    let inserted_len = caret.location.checked_sub(replacement.location)?;
    let deleted_len = before_len.checked_sub(replacement.length)?;
    if deleted_len.checked_add(inserted_len)? != after_len {
        return None; // 原替换与实际候选新长度必须精确一致，不能以 raw 值或夹取修正差异。
    }
    Some((
        NSRange::new(before_len, 0),
        sources
            .is_input_replacement
            .then_some(NSRange::new(deleted_len, 0)),
    ))
}

/// 同一个实际返回收据 Rc 穿过通知、池释放和 superclass 尾声；不持正文或 Native 图。
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionOwner {
    pub(in crate::native_surface) id: u64,
    pub(in crate::native_surface) committed: Rc<NativeMarkdownCommittedSelection>,
    pub(in crate::native_surface) phase: Cell<NativeMarkdownPendingSelectionPhase>,
    pub(in crate::native_surface) source: &'static str,
    sampling: Cell<bool>,
    revoked: Cell<bool>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    reads: Rc<Cell<u64>>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fault: Option<
        std::rc::Weak<super::super::probe::pending_fault::NativeMarkdownPendingSelectionFault>,
    >,
    ranges: [Option<NSRange>; 9],
}

pub(in crate::native_surface) type NativeMarkdownPendingSelectionSlot =
    RefCell<Option<Rc<NativeMarkdownPendingSelectionOwner>>>;

/// 只释放原 id 与同 Rc 的槽；局部旧 guard 不得 take 掉 Native 重入发布的后继 owner。
fn take_pending_selection_owner(
    slot: &NativeMarkdownPendingSelectionSlot,
    owner: &Rc<NativeMarkdownPendingSelectionOwner>,
) -> Option<Rc<NativeMarkdownPendingSelectionOwner>> {
    let mut slot = slot.borrow_mut();
    if slot
        .as_ref()
        .is_some_and(|actual| actual.id == owner.id && Rc::ptr_eq(actual, owner))
    {
        slot.take()
    } else {
        None
    }
}

impl NativeMarkdownPendingSelectionOwner {
    /// 安装尾声仍须拥有原槽的同一 Rc；同值 serial 或同一收据的另一个局部都不能替代它。
    pub(in crate::native_surface) fn owns_slot(
        self: &Rc<Self>,
        slot: &NativeMarkdownPendingSelectionSlot,
    ) -> bool {
        slot.borrow()
            .as_ref()
            .is_some_and(|actual| actual.id == self.id && Rc::ptr_eq(actual, self))
    }

    /// 每次拒绝都留下 tombstone；旧收据若已失效，不得重置真实后继输入的 Known。
    pub(in crate::native_surface) fn reject(&self) {
        self.revoked.set(true);
        self.committed.revoke_pending_selection();
    }

    pub(in crate::native_surface) fn current(&self) -> bool {
        !self.revoked.get() && self.committed.current()
    }

    pub(in crate::native_surface) fn native_step<T>(
        &self,
        work: impl FnOnce() -> T,
    ) -> Result<T, NativeMarkdownFoldError> {
        if !self.current() {
            self.reject();
            return Err(NativeMarkdownFoldError::Stale);
        }
        let result = work();
        if !self.current() {
            self.reject();
            return Err(NativeMarkdownFoldError::Stale);
        }
        Ok(result)
    }

    /// raw 值只与首通知前的固定坐标比较；旧 before 可超过新 EOF，不能拿它读取新正文。
    pub(in crate::native_surface) fn observe(
        &self,
        present: bool,
        count: usize,
        raw: NSRange,
    ) -> bool {
        if !self.current() || !present || count != 1 || self.source_hits(raw) == 0 {
            self.reject();
            return false;
        }
        true
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn record_read(&self) {
        if let Some(next) = self.reads.get().checked_add(1) {
            self.reads.set(next);
        }
    }

    pub(in crate::native_surface) fn begin_sample(
        self: &Rc<Self>,
    ) -> Option<NativeMarkdownPendingSelectionSampleGuard> {
        if !self.current() || self.sampling.replace(true) {
            self.reject(); // 不能递归读 getter，也不能让重入后返回 A 复活原收据。
            return None;
        }
        Some(NativeMarkdownPendingSelectionSampleGuard {
            owner: Rc::clone(self),
        })
    }

    pub(in crate::native_surface) fn source_hits(&self, raw: NSRange) -> u16 {
        let mut hits = 0_u16;
        for (index, range) in self.ranges.iter().enumerate() {
            if index == 7
                && !matches!(
                    self.phase.get(),
                    NativeMarkdownPendingSelectionPhase::EndEditing
                        | NativeMarkdownPendingSelectionPhase::SuperclassTail
                )
            {
                continue; // 原 EOF 只在结束通知及其连续安装尾声有效，不能授权更早的选择变化。
            }
            if index == 8
                && !matches!(
                    self.phase.get(),
                    NativeMarkdownPendingSelectionPhase::SuperclassTail
                )
            {
                continue; // delete-only EOF 只属于真实输入 super 的尾声，普通通知阶段不接受它。
            }
            if *range == Some(raw) {
                // 0..6 保持原来源；7=首 Native 前 EOF；8=原替换删除后、尚未插入新候选的 EOF。
                hits |= 1 << index;
            }
        }
        hits
    }
}

/// getter 或 pool drain 再入即 sticky 撤原权；局部 guard 只恢复 reading，不得清除 revoked。
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionSampleGuard {
    owner: Rc<NativeMarkdownPendingSelectionOwner>,
}
impl Drop for NativeMarkdownPendingSelectionSampleGuard {
    fn drop(&mut self) {
        self.owner.sampling.set(false);
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
pub(in crate::native_surface) fn native_markdown_pending_selection_trace_enabled() -> bool {
    std::env::var_os("OMNI_NATIVE_PROBE_PENDING_SELECTION_TRACE").is_some()
}

#[cfg(not(feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) fn native_markdown_pending_selection_trace_enabled() -> bool {
    false // 普通发布不读取 probe 环境变量，也不进入逐通知诊断日志。
}

impl PieceBackedMarkdownTextStorage {
    /// 纯提交后必定安装 owner；构造失权也保留 revoked tombstone，不静默退回普通 capture。
    pub(in crate::native_surface) fn begin_pending_selection(
        &self,
        committed: &Rc<NativeMarkdownCommittedSelection>,
        sources: NativeMarkdownPendingSelectionSources,
    ) -> Rc<NativeMarkdownPendingSelectionOwner> {
        if let Some(existing) = self.pending_selection_for(committed) {
            return existing;
        }
        // 精确新长度只依赖原提交票和 Weak 文档身份；兼容范围收据的未知 head 不能阻止测量。
        let new_len = self
            .document()
            .and_then(|document| committed.current_utf16_len_for(&document));
        let new_eof = new_len.map(|length| NSRange::new(length, 0));
        let original_eof_sources = new_len.and_then(|length| {
            native_markdown_pending_selection_eof_sources(
                committed.native_before_utf16_len(),
                length,
                sources,
            )
        });
        // 字符提交和纯 fold 发布均已完成，首 Native 通知/池 drain 尚未发生。
        // 方法内核同 generation/full selection/serial 后结束 Doc 借用，不从 raw 推导或夹取 EOF。
        let id = committed.selection_serial(); // 原票已有的有限 serial，不引入提交后可能失败的另一计数器。
        let selection = committed.selection;
        let after = NSRange::new(
            selection.anchor_utf16.min(selection.head_utf16),
            selection.anchor_utf16.abs_diff(selection.head_utf16),
        );
        let owner = Rc::new(NativeMarkdownPendingSelectionOwner {
            id,
            committed: Rc::clone(committed),
            phase: Cell::new(NativeMarkdownPendingSelectionPhase::Published),
            source: sources.source,
            sampling: Cell::new(false),
            revoked: Cell::new(new_eof.is_none() || original_eof_sources.is_none()),
            #[cfg(feature = "native-markdown-runtime-probe")]
            reads: Rc::clone(&self.ivars().pending_selection_reads),
            #[cfg(feature = "native-markdown-runtime-probe")]
            fault: self
                .ivars()
                .pending_selection_fault
                .borrow()
                .as_ref()
                .map(Rc::downgrade),
            ranges: [
                Some(committed.native_before_range()),
                Some(after),
                sources.replacement,
                sources.caret,
                sources.marked_before,
                sources.marked_after,
                new_eof,
                original_eof_sources.map(|(old_eof, _)| old_eof),
                original_eof_sources.and_then(|(_, deleted_eof)| deleted_eof),
            ],
        });
        let previous = self
            .ivars()
            .pending_selection
            .replace(Some(Rc::clone(&owner)));
        drop(previous); // slot 借用已结束；旧局部 guard 无权清理这个新 id。
        if owner.revoked.get() {
            owner.reject();
        }
        if native_markdown_pending_selection_trace_enabled() {
            eprintln!(
                "omni_pending event=begin id={id} source={} current={} ranges={:?}",
                owner.source,
                committed.current(),
                owner.ranges
            );
        }
        owner
    }

    pub(in crate::native_surface) fn pending_selection_for(
        &self,
        committed: &Rc<NativeMarkdownCommittedSelection>,
    ) -> Option<Rc<NativeMarkdownPendingSelectionOwner>> {
        self.ivars()
            .pending_selection
            .borrow()
            .as_ref()
            .filter(|owner| Rc::ptr_eq(&owner.committed, committed))
            .map(Rc::clone)
    }

    pub(in crate::native_surface) fn pending_selection_phase(
        &self,
        owner: &Rc<NativeMarkdownPendingSelectionOwner>,
        phase: NativeMarkdownPendingSelectionPhase,
    ) {
        if self
            .ivars()
            .pending_selection
            .borrow()
            .as_ref()
            .is_some_and(|current| current.id == owner.id && Rc::ptr_eq(current, owner))
        {
            owner.phase.set(phase); // 仅标注原同步阶段，不调用 Native，也不改变任何选择权限。
        }
    }

    pub(in crate::native_surface) fn pending_selection_phase_for(
        &self,
        committed: &Rc<NativeMarkdownCommittedSelection>,
        phase: NativeMarkdownPendingSelectionPhase,
    ) {
        if let Some(owner) = self.pending_selection_for(committed) {
            self.pending_selection_phase(&owner, phase);
        }
    }

    pub(in crate::native_surface) fn finish_pending_selection(
        &self,
        owner: &Rc<NativeMarkdownPendingSelectionOwner>,
    ) {
        let removed = take_pending_selection_owner(&self.ivars().pending_selection, owner);
        if removed.is_some() && native_markdown_pending_selection_trace_enabled() {
            eprintln!(
                "omni_pending event=end id={} source={} phase={:?} current={}",
                owner.id,
                owner.source,
                owner.phase.get(),
                owner.committed.current()
            );
        }
        drop(removed);
    }
}

/// canonical 共用通知路径持局部 guard；旧 guard 的 Drop 不能 take 后继 owner。
pub(in crate::native_surface) struct NativeMarkdownPendingSelectionGuard {
    pub(in crate::native_surface) slot: Rc<NativeMarkdownPendingSelectionSlot>,
    pub(in crate::native_surface) owner: Rc<NativeMarkdownPendingSelectionOwner>,
}
impl Drop for NativeMarkdownPendingSelectionGuard {
    fn drop(&mut self) {
        let removed = take_pending_selection_owner(&self.slot, &self.owner);
        if removed.is_some() && native_markdown_pending_selection_trace_enabled() {
            eprintln!(
                "omni_pending event=end id={} source={} phase={:?} current={}",
                self.owner.id,
                self.owner.source,
                self.owner.phase.get(),
                self.owner.current()
            );
        }
        drop(removed); // 结束槽借用后再释放原收据；不持有 storage/Native/Doc 强 owner。
    }
}

#[cfg(test)]
#[path = "pending_selection/tests.rs"]
mod tests;
