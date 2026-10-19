//! 一次真实输入回调的有限计划与结果；只把原 primitive 的同一提交票交回原调用栈。

use super::*;

// 与标准 workspace 的 32MiB 单会话追加上限一致；这里只限制本次输入副本，累积余量仍由 Core 核验。
pub(in crate::native_surface) const NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES: usize =
    32 * 1024 * 1024;

#[derive(Clone, Copy, Eq, PartialEq)]
enum InputPhase {
    Pending,
    Applying,
    Committed,
    Rejected,
}

/// 字符编辑与解除标记权限分离；unmark 不因拥有槽就获得修改正文的权利。
#[derive(Clone, Copy, Eq, PartialEq)]
enum InputPurpose {
    Edit,
    Unmark { original_marked: Option<NSRange> },
}

/// 只匹配实际观察到的 unmark 双回调协议；删除指令延迟，正文从不临时清空。
#[derive(Clone, Copy, Eq, PartialEq)]
enum UnmarkProtocol {
    AwaitClear,
    ClearDeferred,
    CommitInFlight,
    Complete,
}

/// Core 候选与同一次 observer 结果成对移动；不允许复制 token 或在 primitive 内重新准备。
pub(in crate::native_surface) struct NativeMarkdownPreparedInput {
    pub(in crate::native_surface) token: omni_markdown_core::MarkdownPreparedTextInput,
    pub(in crate::native_surface) folds: NativeMarkdownPreparedFolds,
}

/// 只保存已证明等值的本次 nested 参数；原文通过固定栈缓冲比较，不进入此载荷。
pub(in crate::native_surface) struct NativeMarkdownInputBookkeeping {
    pub(in crate::native_surface) id: u64,
    pub(in crate::native_surface) range: NSRange,
    pub(in crate::native_surface) text: String,
    pub(in crate::native_surface) consumed: Cell<bool>,
}

/// 载荷仅为当前新增候选，不包含原文副本；选择票和 marked Cell 均不持 Native 图。
pub(in crate::native_surface) struct NativeMarkdownInputTransaction {
    pub(in crate::native_surface) captured: NativeMarkdownCapturedSelection,
    pub(in crate::native_surface) epoch: u64,
    pub(in crate::native_surface) range: NSRange,
    pub(in crate::native_surface) text: String,
    pub(in crate::native_surface) after: MarkdownTextSelection,
    pub(in crate::native_surface) marked_after: Option<NSRange>,
    pub(in crate::native_surface) marked_cell: Rc<Cell<Option<NSRange>>>,
    purpose: InputPurpose,
    unmark_protocol: Cell<UnmarkProtocol>,
    pub(in crate::native_surface) prepared: RefCell<Option<NativeMarkdownPreparedInput>>,
    pub(in crate::native_surface) bookkeeping: RefCell<Option<Rc<NativeMarkdownInputBookkeeping>>>,
    next_bookkeeping_id: Cell<u64>,
    phase: Cell<InputPhase>,
    result: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>>,
    pending_owner: RefCell<Option<Rc<NativeMarkdownPendingSelectionOwner>>>,
    failure: RefCell<Option<String>>,
}

impl NativeMarkdownInputTransaction {
    pub(in crate::native_surface) fn new(
        captured: NativeMarkdownCapturedSelection,
        epoch: u64,
        range: NSRange,
        text: String,
        after: MarkdownTextSelection,
        marked_after: Option<NSRange>,
        marked_cell: Rc<Cell<Option<NSRange>>>,
    ) -> Self {
        Self {
            captured,
            epoch,
            range,
            text,
            after,
            marked_after,
            marked_cell,
            purpose: InputPurpose::Edit,
            unmark_protocol: Cell::new(UnmarkProtocol::AwaitClear),
            prepared: RefCell::new(None),
            bookkeeping: RefCell::new(None),
            next_bookkeeping_id: Cell::new(0),
            phase: Cell::new(InputPhase::Pending),
            result: RefCell::new(None),
            pending_owner: RefCell::new(None),
            failure: RefCell::new(None),
        }
    }

    pub(in crate::native_surface) fn unmark(
        captured: NativeMarkdownCapturedSelection,
        epoch: u64,
        marked_cell: Rc<Cell<Option<NSRange>>>,
    ) -> Self {
        let original_marked = marked_cell.get();
        let selection = captured.selection;
        let mut input = Self::new(
            captured,
            epoch,
            NSRange::new(NSNotFound as usize, 0),
            String::new(),
            selection,
            None,
            marked_cell,
        );
        input.purpose = InputPurpose::Unmark { original_marked };
        input
    }

    pub(in crate::native_surface) fn unmark_range(&self) -> Option<Option<NSRange>> {
        match self.purpose {
            InputPurpose::Edit => None,
            InputPurpose::Unmark { original_marked } => Some(original_marked),
        }
    }

    pub(in crate::native_surface) fn current(&self) -> bool {
        self.committed()
            .map_or_else(|| self.captured.current(), |committed| committed.current())
    }

    /// exact 空 setMarked 只记录清标记指令；没有 primitive 权限，也不改自有 marked。
    pub(in crate::native_surface) fn defer_unmark_clear(
        &self,
        range: NSRange,
    ) -> Result<(), String> {
        let end = range
            .location
            .checked_add(range.length)
            .ok_or("解除标记范围溢出")?;
        let selection = self.captured.selection;
        if self.unmark_range() != Some(Some(range))
            || range.length == 0
            || self.marked_cell.get() != Some(range)
            || !self.captured.current()
            || self.phase.get() != InputPhase::Pending
            || self.bookkeeping.borrow().is_some()
            || self.unmark_protocol.get() != UnmarkProtocol::AwaitClear
            || selection.anchor_utf16.min(selection.head_utf16) < range.location
            || selection.anchor_utf16.max(selection.head_utf16) > end
        {
            return Err("解除标记的空清除请求不符合原协议顺序或范围".into());
        }
        self.unmark_protocol.set(UnmarkProtocol::ClearDeferred);
        Ok(())
    }

    /// 等值 insert 的实际 primitive 和完整选择恢复都通过后才完成第二阶段。
    pub(in crate::native_surface) fn complete_unmark_bookkeeping(
        &self,
        child: &Rc<NativeMarkdownInputBookkeeping>,
    ) -> Result<(), String> {
        if self.unmark_protocol.get() != UnmarkProtocol::CommitInFlight
            || self.phase.get() != InputPhase::Pending
            || !self.captured.current()
            || !child.consumed.get()
            || !self
                .bookkeeping
                .borrow()
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, child))
        {
            return Err("解除标记的等值提交没有唯一成功终点".into());
        }
        self.unmark_protocol.set(UnmarkProtocol::Complete);
        Ok(())
    }

    pub(in crate::native_surface) fn unmark_is_complete(&self) -> bool {
        match self.unmark_range() {
            Some(Some(_)) => self.unmark_protocol.get() == UnmarkProtocol::Complete,
            // 原本没有 marked 的幂等 unmark 不需要伪造两次子调用。
            Some(None) => self.unmark_protocol.get() == UnmarkProtocol::AwaitClear,
            None => false,
        }
    }

    /// 所有子权限都沿原 parent；占用槽、重复 primitive 或失去原票时不覆盖旧 child。
    pub(in crate::native_surface) fn begin_bookkeeping(
        self: &Rc<Self>,
        range: NSRange,
        text: String,
    ) -> Result<NativeMarkdownInputBookkeepingGuard, String> {
        if self.unmark_range() != Some(Some(range))
            || !self.captured.current()
            || self.phase.get() != InputPhase::Pending
            || self.bookkeeping.borrow().is_some()
            || self.unmark_protocol.get() != UnmarkProtocol::ClearDeferred
        {
            return Err("解除标记的子调用没有原同步权限".into());
        }
        let id = self
            .next_bookkeeping_id
            .get()
            .checked_add(1)
            .ok_or("解除标记子调用代次耗尽")?;
        self.next_bookkeeping_id.set(id);
        self.unmark_protocol.set(UnmarkProtocol::CommitInFlight);
        let child = Rc::new(NativeMarkdownInputBookkeeping {
            id,
            range,
            text,
            consumed: Cell::new(false),
        });
        self.bookkeeping.replace(Some(Rc::clone(&child)));
        Ok(NativeMarkdownInputBookkeepingGuard {
            parent: Rc::clone(self),
            child,
        })
    }

    /// 一个计划只接受一个完全相同的 primitive；额外修改不能获得普通输入的回退权限。
    pub(in crate::native_surface) fn claim(&self, range: NSRange, text: &str) -> bool {
        if self.phase.get() != InputPhase::Pending
            || self.purpose != InputPurpose::Edit
            || self.range.location == NSNotFound as usize
            || self.range != range
            || self.text != text
            || !self.captured.current()
        {
            // unmark 计划以 NSNotFound 明确不授字符权限，连空 primitive 也不走 Core。
            self.reject("输入过程产生了未声明或已失效的文本修改");
            return false;
        }
        self.phase.set(InputPhase::Applying);
        true
    }

    /// Core 纯发布之后、任何池 drain 或 Native 通知之前接住唯一成功票并安装 marked 范围。
    pub(in crate::native_surface) fn publish(
        &self,
        committed: Rc<NativeMarkdownCommittedSelection>,
        owner: Rc<NativeMarkdownPendingSelectionOwner>,
    ) {
        self.marked_cell.set(self.marked_after);
        self.result.replace(Some(committed));
        self.pending_owner.replace(Some(owner)); // 原 owner Rc 跟随原输入 guard，不由清理时查询的新槽替代。
        self.phase.set(InputPhase::Committed);
    }

    pub(in crate::native_surface) fn reject(&self, error: &str) {
        if self.failure.borrow().is_none() {
            self.failure.replace(Some(error.to_owned()));
        }
        if self.phase.get() != InputPhase::Committed {
            // 已提交的第一笔不能因额外 primitive 被伪装成回滚；首个失败仍保留给原回调。
            self.phase.set(InputPhase::Rejected);
        }
    }

    pub(in crate::native_surface) fn committed(
        &self,
    ) -> Option<Rc<NativeMarkdownCommittedSelection>> {
        self.result.borrow().as_ref().map(Rc::clone)
    }

    pub(in crate::native_surface) fn failure(&self) -> Option<String> {
        self.failure.borrow().clone()
    }
}

/// 只清同 id 的子权限；较新的子调用与 parent 槽都不能被旧 guard 的 Drop 撤销。
pub(in crate::native_surface) struct NativeMarkdownInputBookkeepingGuard {
    parent: Rc<NativeMarkdownInputTransaction>,
    pub(in crate::native_surface) child: Rc<NativeMarkdownInputBookkeeping>,
}
impl Drop for NativeMarkdownInputBookkeepingGuard {
    fn drop(&mut self) {
        let removed = {
            let mut slot = self.parent.bookkeeping.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|child| child.id == self.child.id && Rc::ptr_eq(child, &self.child))
            {
                slot.take()
            } else {
                None
            }
        };
        drop(removed);
    }
}

/// view 只保存同一事务的 Weak backlink，使 superclass 的同步回调在首 Native 前找到原票。
pub(in crate::native_surface) struct NativeMarkdownInputCallbackGuard<'view> {
    pub(in crate::native_surface) view: &'view PieceBackedMarkdownTextView,
    pub(in crate::native_surface) input: std::rc::Weak<NativeMarkdownInputTransaction>,
}
impl Drop for NativeMarkdownInputCallbackGuard<'_> {
    fn drop(&mut self) {
        let mut slot = self.view.ivars().input_callback.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|current| std::rc::Weak::ptr_eq(current, &self.input))
        {
            slot.take(); // 只释放纯弱控制块，不恢复旧输入，也没有 Native 析构。
        }
    }
}

/// incoming 始终是本次系统传入的候选；类型查询、转换与池释放都受原票前后检查。
pub(in crate::native_surface) fn copy_native_markdown_input_text(
    string: &AnyObject,
    current: &impl Fn() -> bool,
) -> Result<String, String> {
    copy_native_markdown_input_text_with_limit(string, current, &|bytes| {
        bytes <= NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
    })
}

/// 延后输入只借原转换实现；先在同票下预留本次字节额度，才复制新增候选。
/// 预留回调只在已确定的字符串分支调用一次，失败由调用方归还本次预留；
/// 后置current可重入，故额度不能等本函数返回后才登记。
pub(in crate::native_surface) fn copy_native_markdown_input_text_with_limit(
    string: &AnyObject,
    current: &impl Fn() -> bool,
    reserve_bytes: &impl Fn(usize) -> bool,
) -> Result<String, String> {
    native_markdown_table_intent_step(current, || {
        autoreleasepool(|pool| {
            if let Some(value) = string.downcast_ref::<NSString>() {
                if !current() {
                    return Err("输入文本类型查询已撤权".to_owned());
                }
                // SAFETY: 借用在当前池内结束，只复制有上限的本次新增输入。
                let plain = unsafe { value.to_str(pool) };
                if !current() || !reserve_bytes(plain.len()) {
                    return Err("输入文本已失效或超过当前会话上限".to_owned());
                }
                return Ok(plain.to_owned());
            }
            if !current() {
                return Err("输入文本类型查询已撤权".to_owned());
            }
            if let Some(value) = string.downcast_ref::<NSAttributedString>() {
                if !current() {
                    return Err("输入文本类型查询已撤权".to_owned());
                }
                let plain = value.string();
                if !current() {
                    return Err("读取属性候选字符串时已撤权".to_owned());
                }
                // SAFETY: Native claim 与 UTF8 借用均在本池内结束，外层还会检查原权限。
                let plain = unsafe { plain.to_str(pool) };
                if !current() || !reserve_bytes(plain.len()) {
                    return Err("输入文本已失效或超过当前会话上限".to_owned());
                }
                return Ok(plain.to_owned());
            }
            Err("输入对象不是受支持的文本类型".to_owned())
        })
    })
    .map_err(|_| "读取候选时原输入票已失效")?
}

/// 固定缓冲比较真实 canonical 范围；既不复制正文，也不把删除非空范围当作无变化。
pub(in crate::native_surface) fn native_markdown_input_equals_source(
    document: &MarkdownDocumentBuffer,
    range: NSRange,
    text: &str,
) -> Result<bool, String> {
    let end = range
        .location
        .checked_add(range.length)
        .ok_or("输入等值范围溢出")?;
    document
        .byte_offset_for_utf16(range.location)
        .map_err(|error| error.to_string())?;
    document
        .byte_offset_for_utf16(end)
        .map_err(|error| error.to_string())?;
    if text.encode_utf16().count() != range.length {
        return Ok(false);
    }
    let mut incoming = text.encode_utf16();
    let mut offset = range.location;
    let mut buffer = [0_u16; 256];
    while offset < end {
        let count = (end - offset).min(buffer.len());
        document
            .copy_utf16_code_units(offset, &mut buffer[..count])
            .map_err(|error| error.to_string())?;
        for unit in &buffer[..count] {
            if incoming.next() != Some(*unit) {
                return Ok(false);
            }
        }
        offset += count; // 每轮只处理固定栈块，等值原文绝不形成可保留的 String。
    }
    Ok(incoming.next().is_none())
}

/// 仅显式合成诊断记录有限阶段；不输出候选、路径或任何正文，不改变默认产品输入行为。
#[cfg(feature = "native-markdown-runtime-probe")]
pub(in crate::native_surface) fn native_markdown_input_probe_stage(
    phase: &'static str,
    storage: &PieceBackedMarkdownTextStorage,
) {
    if std::env::var_os("OMNI_NATIVE_PROBE_INPUT_DIAGNOSTIC").is_none()
        && std::env::var_os("OMNI_NATIVE_PROBE_INPUT_CAPACITY_REJECTION").is_none()
    {
        return; // 普通产品与未请求输入诊断的验收不记录额外日志。
    }
    let (slot_phase, bytes, failure, committed) = {
        let slot = storage.ivars().input_transaction.borrow();
        slot.as_ref().map_or((0, 0, false, false), |input| {
            let phase = match input.phase.get() {
                InputPhase::Pending => 1,
                InputPhase::Applying => 2,
                InputPhase::Committed => 3,
                InputPhase::Rejected => 4,
            };
            (
                phase,
                input.text.len(),
                input.failure.borrow().is_some(),
                input.result.borrow().is_some(),
            )
        })
    };
    // 全部槽借用已结束；日志只记录原计划是否实际推进到 primitive 与 Core 终点。
    eprintln!(
        "omni_input phase={phase} epoch={} slot={slot_phase} bytes={bytes} failure={failure} committed={committed} composing={} process={} commits={} callbacks={} autosave={}",
        storage.ivars().input_transaction_epoch.get(),
        storage.ivars().composition_active.get(),
        storage.ivars().character_process_count.get(),
        storage.ivars().input_commit_count.get(),
        storage.ivars().input_callback_success_count.get(),
        storage.ivars().autosave_enqueue_count.get(),
    );
}

/// guard 只清自己的 Rc 槽；不保存旧 Pending，因此嵌套回调不能恢复陈旧计划。
pub(in crate::native_surface) struct NativeMarkdownInputTransactionGuard<'storage> {
    pub(in crate::native_surface) storage: &'storage PieceBackedMarkdownTextStorage,
    pub(in crate::native_surface) transaction: Rc<NativeMarkdownInputTransaction>,
}

impl Drop for NativeMarkdownInputTransactionGuard<'_> {
    fn drop(&mut self) {
        let owns_slot = self
            .storage
            .ivars()
            .input_transaction
            .borrow()
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, &self.transaction));
        if owns_slot {
            self.storage.cancel_input_preparation(&self.transaction);
            let owner = self
                .transaction
                .pending_owner
                .borrow()
                .as_ref()
                .map(Rc::clone);
            if let Some(owner) = owner {
                self.storage.finish_pending_selection(&owner);
            }
        } // 已失槽的旧 guard 不进行无 token 清理，更不能顺手排空后继取消候选。
        let removed = {
            let mut slot = self.storage.ivars().input_transaction.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &self.transaction))
            {
                slot.take()
            } else {
                None // 后继输入拥有的新槽不能由旧回调撤销。
            }
        };
        drop(removed); // incoming String 容量不跨槽借用释放，也不归还为闲置缓存。
    }
}
