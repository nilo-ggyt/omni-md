//! 词组删除、选词和随后普通输入沿同一提交链保序；每步只消费原Composer或有限新增载荷。

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputPermit;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedCellWordTarget as CellWordTarget,
    NativeMarkdownPagedWordAnchor as WordAnchor, NativeMarkdownPagedWordMotion as WordMotion,
    NativeMarkdownPagedWordSelection as WordSelection,
};
use objc2_foundation::NSRange;
use omni_markdown_core::{
    MarkdownEditingCommand, MarkdownLayoutTextAuthority, MarkdownTextSelection,
    MarkdownWordDeleteAnswer, MarkdownWordGroupAction, MarkdownWordGroupAnswer,
    MarkdownWordMoveScope,
};
use std::ops::Range;

mod folding;
pub(super) mod native;
use native::NativeInput;

/// 相邻同方向只保次数；普通输入槽预留真实到达次序，转换后移动唯一新增字符串。
enum WordInput {
    Delete {
        forward: bool,
        /// 普通删除与词组删除共用先后顺序，实际停止规则交原Composer判断。
        grapheme: bool,
        remaining: usize,
    },
    Move {
        motion: WordMotion,
        extend: bool,
        remaining: usize,
    },
    Visual {
        right: bool,
        extend: bool,
        remaining: usize,
    },
    Select {
        selection: WordSelection,
        /// 原真实 Down 的身份只用于回交首组；键盘选词没有此权限。
        gesture: Option<Weak<()>>,
        /// 当前唯一 Core 查询的真实起点与侧；最新 Drag 不重置尚未完成的首查询。
        query: (usize, bool, Option<CellWordTarget>),
        /// 已完成的首击组只保两个端点，后继目标串行复用同一个 Core 槽。
        first: Option<Range<usize>>,
        /// 首击位置只用于同组方向；最新目标变化不能改变它，也不会从这里重启组查询。
        origin_utf16: usize,
    },
    /// 原有文档首尾语义，不持旧长度、anchor或正文。
    Document {
        end: bool,
        extend: bool,
        remaining: usize,
    },
    Typing(Rc<RefCell<Option<String>>>),
    /// 尚未交给NSTextInputContext解释的原事件/菜单；不保存异步marked状态。
    Native(Rc<RefCell<Option<NativeInput>>>),
    /// 只保原语义命令；列表续行及缩进在前一步真实提交后才由原Core解释。
    Source {
        command: MarkdownEditingCommand,
        remaining: usize,
    },
}

/// 当前唯一实际事务：已转换新增文本、已认证删除范围或原Source命令，不借后继队列猜本步语义。
enum WordEdit<'a> {
    Typing(&'a str),
    Delete(NSRange),
    Source(MarkdownEditingCommand),
}

/// Capture facts only when this action reaches the front; later typing cannot inherit a literal old-Source head.
#[derive(Clone, Copy)]
pub(super) struct VisualIntent {
    pub selection: MarkdownTextSelection,
    pub upstream: bool,
    pub side: Option<super::super::paged_input::NativeMarkdownBidiCaretSide>,
    pub right: bool,
    pub extend: bool,
}
#[derive(Clone, Copy)]
pub(super) struct VisualSelection {
    pub selection: MarkdownTextSelection,
    pub upstream: Option<bool>,
    pub side: Option<super::super::paged_input::NativeMarkdownBidiCaretSide>,
}

/// 原链只持当前票、最后收据及有限未解释event；不缓存正文或第二份Input。
pub(super) struct PendingWordDelete {
    request: RefCell<NativeMarkdownPagedSceneRequest>,
    steps: RefCell<VecDeque<WordInput>>,
    queued_bytes: Cell<usize>,
    receipt: RefCell<Option<Rc<NativeMarkdownPagedInputCommit>>>,
    retired_serial: Cell<Option<u64>>,
    permit: RefCell<Option<NativeMarkdownPagedInputPermit>>,
    bound: RefCell<Option<(u64, MarkdownLayoutTextAuthority)>>,
    ready: RefCell<Option<Range<usize>>>,
    /// 只保原 Producer 已签的 opaque 端点，展开后原队首仍重算同一次动作。
    pub(super) fold_expansion: RefCell<Option<Range<usize>>>,
    /// 普通折叠业务拒绝等队列真实结束再显示，避免反馈重排撤销后续已接纳输入。
    fold_failure: RefCell<Option<ViewError>>,
    move_prepared: Cell<bool>,
    /// 原零级快路无法决定当前屏幕顺序时，仍由同一队首切到实际视觉查询。
    visual_word: Cell<bool>,
    pub(super) visual_seed: Cell<Option<VisualIntent>>,
    pub(super) visual_ready: Cell<Option<VisualSelection>>,
    /// 只在原队首Native解释栈内有效；回调派生的语义插在后继未解释事件之前。
    interpreting_native: Cell<bool>,
    native_insert_at: Cell<usize>,
    /// 有限业务首错等原链结束再显示；状态栏布局不能撤权仍在执行的后继输入。
    attachment_failure: RefCell<Option<ViewError>>,
    waiting_vertical: RefCell<Option<Rc<super::navigation::PendingVertical>>>,
}

impl PendingWordDelete {
    /// 发布后的唯一收据替换本步before；几何和原Doc/Cell身份仍继承同次请求。
    fn request(&self) -> NativeMarkdownPagedSceneRequest {
        let mut request = self.request.borrow().clone();
        if let Some(receipt) = self.receipt.borrow().as_ref() {
            request.input = receipt.state;
        }
        request
    }

    /// 只压缩相邻同方向，反向必须保留先后；溢出或有限段用尽都先拒绝，不丢已接纳动作。
    fn append_delete(&self, forward: bool, grapheme: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Delete {
                forward: last,
                grapheme: old_grapheme,
                remaining,
            }) = steps.back_mut()
            && *last == forward
            && *old_grapheme == grapheme
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy); // 与原垂直动作同一有限方向段边界；长按同方向不受次数32限制。
        }
        let step = WordInput::Delete {
            forward,
            grapheme,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }

    /// 同方向且相同扩选规则的长按只压次数；交错动作仍按原到达顺序保留。
    fn append_move(&self, motion: WordMotion, extend: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Move {
                motion: last,
                extend: extending,
                remaining,
            }) = steps.back_mut()
            && *last == motion
            && *extending == extend
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy);
        }
        let step = WordInput::Move {
            motion,
            extend,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }

    /// 端点动作只保方向及扩选语义；执行时再取真实长度/anchor，不保留到达时的旧坐标。
    fn append_document(&self, end: bool, extend: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Document {
                end: old_end,
                extend: old_extend,
                remaining,
            }) = steps.back_mut()
            && *old_end == end
            && *old_extend == extend
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy); // 原链保留已接纳动作，不用覆盖旧方向段为新输入腾槽。
        }
        let step = WordInput::Document {
            end,
            extend,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }

    /// 只读实际队首的端点语义，后继端点不得抢在当前输入之前解释。
    fn document_motion(&self) -> Option<(bool, bool)> {
        match self.steps.borrow().front()? {
            WordInput::Document { end, extend, .. } => Some((*end, *extend)),
            _ => None, // 只有实际队首端点动作能消费这一语义，后继输入不能抢先改选择。
        }
    }

    /// 原选择事务完成后才消费一个动作；文档端点无文字历史，但仍保原选择侧/serial合同。
    fn consume_document(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Document { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// 相邻同命令只压次数；不同缩进方向和Return必须保持原到达顺序，每步仍是独立事务。
    fn append_source(&self, command: MarkdownEditingCommand) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Source {
                command: old,
                remaining,
            }) = steps.back_mut()
            && *old == command
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy); // 新方向段先拒绝，已接纳的命令和字符均留在原链。
        }
        let step = WordInput::Source {
            command,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }

    fn source_command(&self) -> Option<MarkdownEditingCommand> {
        match self.steps.borrow().front()? {
            WordInput::Source { command, .. } => Some(*command),
            _ => None, // 其他步骤保持自己的原GCB、选择或新增载荷语义。
        }
    }

    /// 只消费Core确实提交或明确Unchanged的一次语义命令，不预消费后面的Return或缩进。
    fn consume_source(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Source { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// Share the Word/Typing queue; coalesce only adjacent motions with the same direction and extension rule.
    fn append_visual(&self, right: bool, extend: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Visual {
                right: old,
                extend: old_extend,
                remaining,
            }) = steps.back_mut()
            && *old == right
            && *old_extend == extend
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy);
        }
        let step = WordInput::Visual {
            right,
            extend,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }
    pub(super) fn visual_motion(&self) -> Option<(bool, bool)> {
        match self.steps.borrow().front()? {
            WordInput::Visual { right, extend, .. } => Some((*right, *extend)),
            WordInput::Move { motion, extend, .. }
                if !*extend && self.request().input.primary_range.length != 0 =>
            {
                motion.visual_right().map(|right| (right, false))
            } // 原非空视觉选择只收拢一次，复用真实 line.base_level；不启动分类或借逻辑端点猜侧。
            _ => None,
        }
    }
    /// 只在原队首已经选择真实视觉消费者后给出 Word 左右；不会影响逻辑前后或普通字符移动。
    pub(super) fn visual_word_motion(&self) -> Option<(bool, bool)> {
        if !self.visual_word.get() {
            return None;
        }
        self.moving()
            .and_then(|(motion, extend)| motion.visual_right().map(|right| (right, extend)))
    }
    fn consume_visual(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Visual { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    fn moving(&self) -> Option<(WordMotion, bool)> {
        match self.steps.borrow().front()? {
            WordInput::Move { motion, extend, .. } => Some((*motion, *extend)),
            _ => None, // 其他输入没有Word移动方向，不可借当前选择猜一个方向。
        }
    }

    /// 只消费已经发布或明确拒绝的一个系统动作，不能丢掉后面已接纳的输入。
    fn consume_move(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Move { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// 只有当前真实词组步骤可计算范围；输入槽另走原输入事务，不借Word近似文字行为。
    fn direction(&self) -> Option<bool> {
        match self.steps.borrow().front()? {
            WordInput::Delete { forward, .. } => Some(*forward),
            WordInput::Typing(_)
            | WordInput::Native(_)
            | WordInput::Document { .. }
            | WordInput::Source { .. }
            | WordInput::Select { .. }
            | WordInput::Move { .. }
            | WordInput::Visual { .. } => None,
        }
    }

    /// 本步是否只删一个完整簇；该事实属于已接纳动作，不能从后来的选择或布局猜测。
    fn deleting_grapheme(&self) -> bool {
        matches!(
            self.steps.borrow().front(),
            Some(WordInput::Delete { grapheme: true, .. })
        )
    }

    /// 正文仅在这类未改字符的查询期间保留同票Scene，供后继原指针继续真实命中。
    pub(super) fn selecting(&self) -> bool {
        matches!(self.steps.borrow().front(), Some(WordInput::Select { .. }))
    }

    /// 当前量子只推进已开始的真实查询；同手势更新目标不能让首组在快拖中反复重启。
    fn selection_query(&self) -> Option<(usize, bool, Option<CellWordTarget>)> {
        let steps = self.steps.borrow();
        match steps.front()? {
            WordInput::Select { query, .. } => Some(*query),
            _ => None,
        }
    }

    /// 首查询 Ready 才能保存首组；之后只查询最新目标组，不再从原 seed 重扩首击范围。
    /// 同一量子最多结束一个查询，切到新目标仍须由下一原调度量子推进。
    fn selection_target_query(
        &self,
        range: Range<usize>,
    ) -> Option<(usize, bool, Option<CellWordTarget>)> {
        let mut steps = self.steps.borrow_mut();
        let WordInput::Select {
            selection,
            query,
            first,
            ..
        } = steps.front_mut()?
        else {
            return None;
        };
        if first.is_none() && !matches!(selection.anchor, Some(WordAnchor::Selection(_))) {
            *first = Some(range);
        }
        // 目标在原查询期间可以继续改变；完成的是哪个位置只由 query 记录，不拿最新点冒认旧 Ready。
        // 首组一旦存在就保持至本次选择结束，反向拖动也只换目标查询，不重读原组。
        let latest = (
            selection.head_utf16,
            selection.backward_bias,
            selection.cell,
        );
        if *query != latest {
            *query = latest;
            return Some(latest);
        }
        None
    }

    /// 新点只替换同一实际 Down 的最新终点；已开始的首组查询和原 permit 保持唯一。
    /// 只有纯 Select 队首可这样合并，已经承诺的键盘/输入链仍由原顺序执行。
    fn replace_pointer_selection(
        &self,
        selection: WordSelection,
        gesture: &Option<Weak<()>>,
    ) -> bool {
        let mut steps = self.steps.borrow_mut();
        if steps.len() != 1 {
            return false;
        }
        let Some(WordInput::Select {
            selection: latest,
            gesture: original,
            ..
        }) = steps.front_mut()
        else {
            return false;
        };
        let same = match (original.as_ref(), gesture.as_ref()) {
            (Some(original), Some(gesture)) => Weak::ptr_eq(original, gesture),
            _ => false,
        };
        if !same {
            return false;
        }
        // 这里不改原输入许可、Source 绑定或正在推进的 Core 游标；取消仍由原 Body 唯一 owner 负责。
        // 新终点只是两个标量和原锚角色，不保存事件，也不把所有中间 Drag 排成无界动作。
        *latest = selection;
        drop(steps);
        self.ready.borrow_mut().take(); // 目标可能已改变，旧 Ready 不得提前提交后继位置。
        true
    }

    /// 只有这条唯一纯选词链才随鼠标结束或撤权；真实文字及已排队的其它输入仍须按原顺序完成。
    fn pointer_gesture(&self) -> Option<Weak<()>> {
        let steps = self.steps.borrow();
        if steps.len() != 1 {
            return None;
        }
        match steps.front() {
            Some(WordInput::Select { gesture, .. }) => gesture.clone(),
            _ => None,
        }
    }

    /// 真正首组与原手势一同离开队首；最终并集或 Shift 标量不冒充原首击词范围。
    fn pointer_group(&self) -> Option<(WordAnchor, Weak<()>)> {
        let steps = self.steps.borrow();
        let WordInput::Select {
            selection,
            first,
            gesture,
            origin_utf16,
            ..
        } = steps.front()?
        else {
            return None;
        };
        let anchor = match selection.anchor {
            Some(WordAnchor::Selection(anchor)) => WordAnchor::Selection(anchor),
            _ => {
                let group = first.as_ref()?;
                WordAnchor::Group {
                    from_utf16: group.start,
                    to_utf16: group.end,
                    origin_utf16: *origin_utf16,
                }
            }
        };
        Some((anchor, gesture.clone()?))
    }

    /// 同原CodeMirror rangeForClick/SelectionRange.extend：拖动保两端完整组，Shift保真实锚点。
    fn selection_target(
        &self,
        range: Range<usize>,
    ) -> std::result::Result<MarkdownTextSelection, ViewError> {
        if let Some((motion, extend)) = self.moving() {
            let forward = matches!(motion, WordMotion::Forward | WordMotion::Right);
            let request = self.request();
            let before = request
                .input
                .directed_selection()
                .ok_or(ViewError::Revoked)?;
            let from = before.anchor_utf16.min(before.head_utf16);
            let to = before.anchor_utf16.max(before.head_utf16);
            let head = if !extend && from != to {
                let end = if forward { to } else { from };
                if range != (end..end) {
                    return Err(ViewError::InvalidRange);
                }
                end // 原CM cursorByGroup非空只收拢到from/to；本次不得再越过一个组。
            } else {
                if (forward && range.start != before.head_utf16)
                    || (!forward && range.end != before.head_utf16)
                {
                    return Err(ViewError::InvalidRange);
                }
                if forward { range.end } else { range.start }
            };
            return Ok(MarkdownTextSelection {
                anchor_utf16: if extend { before.anchor_utf16 } else { head },
                head_utf16: head,
            }); // 扩选只动真实head，不从方向或范围猜/翻转原anchor。
        }
        let steps = self.steps.borrow();
        let Some(WordInput::Select {
            selection,
            first,
            origin_utf16,
            ..
        }) = steps.front()
        else {
            return Err(ViewError::Revoked);
        };
        let (anchor_utf16, head_utf16) = match selection.anchor {
            None => (range.start, range.end),
            Some(WordAnchor::Selection(anchor)) => {
                if range.start <= anchor && anchor <= range.end {
                    (range.start, range.end) // 原锚点在本组内部时改成完整组，不能留下半词。
                } else {
                    let head = if range.start.abs_diff(anchor) > range.end.abs_diff(anchor) {
                        range.start
                    } else {
                        range.end
                    };
                    (anchor, head) // 原锚点在组外时只把活动端伸到本组较远的一侧。
                }
            }
            Some(WordAnchor::Word { .. } | WordAnchor::Group { .. }) => {
                let first = first.as_ref().ok_or(ViewError::Revoked)?;
                if selection.head_utf16 == *origin_utf16 {
                    (range.start, range.end) // 原首击位置不合并首组；亲和改变时保留本次实际目标组。
                } else if range.start > first.start {
                    (first.start, first.end.max(range.end))
                } else {
                    (first.end.max(range.end), first.start.min(range.start))
                } // 同组非首击位置仍沿原 CM 反向合同，不重新扩首词。
            }
        };
        Ok(MarkdownTextSelection {
            anchor_utf16,
            head_utf16,
        })
    }

    fn consume_selection(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !matches!(steps.front(), Some(WordInput::Select { .. })) {
            return Err(ViewError::Revoked);
        }
        steps.pop_front(); // 只有本次选择已提交或真实无变化后，后续普通输入才能消费新选择。
        Ok(())
    }

    /// 当前步已经真实提交或证明端点无变化后才减少次数；不预消费尚未发生的删除。
    fn consume_word(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Delete { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// Native载荷getter尚在原同步栈内时，不允许原续体先越过预留输入槽。
    fn copying_input(&self) -> bool {
        self.steps.borrow().iter().any(|step| match step {
            WordInput::Typing(text) => text.borrow().is_none(),
            WordInput::Native(input) => input.borrow().is_none(),
            WordInput::Delete { .. }
            | WordInput::Document { .. }
            | WordInput::Source { .. }
            | WordInput::Select { .. }
            | WordInput::Move { .. }
            | WordInput::Visual { .. } => false,
        })
    }

    /// 取消先归还已经转换的全部新增载荷；尚在Native栈内的预留由它自己的失败出口归还。
    fn clear_steps(&self) {
        *self.fold_expansion.borrow_mut() = None;
        drop(self.fold_failure.borrow_mut().take());
        let attachment_failure = self.attachment_failure.borrow_mut().take();
        drop(attachment_failure); // 取消归还原错误文字，关闭的旧队列没有向后继窗口显示状态的权限。
        let steps = std::mem::take(&mut *self.steps.borrow_mut());
        let bytes: usize = steps
            .iter()
            .map(|step| match step {
                WordInput::Typing(text) => text.borrow().as_ref().map_or(0, String::len),
                WordInput::Native(input) => input.borrow().as_ref().map_or(0, NativeInput::bytes),
                WordInput::Delete { .. }
                | WordInput::Document { .. }
                | WordInput::Source { .. }
                | WordInput::Select { .. }
                | WordInput::Move { .. }
                | WordInput::Visual { .. } => 0,
            })
            .sum();
        drop(steps);
        self.queued_bytes.set(self.queued_bytes.get() - bytes);
    }

    /// selection_serial、marked、fold和Cell域均来自原InputState；相同正文不能给后继选择续权。
    fn current_at(&self, owner: &NativeMarkdownPagedBody, serial: u64) -> bool {
        let request = self.request();
        owner.current_body()
            && owner.serial.get() == serial
            && owner
                .pending_word_delete
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::ptr::eq(&**actual, self))
            && Weak::ptr_eq(&request.document, &owner.input.document_identity())
            && owner
                .input
                .target_domain()
                .is_ok_and(|target| target.same(&request.target))
            && owner
                .input
                .state()
                .is_ok_and(|state| state == request.input)
    }

    /// 未绑定时仅允许原输入继续等待唯一Producer；绑定后不能更换Source或serial重试。
    fn current(&self, owner: &NativeMarkdownPagedBody) -> bool {
        let serial = self.retired_serial.get().unwrap_or_else(|| {
            self.bound
                .borrow()
                .as_ref()
                .map_or(owner.serial.get(), |bound| bound.0)
        });
        self.current_at(owner, serial)
    }

    /// 原Native请求在每个实际重入边界仍须相同；清理自身只预声明原serial+1，不补签其他变化。
    fn current_native_at(&self, owner: &NativeMarkdownPagedBody, serial: u64) -> bool {
        self.current_at(owner, serial)
            && owner.view().is_ok_and(|view| {
                view.capture_scene_request().is_ok_and(|actual| {
                    if let Some(vertical) = self.waiting_vertical.borrow().as_ref() {
                        vertical.matches(owner, &actual)
                    } else {
                        actual.same(&self.request())
                    }
                })
            })
            && self.current_at(owner, serial)
    }
}

impl NativeMarkdownPagedBody {
    /// 关闭可取消纯显示和尚未开始复制的已捕获附件；未解释输入、文字及已开始事务仍有正文义务。
    /// 此处只读原队列，不提前取消；原 begin_close 在关闭真实获准后统一退休所有续体和资源。
    pub(in crate::native_surface) fn has_pending_canonical_input(&self) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| {
                pending.steps.borrow().iter().any(|step| match step {
                    WordInput::Move { .. }
                    | WordInput::Visual { .. }
                    | WordInput::Select { .. }
                    | WordInput::Document { .. } => false,
                    WordInput::Native(slot) => {
                        !matches!(
                            &*slot.borrow(),
                            Some(NativeInput::Fold(_) | NativeInput::Attachments { .. })
                        ) // None 表示 getter/解释/复制栈已占有载荷，仍须 Busy；不取消已开始的附件事务。
                    }
                    WordInput::Delete { .. } | WordInput::Typing(_) | WordInput::Source { .. } => {
                        true
                    }
                })
            })
    }

    /// 已有原输入链才接住入队前URL/批次拒绝；不为显示错误建立第二条工作链。
    pub(super) fn retain_attachment_rejection(&self, message: &str) -> bool {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return false;
        };
        if !pending.current_native_at(self, self.serial.get()) {
            return false;
        }
        let mut failure = pending.attachment_failure.borrow_mut();
        if failure.is_none() {
            *failure = Some(ViewError::AttachmentImport(message.to_owned()));
        } // 只有原业务首错暂留；Core/资源错误从未进入此槽，取消会同步归还本有限字符串。
        true
    }

    /// 明确选词/指针选择接管旧位置意图；原permit仍指同Input，不捕后继正文或补签旧命令。
    pub(super) fn accept_word_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        selection: WordSelection,
        gesture: Option<Weak<()>>,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        if self.preparing.get() {
            return Err(ViewError::Busy.into());
        }
        let current = |epoch| {
            self.current(epoch)
                && self.input.state().is_ok_and(|state| state == request.input)
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|actual| actual.same(request))
                })
        };
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if request.input.marked.is_some()
            || request.input.directed_selection().is_none()
            || selection.head_utf16 > request.input.utf16_len
            || selection.anchor.is_some_and(|anchor| match anchor {
                WordAnchor::Selection(anchor) => anchor > request.input.utf16_len,
                WordAnchor::Word { head_utf16, .. } => head_utf16 > request.input.utf16_len,
                WordAnchor::Group {
                    from_utf16,
                    to_utf16,
                    origin_utf16,
                } => {
                    from_utf16 > to_utf16
                        || to_utf16 > request.input.utf16_len
                        || origin_utf16 < from_utf16
                        || origin_utf16 > to_utf16
                }
            })
            || !self
                .input
                .target_domain()
                .map_err(ViewError::from)?
                .same(&request.target)
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
        {
            return Err(ViewError::InvalidRange.into());
        } // IME、多选主方向及跨客户坐标均不能被选词推断替代。
        self.refresh_word_delete(request)?;
        if self
            .pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|previous| {
                previous
                    .steps
                    .borrow()
                    .iter()
                    .any(|step| !matches!(step, WordInput::Select { .. }))
            })
        {
            // 旧Scene的点击端点不能自动转为后继文字Source坐标；接纳前明确拒绝，保已承诺字符链。
            if let Some(producer) = self.producer.borrow().as_ref() {
                producer.target_source_health()?;
            }
            return Err(ViewError::Busy.into());
        }
        let previous = self.pending_word_delete.borrow().clone();
        if let Some(previous) = previous
            && previous.request().same(request)
            && previous.replace_pointer_selection(selection, &gesture)
        {
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
            return self.schedule(); // 原同手势只更新最新点，不取消第一组或再捕一份 Source/Reader。
        }
        self.cancel_word_delete()?; // 只有真实新选择票已核准才撤旧词组；原Source首错仍不得降为指针覆盖。
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target();
        *self.pending_word_delete.borrow_mut() = Some(Rc::new(PendingWordDelete {
            request: RefCell::new(request.clone()),
            steps: RefCell::new(VecDeque::from([WordInput::Select {
                selection,
                gesture,
                // 未 Ready 的真实首击种子只启动一次；已知 Group 直接查询当前目标，不做反向重建。
                query: match selection.anchor {
                    Some(WordAnchor::Word {
                        head_utf16,
                        backward_bias,
                        cell,
                    }) => (head_utf16, backward_bias, cell),
                    _ => (
                        selection.head_utf16,
                        selection.backward_bias,
                        selection.cell,
                    ),
                },
                first: match selection.anchor {
                    Some(WordAnchor::Group {
                        from_utf16,
                        to_utf16,
                        ..
                    }) => Some(from_utf16..to_utf16),
                    _ => None,
                },
                origin_utf16: match selection.anchor {
                    Some(WordAnchor::Word { head_utf16, .. }) => head_utf16,
                    Some(WordAnchor::Group { origin_utf16, .. }) => origin_utf16,
                    _ => selection.head_utf16,
                },
            }])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            fold_expansion: RefCell::new(None),
            fold_failure: RefCell::new(None),
            move_prepared: Cell::new(false),
            visual_word: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
            interpreting_native: Cell::new(false),
            native_insert_at: Cell::new(0),
            attachment_failure: RefCell::new(None),
            waiting_vertical: RefCell::new(None),
        }));
        self.schedule()
    }

    /// 标准Word移动沿同一个已接纳输入链；全源UBA能力仅由原Producer的准备结果决定。
    pub(super) fn accept_word_move(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        motion: WordMotion,
        extend: bool,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.accept_queued_move(
            request,
            WordInput::Move {
                motion,
                extend,
                remaining: 1,
            },
            permit,
        )
    }

    /// Keep the original synchronous Ready interaction; only an incomplete query enters asynchronous continuation.
    pub(super) fn accept_visual_move(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        right: bool,
        extend: bool,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.refresh_word_delete(request)?;
        let queued = self.pending_word_delete.borrow().is_some();
        let key = self
            .scene
            .borrow()
            .as_ref()
            .map(|scene| scene.key().clone());
        let ready = !queued
            && key
                .as_ref()
                .is_some_and(|key| key.request.same(request) && self.scene_current(key));
        self.accept_queued_move(
            request,
            WordInput::Visual {
                right,
                extend,
                remaining: 1,
            },
            permit,
        )?;
        if !ready {
            return Ok(());
        } // Never bypass an admitted input or guess geometry before the first real Scene.
        let pending = self
            .pending_word_delete
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        let serial = self.serial.get();
        let result: Result<()> = (|| {
            let source = {
                let slot = self.producer.borrow();
                let Some(producer) = slot.as_ref() else {
                    return Ok(());
                };
                let key = key
                    .as_ref()
                    .expect("The original Ready key was captured before this bounded attempt");
                producer.target_source_health()?;
                if !producer.owns_target_source(key) {
                    return Ok(());
                }
                producer.target_source_authority(request, serial)?
            }; // Drop the Producer borrow before any input/current Native callback.
            *pending.bound.borrow_mut() = Some((serial, source.clone()));
            let seed_result = self.bind_visual_seed(&pending, request, serial, right, extend);
            {
                let slot = self.producer.borrow();
                if let Some(producer) = slot.as_ref() {
                    producer.target_source_health()?;
                }
            } // A Source error recorded by a callback must precede its ordinary permission failure.
            let seed = seed_result?;
            let step = {
                let mut slot = self.producer.borrow_mut();
                let producer = slot.as_mut().ok_or(ViewError::Revoked)?;
                if producer.target_source_authority(request, serial)? != source {
                    return Err(ViewError::Revoked.into());
                }
                producer.step_visual_intent(request, seed)?
            }; // The pure bounded query cannot reenter AppKit while the Producer slot is borrowed.
            if let super::producer::VisualAdvance::Ready(ready) = step {
                pending.visual_ready.set(Some(ready));
                self.finish_word_delete()?; // Publish through the same permit/receipt before the native command returns.
            }
            Ok(()) // Pending/NeedPrefix retain the same query and seed for the already scheduled original queue.
        })();
        self.word_input_result(&pending, serial, serial, result)
    }

    /// Read navigation facts from the actual front permit once, after prior character commits have really completed.
    fn bind_visual_seed(
        &self,
        pending: &PendingWordDelete,
        request: &NativeMarkdownPagedSceneRequest,
        serial: u64,
        right: bool,
        extend: bool,
    ) -> Result<VisualIntent> {
        if let Some(seed) = pending.visual_seed.get() {
            return Ok(seed);
        }
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let result: Result<VisualIntent> = (|| {
            let side = self
                .input
                .navigation_bidi_side(&permit, &current)
                .map_err(ViewError::from)?;
            let (upstream, _) = self
                .input
                .navigation_selection_facts(&permit, &current)
                .map_err(ViewError::from)?;
            let selection = request
                .input
                .directed_selection()
                .ok_or(ViewError::Revoked)?;
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
            Ok(VisualIntent {
                selection,
                upstream: upstream.unwrap_or(false),
                side,
                right,
                extend,
            })
        })();
        *pending.permit.borrow_mut() = Some(permit); // A failed/revoked getter cannot silently consume the queued permit.
        let seed = result?;
        pending.visual_seed.set(Some(seed));
        Ok(seed)
    }

    fn accept_queued_move(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        action: WordInput,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.refresh_word_delete(request)?;
        let previous = self.pending_word_delete.borrow().clone();
        if let Some(previous) = previous {
            if let Some(producer) = self.producer.borrow().as_ref() {
                producer.target_source_health()?;
            }
            let serial = self.serial.get();
            let current = |epoch| self.current(epoch) && previous.current_native_at(self, serial);
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
            match action {
                WordInput::Visual { right, extend, .. } => previous.append_visual(right, extend)?,
                WordInput::Move { motion, extend, .. } => previous.append_move(motion, extend)?,
                _ => unreachable!("原移动入口只创建实际 Visual/Move 动作"),
            }
            return self.schedule(); // 当前选择尚未提交时只登记方向/扩选，不提前猜未来head。
        }
        if self.preparing.get() {
            return Err(ViewError::Busy.into());
        }
        let current = |epoch| {
            self.current(epoch)
                && self.input.state().is_ok_and(|state| state == request.input)
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|actual| actual.same(request))
                })
        };
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if request.input.marked.is_some()
            || request.input.selection_range_count != 1
            || request.input.directed_selection().is_none()
            || !self
                .input
                .target_domain()
                .map_err(ViewError::from)?
                .same(&request.target)
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
        {
            return Err(ViewError::InvalidRange.into());
        } // 多选/marked/外来Source不能借词组命令推测方向。
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target();
        *self.pending_word_delete.borrow_mut() = Some(Rc::new(PendingWordDelete {
            request: RefCell::new(request.clone()),
            steps: RefCell::new(VecDeque::from([action])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            fold_expansion: RefCell::new(None),
            fold_failure: RefCell::new(None),
            move_prepared: Cell::new(false),
            visual_word: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
            interpreting_native: Cell::new(false),
            native_insert_at: Cell::new(0),
            attachment_failure: RefCell::new(None),
            waiting_vertical: RefCell::new(None),
        }));
        self.schedule()
    }

    /// 单选择的真实系统动作携原permit保序；无在途链的非空选择直接删除，多范围仍由View处理。
    pub(super) fn accept_word_delete(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        forward: bool,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.refresh_word_delete(request)?;
        let previous = self.pending_word_delete.borrow().clone();
        if let Some(previous) = previous {
            if let Some(producer) = self.producer.borrow().as_ref() {
                producer.target_source_health()?; // 追加也不能掩盖原Source已发生的读取首错。
            }
            let serial = self.serial.get();
            let current = |epoch| self.current(epoch) && previous.current_native_at(self, serial);
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
            previous.append_delete(forward, false)?;
            return self.schedule(); // 只在原链末尾登记，不在Native重入栈提交或拿这张before执行未来一步。
        }
        if self.preparing.get() {
            return Err(ViewError::Busy.into()); // 没有原链的准备栈仍不能另起一条同步编辑。
        }
        let current = |epoch| {
            self.current(epoch)
                && self.input.state().is_ok_and(|state| state == request.input)
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|actual| actual.same(request))
                })
        };
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if request.input.marked.is_some()
            || request.input.selection_range_count != 1
            || request.input.directed_selection().is_none()
            || !self
                .input
                .target_domain()
                .map_err(ViewError::from)?
                .same(&request.target)
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
        {
            return Err(ViewError::InvalidRange.into()); // 委托边界只接受原Body/Cell的一个真实未marked选择。
        }
        if request.input.primary_range.length != 0 {
            // 没有原异步链时，明确的单一非空选择仍在本次调用直接删除；在途Select的后续
            // Word动作已在上面按序排入，不能拿选词提交前的旧非空范围提前删除。
            let view = self.view()?;
            let outcome = self
                .input
                .delete_selected_ranges(&permit, &current)
                .map_err(ViewError::from)?;
            return match outcome {
                super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                    view.finish_word_delete_commit(&commit).map_err(Into::into)
                }
                super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => Ok(()),
            };
        }
        // 后来的字符动作接管原选择意图；只撤原显示目标，不先改canonical或启动新的读取。
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target();
        let pending = Rc::new(PendingWordDelete {
            request: RefCell::new(request.clone()),
            steps: RefCell::new(VecDeque::from([WordInput::Delete {
                forward,
                grapheme: false,
                remaining: 1,
            }])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            fold_expansion: RefCell::new(None),
            fold_failure: RefCell::new(None),
            move_prepared: Cell::new(false),
            visual_word: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
            interpreting_native: Cell::new(false),
            native_insert_at: Cell::new(0),
            attachment_failure: RefCell::new(None),
            waiting_vertical: RefCell::new(None),
        });
        *self.pending_word_delete.borrow_mut() = Some(pending);
        self.schedule()
    }

    /// 文档首尾接在原输入链后执行；没有pending时仍沿既有直接端点交互，不创建新管线。
    pub(super) fn accept_document_motion(
        &self,
        end: bool,
        extend: bool,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        if self.input.is_cell() {
            return Ok(false); // 本轮只保Body文档端点顺序；Cell仍由原局部选择及客户交互直接解释。
        }
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        if pending.interpreting_native.get() {
            return Ok(false); // 这次原key的同步回调已到真实caret，不能再排到尚未解释的后继事件后面。
        }
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        } // 已有Source真实失败仍优先，不能被后继绝对导航洗成普通取消。
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(permit, &current)
            .map_err(ViewError::from)?;
        pending.append_document(end, extend)?;
        self.schedule()?;
        Ok(true)
    }

    /// 只接Body已有的Return/Tab/Backtab；Cell的行列导航仍交原客户控制器，不伪装成正文输入。
    pub(super) fn accept_source_command(
        &self,
        command: MarkdownEditingCommand,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        if self.input.is_cell() || command == MarkdownEditingCommand::DeleteMarkupBackward {
            return Ok(false); // 退格已由真实GCB队列负责，Cell绝不能拿canonical缩进算法改局部草稿。
        }
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false); // 普通就绪输入仍同步执行；不为延后而另建输入链或改变原交互。
        };
        if pending.interpreting_native.get() {
            return Ok(false); // 这次原key的同步回调已到真实caret，不能再排到尚未解释的后继事件后面。
        }
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        } // 原Source已发生的真实错误不能被队列额度或后继输入覆盖。
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(permit, &current)
            .map_err(ViewError::from)?;
        pending.append_source(command)?;
        self.schedule()?;
        Ok(true)
    }

    /// 真实移动尚未提交时，将随后退格/前删排入原链；无在途链仍走既有同步交互。
    pub(super) fn accept_grapheme_delete(
        &self,
        forward: bool,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false); // 不为普通已就绪删除制造异步步骤，保留原标记处理及输入法路径。
        };

        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // 不能借队列接纳覆盖旧源已经发生的真实读取错误。
        }
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(permit, &current)
            .map_err(ViewError::from)?;
        pending.append_delete(forward, true)?; // 仅登记动作；范围等前面真实收据完成后再取原GCB。
        self.schedule()?;
        Ok(true)
    }

    /// 普通NSNotFound输入先占真实次序，再转换唯一新增载荷；没有Word链时保持原直接输入路径。
    pub(super) fn accept_word_typing(
        &self,
        text: &objc2::runtime::AnyObject,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false); // 无pending不多发Native getter，也不改变显式大输入或Frozen准入。
        };
        if pending.interpreting_native.get() {
            return Ok(false); // 这次原key的同步回调已到真实caret，不能再排到尚未解释的后继事件后面。
        }
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // 队列额度拒绝也不能覆盖原Source已记录的读取首错。
        }
        let serial = self.serial.get();
        let payload = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() >= 32 {
                return Err(ViewError::Busy.into()); // 先拒绝新增槽；此前已接纳方向和字符均保留。
            }
            steps.push_back(WordInput::Typing(Rc::clone(&payload)));
        } // 占位发生在首个Native读取前，嵌套输入只能排在本次输入之后。
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let charged = Cell::new(0);
        let converted = self
            .input
            .copy_permit_text_with_limit(permit, text, &current, &|bytes| {
                let remaining =
                    super::super::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
                        .saturating_sub(pending.queued_bytes.get());
                if bytes > remaining {
                    return false; // 在真正复制前拒绝，不先分配第二份大载荷再检查总量。
                }
                pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
                charged.set(bytes);
                true
            });
        match converted {
            Ok(text) => {
                // 转换内部已经在最后getter后核原票；下面只有Rust移动，不再获取后继状态续权。
                *payload.borrow_mut() = Some(text);
            }
            Err(cause) => {
                pending
                    .queued_bytes
                    .set(pending.queued_bytes.get() - charged.get());
                pending.steps.borrow_mut().retain(|step| {
                    !matches!(step, WordInput::Typing(actual) if Rc::ptr_eq(actual, &payload))
                }); // 只移除原转换槽，绝不撤掉较晚成功排入的字符或后继队列。
                if pending.current(self) {
                    self.schedule()?;
                }
                return Err(ViewError::from(cause).into());
            }
        }
        self.schedule()?; // 转换中被原advance暂停的续体只由这次真实完成恢复。
        Ok(true)
    }

    /// 只有同一条链自己的真实Commit通知可越过取消；相同state或后继新收据都不能冒充。
    pub(super) fn owns_word_commit(&self, commit: &NativeMarkdownPagedInputCommit) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| {
                pending
                    .receipt
                    .borrow()
                    .as_ref()
                    .is_some_and(|receipt| std::ptr::eq(&**receipt, commit))
            })
    }

    /// 载荷getter尚未返回时沿原deferred暂停；不另建timer或在槽里保Native对象。
    pub(super) fn word_input_is_copying(&self) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.copying_input())
    }

    /// 新增文本、Source命令和文档端点只消费前步真实receipt；每回合只执行一笔原事务。
    pub(super) fn advance_word_typing(&self) -> Result<bool> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        if pending.waiting_vertical.borrow().is_some() {
            return Ok(false); // 原垂直动作尚无最终收据，不能解释后继输入法事件。
        }
        if pending.native_front() {
            let scene = self.scene.borrow().clone();
            let ready = scene.as_ref().is_some_and(|scene| {
                scene.key().request.same(&pending.request()) && self.scene_current(scene.key())
            });
            drop(scene); // Native解释前归还临时Scene强claim，不持Reader跨输入回调。
            if !ready {
                return Ok(false);
            } // 原event可能是垂直/行端点；只等真实Scene，不先解释失败再重放IME。
            return self.advance_native_input(&pending);
        }
        if let Some((end, extend)) = pending.document_motion() {
            let serial = self.serial.get();
            let result = self.commit_document_motion(&pending, end, extend, serial);
            self.word_input_result(&pending, serial, serial, result)?;
            return Ok(true); // 本回合只完成这次真实选择；不读取EOF正文或提前执行后继键入。
        }
        if let Some(command) = pending.source_command() {
            let serial = self.serial.get();
            let released_serial = serial
                .checked_add(1)
                .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
            let result = self.commit_word_input(
                &pending,
                WordEdit::Source(command),
                serial,
                released_serial,
            );
            self.word_input_result(&pending, serial, released_serial, result)?;
            return Ok(true); // 这一回合只有原Core的一次语义事务，不顺便执行后继输入或开始排版。
        }
        let (payload, serial, released_serial) = {
            let mut steps = pending.steps.borrow_mut();
            if !matches!(steps.front(), Some(WordInput::Typing(_))) {
                return Ok(false);
            }
            let serial = self.serial.get();
            let released_serial = serial
                .checked_add(1)
                .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
            match steps.pop_front() {
                Some(WordInput::Typing(payload)) => (payload, serial, released_serial),
                _ => unreachable!("已在同一无重入借用中确认普通输入槽"),
            }
        };
        let text = payload.borrow_mut().take().ok_or(ViewError::Busy)?;
        let result =
            self.commit_word_input(&pending, WordEdit::Typing(&text), serial, released_serial);
        let bytes = text.len();
        drop(text); // 真实新增副本退出后才归还累计额度，Native重入期间不能重复消费它。
        pending.queued_bytes.set(pending.queued_bytes.get() - bytes);
        self.word_input_result(&pending, serial, released_serial, result)?;
        Ok(true)
    }

    /// 编辑、选择、滚动或模式几何改变即撤原Word；取消也先保原Source已经发生的实际错误。
    pub(super) fn refresh_word_delete(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self.pending_word_delete.borrow().clone();
        if pending.as_ref().is_some_and(|pending| {
            !pending.current(self)
                || if let Some(vertical) = pending.waiting_vertical.borrow().as_ref() {
                    !vertical.matches(self, request)
                } else {
                    !pending.request().same(request)
                }
        }) {
            let gesture = pending
                .as_ref()
                .and_then(|pending| pending.pointer_gesture());
            self.cancel_word_delete()?;
            if let Some(gesture) = gesture {
                // 原查询真实归还后才能撤其待组事实；Source 失败先返回，不把失败当成功的空选择。
                // 清理只认这一个手势，不能撤 Native 重入已安装的后继 Down 或真实 Reopen 来源。
                self.cancel_waiting_pointer_word(&gesture);
            }
        }
        Ok(())
    }

    /// 原 Up 可先于首词组完成；唯一指针槽据此保留结束事实，新 Down 才能撤掉这次查询。
    /// 只识别同一弱身份的纯选词；带文字的已接纳输入链仍有自己的提交义务，不能随手势清除。
    pub(super) fn has_pending_pointer_word(&self, intent: &Weak<()>) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .and_then(|pending| pending.pointer_gesture())
            .is_some_and(|original| Weak::ptr_eq(&original, intent))
    }

    /// 指针续体取消仅命中这一个原Weak的纯选词；输入链中的真实文字事务从不被清掉。
    pub(super) fn cancel_pointer_word(&self, intent: &Weak<()>) -> Result<()> {
        if self.has_pending_pointer_word(intent) {
            self.cancel_word_delete()?;
        }
        Ok(())
    }

    /// 原槽先取走再归还Core续体和permit；不持RefCell借用跨其资源析构。
    pub(super) fn cancel_word_delete(&self) -> Result<()> {
        let pending = self.pending_word_delete.borrow_mut().take();
        let Some(pending) = pending else {
            return Ok(()); // 没有原动作，不碰后继Producer或领取Source。
        };
        let bound_serial = pending.bound.borrow().as_ref().map(|bound| bound.0);
        let request = pending.request();
        let health = {
            let mut slot = self.producer.borrow_mut();
            if let Some(producer) = slot.as_mut() {
                let health = match producer.replaced_cell_source(&self.input, &request) {
                    Ok(true) => Ok(()), // 原Cell已真实编辑，仅这类旧草稿Source失效属于正常退休。
                    Ok(false) => producer.target_source_health(),
                    Err(cause) => Err(cause), // Body与Cell的EIO等实际错误均保留，不能降成取消。
                };
                if bound_serial == Some(self.serial.get()) {
                    producer.cancel_word_delete(); // 只取消同一个已绑定Producer，不影响后继同generation工作。
                    producer.cancel_word_group(); // 同一个槽也可能正扩选原词；没有第二个游标或读取owner。
                    producer.cancel_visual();
                }
                health
            } else {
                Ok(()) // 唯一Producer在原准备栈或已真实归还；本次取消不创建替代读取。
            }
        };
        pending.clear_steps(); // 原槽已经取走；局部观察Rc不能拖住已取消的新增输入字符串。
        drop(pending);
        health
    }

    /// 唯一Producer选定后只绑定一次Source；每回合仅一个Core量子，未Ready不提交删除。
    pub(super) fn advance_word_delete(
        &self,
        producer: &mut Producer,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self
            .pending_word_delete
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        producer.target_source_health()?;
        if !pending.current(self) || !pending.request().same(request) {
            return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
        }
        let serial = self.serial.get();
        let source = producer.target_source_authority(request, serial)?;
        let selecting = pending.selecting();
        let bound = pending.bound.borrow().clone();
        match bound {
            None => {
                if let Some((head, backward_bias, cell)) = pending.selection_query() {
                    producer.begin_word_group(
                        MarkdownWordGroupAction::Select { backward_bias },
                        head,
                        cell,
                    )?;
                } else if pending.moving().is_none()
                    && pending.visual_motion().is_none()
                    && request.input.primary_range.length == 0
                {
                    let forward = pending.direction().ok_or(ViewError::Revoked)?;
                    if pending.deleting_grapheme() {
                        producer
                            .begin_grapheme_delete(forward, request.input.primary_range.location)?;
                    } else {
                        producer
                            .begin_word_delete(forward, request.input.primary_range.location)?;
                    } // 两类真实删除只改变停止条件，读取预算、Source身份与退休边界保持同一条管线。
                } // 选词后的删除消费那次真实完整选择，不从其首端再扩词；Source仍按同一绑定核健康。
                *pending.bound.borrow_mut() = Some((serial, source));
            }
            Some((original_serial, original_source)) => {
                if original_serial != serial || original_source != source {
                    return Err(ViewError::Revoked.into()); // 不能用后继Producer重新计算同一次旧删除。
                }
            }
        }
        if let Some((right, extend)) = pending
            .visual_motion()
            .or_else(|| pending.visual_word_motion())
        {
            self.bind_visual_seed(&pending, request, serial, right, extend)?;
            return producer.advance(self, request); // Original font/line work and bounded visual queries share turns; do not scroll before commit.
        }
        if let Some((motion, extend)) = pending.moving() {
            let forward = matches!(motion, WordMotion::Forward | WordMotion::Right);
            if !pending.move_prepared.get() {
                if let Some(forward) = motion.logical_forward() {
                    pending.move_prepared.set(true);
                    if !extend && request.input.primary_range.length != 0 {
                        let selection = request
                            .input
                            .directed_selection()
                            .ok_or(ViewError::Revoked)?;
                        let head = if forward {
                            selection.anchor_utf16.max(selection.head_utf16)
                        } else {
                            selection.anchor_utf16.min(selection.head_utf16)
                        };
                        *pending.ready.borrow_mut() = Some(head..head);
                        return Ok(());
                    } // 逻辑收拢保原 ordered from/to；不要求视觉层级，也不借 directed head 替代原契约。
                    producer.begin_word_group(
                        if forward {
                            MarkdownWordGroupAction::Forward
                        } else {
                            MarkdownWordGroupAction::Backward
                        },
                        request.input.selection.head_utf16,
                        None,
                    )?;
                    return self.schedule();
                } // Forward/Backward 的真实 Source 组边界不需要整源零级；RTL 也保原逻辑含义。
                let proved = producer.prepare_word_move(self, request)?;
                if !pending.current(self) {
                    return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
                }
                match proved {
                    None => return self.schedule(), // 本回合只有原Bidi量子，不再启动/推进GCB。
                    Some(MarkdownWordMoveScope::Unsupported) => {
                        pending.visual_word.set(true);
                        return self.schedule(); // 完整非零 UBA 仍有真实视觉顺序；下一回合用原 Parts/GCB 查询，不借逻辑组替代。
                    }
                    Some(
                        MarkdownWordMoveScope::WholeUniformZero
                        | MarkdownWordMoveScope::VisibleParagraphScopes,
                    ) => {
                        pending.move_prepared.set(true);
                    }
                }
                producer.begin_visual_word_group(forward, request.input.selection.head_utf16)?;
                return self.schedule(); // 原Bidi这次可能刚读完，GCB只能在下一原量子推进。
            }
            let answer = producer.step_word_group(self, request)?;
            if !pending.current(self) {
                return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
            }
            return match answer {
                MarkdownWordGroupAnswer::Yielded => self.schedule(),
                MarkdownWordGroupAnswer::UnsupportedVisualOrder => {
                    producer.cancel_word_group();
                    producer.target_source_health()?;
                    pending.visual_word.set(true);
                    self.schedule() // 原两槽布局上下文已经恢复；只切本步消费者，不重建 Source 或丢后继输入。
                }
                MarkdownWordGroupAnswer::NeedsFoldExpansion(hidden) => {
                    *pending.fold_expansion.borrow_mut() = Some(hidden);
                    Ok(()) // 原 step 工作已记入 Producer；归槽后才退休与展开，不重入栈内 composer。
                }
                MarkdownWordGroupAnswer::Ready(range) => {
                    *pending.ready.borrow_mut() = Some(range);
                    Ok(())
                }
            };
        }
        if !selecting && request.input.primary_range.length != 0 {
            let range = request.input.primary_range;
            let end = range
                .location
                .checked_add(range.length)
                .filter(|end| *end <= request.input.utf16_len)
                .ok_or(ViewError::InvalidRange)?;
            *pending.ready.borrow_mut() = Some(range.location..end);
            return Ok(()); // 队列已由前次Commit授予这个真实单选择；无需扫描或另建Core删除游标。
        }
        if selecting {
            let answer = producer.step_word_group(self, request)?;
            if !pending.current(self) {
                return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
            }
            return match answer {
                MarkdownWordGroupAnswer::Yielded => self.schedule(),
                MarkdownWordGroupAnswer::UnsupportedVisualOrder => {
                    Err(ViewError::UnsupportedCommand.into())
                } // 逻辑 Select 从不请求视觉证书，不能把错误消费者当一个选词结果。
                MarkdownWordGroupAnswer::NeedsFoldExpansion(hidden) => {
                    *pending.fold_expansion.borrow_mut() = Some(hidden);
                    Ok(()) // 原 step 工作已记入 Producer；归槽后才退休与展开，不重入栈内 composer。
                }
                MarkdownWordGroupAnswer::Ready(range) => {
                    if let Some((head, backward_bias, cell)) =
                        pending.selection_target_query(range.clone())
                    {
                        producer.cancel_word_group();
                        producer.begin_word_group(
                            MarkdownWordGroupAction::Select { backward_bias },
                            head,
                            cell,
                        )?;
                        self.schedule() // 下一回合只查最新目标；已 Ready 的首组不会再次扩展。
                    } else {
                        *pending.ready.borrow_mut() = Some(range);
                        Ok(()) // 两个原范围已齐，提交阶段再按原anchor合并，不改用户正文或历史。
                    }
                }
            };
        }
        let answer = producer.step_word_delete(self, request)?;
        // 原PreparationTicket已核同次Native请求及Source；这里只补原pending槽的纯所有权，不重复整组getter。
        if !pending.current(self) {
            return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
        }
        match answer {
            MarkdownWordDeleteAnswer::NeedsFoldExpansion(hidden) => {
                *pending.fold_expansion.borrow_mut() = Some(hidden);
                Ok(()) // 不把 opaque 可见端点发布成删除范围，原队首等待真实展开后重算。
            }
            MarkdownWordDeleteAnswer::Yielded => self.schedule(), // 原同一续体继续，不开timer或独立worker。
            MarkdownWordDeleteAnswer::Ready(range) => {
                *pending.ready.borrow_mut() = Some(range); // exact范围只活到同一原permit的唯一最终提交。
                Ok(())
            }
        }
    }

    /// Ready先归还旧读取，后消费原permit；没有选择修改或部分删除可被误记为一次成功。
    pub(super) fn finish_word_delete(&self) -> Result<()> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        let folded = pending.fold_expansion.borrow_mut().take();
        if let Some(hidden) = folded {
            let serial = pending
                .bound
                .borrow()
                .as_ref()
                .map(|bound| bound.0)
                .ok_or(ViewError::Revoked)?;
            let released = serial
                .checked_add(1)
                .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
            let result = self.commit_fold_expansion(&pending, hidden, serial, released);
            return self.word_input_result(&pending, serial, released, result);
        }
        if let Some(ready) = pending.visual_ready.take() {
            let serial = pending
                .bound
                .borrow()
                .as_ref()
                .map(|bound| bound.0)
                .ok_or(ViewError::Revoked)?;
            let result = self.commit_visual_selection(&pending, ready, serial);
            return self.word_input_result(&pending, serial, serial, result);
        }
        let ready = pending.ready.borrow_mut().take();
        let Some(range) = ready else {
            return Ok(());
        }; // Yielded的正常后继已经由原schedule登记。
        let serial = pending
            .bound
            .borrow()
            .as_ref()
            .map(|bound| bound.0)
            .ok_or(ViewError::Revoked)?;
        if !pending.current_native_at(self, serial) {
            self.cancel_original_word_delete(&pending)?;
            return self.schedule(); // 原输入已撤权；只能显示真实后继，不能提交这张旧票。
        }
        if pending.selecting() || pending.moving().is_some() {
            let result = self.commit_word_selection(&pending, range, serial);
            return self.word_input_result(&pending, serial, serial, result);
        }
        let forward = pending.direction().ok_or(ViewError::Revoked)?;
        let before_range = pending.request().input.primary_range;
        let head = before_range.location;
        if (before_range.length == 0
            && ((forward && range.start != head) || (!forward && range.end != head)))
            || (before_range.length != 0
                && (range.start != head
                    || Some(range.end) != head.checked_add(before_range.length)))
        {
            return Err(ViewError::InvalidRange.into()); // 不可逆编辑必须守住该方向的原caret端点。
        }
        if range.is_empty() {
            pending.consume_word()?;
            if let Some(producer) = self.producer.borrow_mut().as_mut() {
                producer.cancel_word_delete(); // 真端点只结束这次Core查询，不丢后续反向或普通输入。
            }
            let result = self.arm_word_successor(&pending, None);
            return self.word_input_result(&pending, serial, serial, result); // 无变化不制造Commit或历史。
        }
        let released_serial = serial
            .checked_add(1)
            .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
        let result = self.commit_word_input(
            &pending,
            WordEdit::Delete(NSRange::new(range.start, range.len())),
            serial,
            released_serial,
        );
        self.word_input_result(&pending, serial, released_serial, result)
    }

    /// Publish the actual side and selection together; side-only motion consumes intent without a text Undo record.
    fn commit_visual_selection(
        &self,
        pending: &Rc<PendingWordDelete>,
        ready: VisualSelection,
        serial: u64,
    ) -> Result<()> {
        let view = self.view()?;
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let permission = self.input.check_permit(&permit, &current);
        if let Some(producer) = self.producer.borrow_mut().as_mut() {
            producer.target_source_health()?;
            producer.cancel_visual();
        } // Preserve actual Source failure priority after reentrant Native permission checks.
        permission.map_err(ViewError::from)?;
        let outcome = if let Some(side) = ready.side {
            self.input.set_visual_selection_intent(
                &permit,
                ready.selection,
                ready.upstream.unwrap_or(false),
                side,
                &current,
            )
        } else {
            self.input.set_selection_intent(
                &permit,
                ready.selection,
                ready.upstream,
                None,
                &current,
            )
        }
        .map_err(ViewError::from)?;
        if pending.moving().is_some() {
            pending.consume_move()?;
        } else {
            pending.consume_visual()?;
        } // Word 的非空视觉收拢复用原 line/base 查询，只消费本次 Word 动作，后继方向仍留原队列。
        match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
                view.finish_navigation_commit(&commit)?;
                self.arm_word_successor(pending, Some(&commit))
            }
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                *pending.permit.borrow_mut() = Some(permit);
                self.arm_word_successor(pending, None)
            }
        }
    }

    /// 首尾选择只消费队首permit及前一步真实状态；旧到达时长度与anchor没有这里的权限。
    fn commit_document_motion(
        &self,
        pending: &Rc<PendingWordDelete>,
        end: bool,
        extend: bool,
        serial: u64,
    ) -> Result<()> {
        let view = self.view()?;
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        } // 选择不退休原Source，但先前实际I/O失败仍不能被忽略。
        let state = pending.request().input;
        let head = if end { state.utf16_len } else { 0 };
        let anchor = if extend {
            state
                .directed_selection()
                .ok_or(ViewError::Revoked)?
                .anchor_utf16
        } else {
            head
        }; // 扩选保上一步真实anchor；非扩选只折叠到真实BOF/EOF，不从旧主范围猜方向。
        let outcome = self
            .input
            .set_selection_intent(
                &permit,
                MarkdownTextSelection {
                    anchor_utf16: anchor,
                    head_utf16: head,
                },
                Some(end),
                None,
                &current,
            )
            .map_err(ViewError::from)?;
        pending.consume_document()?;
        match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
                view.finish_navigation_commit(&commit)?;
                self.arm_word_successor(pending, Some(&commit))
            }
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                *pending.permit.borrow_mut() = Some(permit);
                self.arm_word_successor(pending, None)
            } // 真无变化不制造文字Undo；后继仍承接原已认证选择。
        }
    }

    /// 首组在原选择成功后才回交；after 请求从当前真实 Input/Native 几何捕获，不能续旧 serial。
    /// parent 持 Down、child 完成 Source 查询时沿现唯一父 owner 交回；不会把 child-local 组写进父域。
    fn publish_word_pointer_group(
        &self,
        view: &NativeMarkdownPagedTextView,
        before: &NativeMarkdownPagedSceneRequest,
        pointer: Option<(WordAnchor, Weak<()>)>,
        commit: Option<&NativeMarkdownPagedInputCommit>,
    ) -> Result<()> {
        let Some((anchor, intent)) = pointer else {
            return Ok(());
        };
        let effect = |cause| ViewError::CommittedEffect(Box::new(cause));
        let after = view.capture_scene_request().map_err(effect)?;
        if let WordAnchor::Group {
            from_utf16,
            to_utf16,
            ..
        } = anchor
        {
            view.accept_pointer_word_group(&after, from_utf16..to_utf16, &intent)
                .map_err(effect)?;
        }
        // 同一真实提交同时交回已接纳Up的有限终点；View已无Down并不表示这个终点被取消。
        self.finish_pointer_word_group(before, &after, anchor, &intent, commit)?;
        Ok(())
    }

    /// 选词只更新同Input真实选择；原布局Source继续使用，不把查询变成文本编辑或退休第二条管线。
    fn commit_word_selection(
        &self,
        pending: &Rc<PendingWordDelete>,
        range: Range<usize>,
        serial: u64,
    ) -> Result<()> {
        let selection = pending.selection_target(range)?;
        let pointer_before = pending.request();
        let pointer_group = pending.pointer_group(); // 真实首组先离开即将消费的队首，不从最终并集反推。
        let view = self.view()?; // 发布选择前取得原View；提交后不把MissingView报告成未发生的普通失败。
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        {
            let mut producer = self.producer.borrow_mut();
            let producer = producer.as_mut().ok_or(ViewError::Revoked)?;
            producer.target_source_health()?;
            producer.cancel_word_group();
            producer.target_source_health()?; // 实际归还游标节点后再核原Source首错，不跳过真实失败。
        }
        let affinity = pending.moving().and_then(|(motion, extend)| {
            let forward = matches!(motion, WordMotion::Forward | WordMotion::Right);
            (extend || pending.request().input.primary_range.length == 0).then_some(forward)
        }); // 逻辑方向及已证明零级的视觉方向保原 assoc；单纯收拢不伪造段内光标侧。
        let outcome = self
            .input
            .set_selection_intent(&permit, selection, affinity, None, &current)
            .map_err(ViewError::from)?;
        if pending.moving().is_some() {
            pending.consume_move()?;
        } else {
            pending.consume_selection()?;
        }
        match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
                // 先回交同内容的首组，再发可重入的选择/滚动通知；新 Drag 不能在通知间隙继续用旧 seed。
                self.publish_word_pointer_group(
                    &view,
                    &pointer_before,
                    pointer_group,
                    Some(commit.as_ref()),
                )?;
                view.finish_navigation_commit(commit.as_ref())?;
                self.arm_word_successor(pending, Some(commit.as_ref()))
            }
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                *pending.permit.borrow_mut() = Some(permit);
                self.publish_word_pointer_group(&view, &pointer_before, pointer_group, None)?;
                self.arm_word_successor(pending, None) // 真相同选择不发假Commit，也不占一笔Undo。
            }
        }
    }

    /// 只撤销失权的原链；迟到回调不能取走同一个Body上已安装的后继队列。
    fn cancel_original_word_delete(&self, pending: &Rc<PendingWordDelete>) -> Result<()> {
        let original = self
            .pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, pending));
        if original {
            self.cancel_word_delete()?;
        }
        Ok(())
    }

    /// 只有旧前台票的真实撤权可交后继；I/O、资源和提交后果都保原失败。
    fn word_input_result(
        &self,
        pending: &Rc<PendingWordDelete>,
        serial: u64,
        released_serial: u64,
        result: Result<()>,
    ) -> Result<()> {
        match result {
            Err(NativeMarkdownPagedBodyError::View(
                cause @ (ViewError::Revoked
                | ViewError::Input(
                    super::super::paged_input::NativeMarkdownPagedInputError::Revoked
                    | super::super::paged_input::NativeMarkdownPagedInputError::StateChanged,
                )),
            )) => {
                // 只用预声明的清理前/后serial判断旧动作是否撤权；此分支不会据此提交或续领permit。
                let expected = if self.serial.get() == serial {
                    serial
                } else {
                    released_serial
                };
                if pending.current_native_at(self, expected) {
                    return Err(cause.into()); // 原票仍有效时真实拒绝不能被解释为后继输入。
                }
                self.cancel_original_word_delete(pending)?; // 原Source首错仍优先，不误取后继队列。
                self.schedule()
            }
            other => other, // Worker、I/O、准备失败及CommittedEffect均保原失败，不任意豁免。
        }
    }

    /// 清理前后只消费同张原票；Source和Cell都归还真实读取，不调用仅限Cell的释放入口。
    fn commit_word_input(
        &self,
        pending: &Rc<PendingWordDelete>,
        edit: WordEdit<'_>,
        serial: u64,
        released_serial: u64,
    ) -> Result<()> {
        let (text, replacement, deleting, source_command) = match edit {
            WordEdit::Typing(text) => (
                text,
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
                false,
                None,
            ), // 已转换文本已经移出输入占位，不能把下一步Source命令误当成本步。
            WordEdit::Delete(range) => ("", range, true, None), // 删除范围来自同一Source真实GCB或完整选择。
            WordEdit::Source(command) => (
                "",
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
                false,
                Some(command),
            ), // Core命令稍后才读取本次真实选择，不预制续行或缩进文本。
        };
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let view = self.view()?;
        self.retire_word_reading(pending, &permit, serial, released_serial, deleting)?;
        let current =
            |epoch| self.current(epoch) && pending.current_native_at(self, released_serial);
        // 普通编辑结束整条输入链后也要显示真实caret；须在消费当前动作前固定其语义。
        let reveal_body_edit = !deleting || pending.deleting_grapheme();
        let outcome = if let Some(command) = source_command {
            self.input
                .apply_source_editing_command(&permit, command, None, &current)
                .map_err(ViewError::from)?
                .expect("已接纳的Return和缩进命令由Core明确消费，不需要几何后备")
        } else if deleting
            && pending.deleting_grapheme()
            && pending.direction() == Some(false)
            && !self.input.is_cell()
        {
            // 退格仍先遵守原列表/缩进标记规则；普通GCB范围只是Core未命中标记时的真实后备。
            self.input
                .apply_source_editing_command(
                    &permit,
                    omni_markdown_core::MarkdownEditingCommand::DeleteMarkupBackward,
                    Some(replacement),
                    &current,
                )
                .map_err(ViewError::from)?
                .expect("已提供同源完整GCB范围，普通退格必有明确结果")
        } else {
            // 词组删除、前删和普通新增输入都消费本步真实范围，不引入第二次permit或选择重捕。
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(
                self.input
                    .insert_text(&permit, text, replacement, &current)
                    .map_err(ViewError::from)?,
            )
        };
        if source_command.is_some() {
            pending.consume_source()?; // 命令实际提交或明确无变化后才减次数，保持每步Undo与有向选择。
        } else if deleting {
            pending.consume_word()?; // 真实Core结果已确定后只消费本步；后继动作继承同次收据或原选择。
        }
        let commit = match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => commit,
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                // 无需创建正文历史；仍保原票交接后续已接纳动作和上一收据的真实caret显示。
                *pending.permit.borrow_mut() = Some(permit);
                return self.arm_word_successor(pending, None);
            }
        };
        let commit = Rc::new(commit);
        // Cell封闭Source域随本次真实内容版本改变；只能由这张原Input收据领取其后继域，
        // 不能沿旧Cell authority继续，也不能从同一Doc/相同文本重建权限。
        let target = (|| {
            commit.check_current(&|epoch| self.current(epoch))?;
            let target = self.input.target_domain()?;
            commit.check_current(&|epoch| self.current(epoch))?;
            Ok::<_, super::super::paged_input::NativeMarkdownPagedInputError>(target)
        })()
        .map_err(|cause| ViewError::CommittedEffect(Box::new(ViewError::Input(cause))))?;
        pending.request.borrow_mut().target = target;
        *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
        let before_gutter = pending.request();
        let synchronized = self
            .synchronize_committed_gutter(commit.as_ref(), &before_gutter)
            .map_err(|cause| ViewError::CommittedEffect(Box::new(cause)))?;
        if !pending.current_at(self, released_serial) {
            return Err(ViewError::CommittedEffect(Box::new(ViewError::Revoked)).into());
        }
        *pending.request.borrow_mut() = synchronized;
        // 只有同一真实提交引起的行号横向变化能在交接边界更新原请求；后继动作不继承外部resize。
        view.finish_word_delete_commit(commit.as_ref())?;
        if !self.input.is_cell()
            && ((reveal_body_edit && pending.steps.borrow().is_empty())
                || pending.source_command().is_some())
        {
            // 最后普通编辑或后继可能Unchanged的缩进命令，保本次真实收据授权的弱定位意图。
            // 通知后的Native重入若又排入动作，原arm仍会撤掉本意图，不替旧输入补签后继状态。
            self.accept_selection_reveal(commit.as_ref())
                .map_err(|cause| ViewError::CommittedEffect(Box::new(cause)))?;
        }
        self.arm_word_successor(pending, Some(commit.as_ref()))
    }

    /// 下一步只继承真实Commit；端点无变化则保原已认证before，不借当前新用户状态续权。
    fn arm_word_successor(
        &self,
        pending: &Rc<PendingWordDelete>,
        commit: Option<&NativeMarkdownPagedInputCommit>,
    ) -> Result<()> {
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        if let Some(commit) = commit {
            commit.check_current(&current).map_err(ViewError::from)?;
        } else {
            let permit = pending
                .permit
                .borrow_mut()
                .take()
                .ok_or(ViewError::Revoked)?;
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
        }
        if pending.steps.borrow().is_empty() && !pending.interpreting_native.get() {
            let fold_failure = pending.fold_failure.borrow_mut().take();
            let attachment_failure = pending.attachment_failure.borrow_mut().take();
            let removed = self.pending_word_delete.borrow_mut().take();
            drop(removed); // current已核同一Rc且以下没有Native；不会误取后继队列。
            if let Some(cause) = attachment_failure {
                self.view()?.reject_attachment_import(cause); // 真实后继输入已完成，反馈改变viewport时不再保有旧链权限。
            }
            if let Some(cause) = fold_failure {
                self.view()?.reject_source_fold(cause);
            }
            return self.schedule();
        }
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::from)?;
        let expected = pending.request();
        let request = self.view()?.capture_scene_request()?;
        if let Some(commit) = commit {
            commit.check_current(&current).map_err(ViewError::from)?;
        }
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if !request.same(&expected) {
            return Err(ViewError::Revoked.into()); // 几何或无关输入改变只撤余项，不能拿后继request重新授权。
        }
        // 从这里到安装完成没有Native/可重入调用；旧receipt与下一张permit只交接一次。
        *pending.request.borrow_mut() = request;
        *pending.permit.borrow_mut() = Some(permit);
        pending.bound.borrow_mut().take();
        pending.ready.borrow_mut().take();
        pending.visual_seed.set(None);
        pending.visual_ready.set(None);
        pending.move_prepared.set(false);
        pending.visual_word.set(false);
        pending.retired_serial.set(None);
        pending.receipt.borrow_mut().take();
        if pending.source_command().is_none()
            && pending.document_motion().is_none()
            && !pending.native_front()
            && !pending.interpreting_native.get()
        {
            drop(self.selection_reveal.borrow_mut().take());
        } // 语义命令或重复端点若真实Unchanged，仍保前步显示意图；不能靠伪造选择Commit补回。
        self.revoke_source_target(); // 下一已接纳输入接管位置意图，不能先为上一Cell收据强制准备远端caret。
        self.schedule()
    }
}
