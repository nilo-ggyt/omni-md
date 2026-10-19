//! 词组删除、选词和随后普通输入沿同一提交链保序；每步只消费原Composer或有限新增载荷。

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputPermit;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedWordAnchor as WordAnchor, NativeMarkdownPagedWordSelection as WordSelection,
};
use objc2_foundation::NSRange;
use omni_markdown_core::{
    MarkdownLayoutTextAuthority, MarkdownTextSelection, MarkdownWordDeleteAnswer,
    MarkdownWordGroupAction, MarkdownWordGroupAnswer,
};
use std::ops::Range;

/// 相邻同方向只保次数；普通输入槽预留真实到达次序，转换后移动唯一新增字符串。
enum WordInput {
    Delete {
        forward: bool,
        remaining: usize,
    },
    Move {
        forward: bool,
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
        target: Option<Range<usize>>,
    },
    Typing(Rc<RefCell<Option<String>>>),
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

/// 同一个真实输入链只持当前一张票与最后提交收据；不保原生事件、正文或第二份Input。
pub(super) struct PendingWordDelete {
    request: RefCell<NativeMarkdownPagedSceneRequest>,
    steps: RefCell<VecDeque<WordInput>>,
    queued_bytes: Cell<usize>,
    receipt: RefCell<Option<Rc<NativeMarkdownPagedInputCommit>>>,
    retired_serial: Cell<Option<u64>>,
    permit: RefCell<Option<NativeMarkdownPagedInputPermit>>,
    bound: RefCell<Option<(u64, MarkdownLayoutTextAuthority)>>,
    ready: RefCell<Option<Range<usize>>>,
    move_prepared: Cell<bool>,
    move_rejected: Cell<bool>,
    pub(super) visual_seed: Cell<Option<VisualIntent>>,
    pub(super) visual_ready: Cell<Option<VisualSelection>>,
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
    fn append_word(&self, forward: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if let Some(WordInput::Delete {
            forward: last,
            remaining,
        }) = steps.back_mut()
            && *last == forward
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() == 32 {
            return Err(ViewError::Busy); // 与原垂直动作同一有限方向段边界；长按同方向不受次数32限制。
        }
        steps.push_back(WordInput::Delete {
            forward,
            remaining: 1,
        });
        Ok(())
    }

    /// 同方向且相同扩选规则的长按只压次数；交错动作仍按原到达顺序保留。
    fn append_move(&self, forward: bool, extend: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if let Some(WordInput::Move {
            forward: last,
            extend: extending,
            remaining,
        }) = steps.back_mut()
            && *last == forward
            && *extending == extend
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() == 32 {
            return Err(ViewError::Busy);
        }
        steps.push_back(WordInput::Move {
            forward,
            extend,
            remaining: 1,
        });
        Ok(())
    }

    /// Share the Word/Typing queue; coalesce only adjacent motions with the same direction and extension rule.
    fn append_visual(&self, right: bool, extend: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if let Some(WordInput::Visual {
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
        if steps.len() == 32 {
            return Err(ViewError::Busy);
        }
        steps.push_back(WordInput::Visual {
            right,
            extend,
            remaining: 1,
        });
        Ok(())
    }
    pub(super) fn visual_motion(&self) -> Option<(bool, bool)> {
        match self.steps.borrow().front()? {
            WordInput::Visual { right, extend, .. } => Some((*right, *extend)),
            _ => None,
        }
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

    fn moving(&self) -> Option<(bool, bool)> {
        match self.steps.borrow().front()? {
            WordInput::Move {
                forward, extend, ..
            } => Some((*forward, *extend)),
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
            | WordInput::Select { .. }
            | WordInput::Move { .. }
            | WordInput::Visual { .. } => None,
        }
    }

    /// 正文仅在这类未改字符的查询期间保留同票Scene，供后继原指针继续真实命中。
    pub(super) fn selecting(&self) -> bool {
        matches!(self.steps.borrow().front(), Some(WordInput::Select { .. }))
    }

    /// 首先计算本次真实指针组；拖动确实离开首击位置时再借同一Source计算原锚组。
    fn selection_query(&self) -> Option<(usize, bool)> {
        let steps = self.steps.borrow();
        let WordInput::Select { selection, target } = steps.front()? else {
            return None;
        };
        if target.is_some()
            && let Some(WordAnchor::Word {
                head_utf16,
                backward_bias,
            }) = selection.anchor
        {
            return Some((head_utf16, backward_bias)); // 目标组已完成，下一量子查询首击组，不重复扫描目标。
        }
        Some((selection.head_utf16, selection.backward_bias))
    }

    /// 两个词组串行复用一个Core槽；只保存前一真实范围，不保存整词或第二组Reader。
    fn selection_anchor_query(&self, range: Range<usize>) -> Option<(usize, bool)> {
        let mut steps = self.steps.borrow_mut();
        let WordInput::Select { selection, target } = steps.front_mut()? else {
            return None;
        };
        if target.is_none()
            && let Some(WordAnchor::Word {
                head_utf16,
                backward_bias,
            }) = selection.anchor
            && head_utf16 != selection.head_utf16
        {
            *target = Some(range);
            return Some((head_utf16, backward_bias));
        }
        None
    }

    /// 同原CodeMirror rangeForClick/SelectionRange.extend：拖动保两端完整组，Shift保真实锚点。
    fn selection_target(
        &self,
        range: Range<usize>,
    ) -> std::result::Result<MarkdownTextSelection, ViewError> {
        if let Some((forward, extend)) = self.moving() {
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
        let Some(WordInput::Select { selection, target }) = steps.front() else {
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
            Some(WordAnchor::Word {
                head_utf16: first, ..
            }) if first == selection.head_utf16 => (range.start, range.end),
            Some(WordAnchor::Word { .. }) => {
                let target = target.as_ref().ok_or(ViewError::Revoked)?;
                let from = range.start.min(target.start);
                let to = range.end.max(target.end);
                if from < target.start {
                    (from, to)
                } else {
                    (to, from)
                }
                // 真正离开首击位置后，方向来自两组的逻辑顺序；反向拖动不得丢掉首击词的后半段。
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
            WordInput::Delete { .. }
            | WordInput::Select { .. }
            | WordInput::Move { .. }
            | WordInput::Visual { .. } => false,
        })
    }

    /// 取消先归还已经转换的全部新增载荷；尚在Native栈内的预留由它自己的失败出口归还。
    fn clear_steps(&self) {
        let steps = std::mem::take(&mut *self.steps.borrow_mut());
        let bytes: usize = steps
            .iter()
            .map(|step| match step {
                WordInput::Typing(text) => text.borrow().as_ref().map_or(0, String::len),
                WordInput::Delete { .. }
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
                view.capture_scene_request()
                    .is_ok_and(|actual| actual.same(&self.request()))
            })
            && self.current_at(owner, serial)
    }
}

impl NativeMarkdownPagedBody {
    /// 明确选词/指针选择接管旧位置意图；原permit仍指同Input，不捕后继正文或补签旧命令。
    pub(super) fn accept_word_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        selection: WordSelection,
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
                target: None,
            }])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            move_prepared: Cell::new(false),
            move_rejected: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
        }));
        self.schedule()
    }

    /// 标准Word移动沿同一个已接纳输入链；全源UBA能力仅由原Producer的准备结果决定。
    pub(super) fn accept_word_move(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        forward: bool,
        extend: bool,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.accept_queued_move(request, forward, extend, permit, false)
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
        self.accept_queued_move(request, right, extend, permit, true)?;
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
        forward: bool,
        extend: bool,
        permit: NativeMarkdownPagedInputPermit,
        visual: bool,
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
            if visual {
                previous.append_visual(forward, extend)?;
            } else {
                previous.append_move(forward, extend)?;
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
            steps: RefCell::new(VecDeque::from([if visual {
                WordInput::Visual {
                    right: forward,
                    extend,
                    remaining: 1,
                }
            } else {
                WordInput::Move {
                    forward,
                    extend,
                    remaining: 1,
                }
            }])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            move_prepared: Cell::new(false),
            move_rejected: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
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
            previous.append_word(forward)?;
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
                remaining: 1,
            }])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            move_prepared: Cell::new(false),
            move_rejected: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
        });
        *self.pending_word_delete.borrow_mut() = Some(pending);
        self.schedule()
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
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // 队列额度拒绝也不能覆盖原Source已记录的读取首错。
        }
        let serial = self.serial.get();
        let payload = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() == 32 {
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

    /// 当前普通输入只消费上一步真实receipt派生的票；一回合仍只执行一笔原输入事务。
    pub(super) fn advance_word_typing(&self) -> Result<bool> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
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
        let result = self.commit_word_input(
            &pending,
            &text,
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
            false,
            serial,
            released_serial,
        );
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
        if pending
            .as_ref()
            .is_some_and(|pending| !pending.current(self) || !pending.request().same(request))
        {
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
                if let Some((head, backward_bias)) = pending.selection_query() {
                    producer.begin_word_group(
                        MarkdownWordGroupAction::Select { backward_bias },
                        head,
                    )?;
                } else if pending.moving().is_none()
                    && pending.visual_motion().is_none()
                    && request.input.primary_range.length == 0
                {
                    producer.begin_word_delete(
                        pending.direction().ok_or(ViewError::Revoked)?,
                        request.input.primary_range.location,
                    )?;
                } // 选词后的删除消费那次真实完整选择，不从其首端再扩词；Source仍按同一绑定核健康。
                *pending.bound.borrow_mut() = Some((serial, source));
            }
            Some((original_serial, original_source)) => {
                if original_serial != serial || original_source != source {
                    return Err(ViewError::Revoked.into()); // 不能用后继Producer重新计算同一次旧删除。
                }
            }
        }
        if let Some((right, extend)) = pending.visual_motion() {
            self.bind_visual_seed(&pending, request, serial, right, extend)?;
            return producer.advance(self, request); // Original font/line work and bounded visual queries share turns; do not scroll before commit.
        }
        if let Some((forward, extend)) = pending.moving() {
            if !pending.move_prepared.get() {
                let proved = producer.prepare_word_move(self, request)?;
                if !pending.current(self) {
                    return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
                }
                match proved {
                    None => return self.schedule(), // 本回合只有原Bidi量子，不再启动/推进GCB。
                    Some(false) => {
                        pending.move_rejected.set(true);
                        return Ok(()); // 健康能力拒绝留到原Producer归槽后逐项交付，不毒化Source。
                    }
                    Some(true) => pending.move_prepared.set(true),
                }
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
                    return Ok(()); // 收拢只用原有向选择，没有正文读取/字素猜测。
                }
                let action = if forward {
                    MarkdownWordGroupAction::Forward
                } else {
                    MarkdownWordGroupAction::Backward
                };
                producer.begin_word_group(action, request.input.selection.head_utf16)?;
                return self.schedule(); // 原Bidi这次可能刚读完，GCB只能在下一原量子推进。
            }
            let answer = producer.step_word_group(self, request)?;
            if !pending.current(self) {
                return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
            }
            return match answer {
                MarkdownWordGroupAnswer::Yielded => self.schedule(),
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
                MarkdownWordGroupAnswer::Ready(range) => {
                    if let Some((head, backward_bias)) =
                        pending.selection_anchor_query(range.clone())
                    {
                        producer.cancel_word_group();
                        producer.begin_word_group(
                            MarkdownWordGroupAction::Select { backward_bias },
                            head,
                        )?;
                        self.schedule() // 下一回合仍只step一次；本回合不额外读取原首击词组。
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
        if pending.move_rejected.replace(false) {
            let serial = pending
                .bound
                .borrow()
                .as_ref()
                .map(|bound| bound.0)
                .ok_or(ViewError::Revoked)?;
            if !pending.current_native_at(self, serial) {
                self.cancel_original_word_delete(&pending)?;
                return self.schedule();
            }
            if let Some(producer) = self.producer.borrow().as_ref() {
                producer.target_source_health()?;
            }
            let view = self.view()?;
            pending.consume_move()?;
            view.reject_word_move(); // 原View交付明确Unsupported，Body/Source保持健康，后继已接纳字符照常结算。
            let result = self.arm_word_successor(&pending, None);
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
            "",
            NSRange::new(range.start, range.len()),
            true,
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
        pending.consume_visual()?;
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

    /// 选词只更新同Input真实选择；原布局Source继续使用，不把查询变成文本编辑或退休第二条管线。
    fn commit_word_selection(
        &self,
        pending: &Rc<PendingWordDelete>,
        range: Range<usize>,
        serial: u64,
    ) -> Result<()> {
        let selection = pending.selection_target(range)?;
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
        let affinity = pending.moving().and_then(|(forward, extend)| {
            (extend || pending.request().input.primary_range.length == 0).then_some(forward)
        }); // UniformZero方向的CM assoc对应真实上下游侧；仅收拢沿原rangeEnd的未指定侧。
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
                view.finish_navigation_commit(commit.as_ref())?;
                self.arm_word_successor(pending, Some(commit.as_ref()))
            }
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                *pending.permit.borrow_mut() = Some(permit);
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
        text: &str,
        replacement: NSRange,
        deleting: bool,
        serial: u64,
        released_serial: u64,
    ) -> Result<()> {
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let before = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(&permit, &before)
            .map_err(ViewError::from)?;
        let view = self.view()?;
        view.release_scene_readers()?; // Source和Cell都先归还CPU候选实际读取，旧像素可继续显示。
        self.input
            .check_permit(&permit, &before)
            .map_err(ViewError::from)?;
        let resources = {
            let producer = self.producer.borrow();
            // 原Source先报已发生的错误；随后固定同worker的原观察票，才能核Drop中才出现的失败。
            // 观察票只含原子事实，不保Source、Reader或尚未发布的行。
            producer
                .as_ref()
                .map(|producer| {
                    producer.target_source_health()?;
                    producer.word_retirement_resources()
                })
                .transpose()?
        };
        if deleting && resources.is_none() {
            return Err(ViewError::Revoked.into()); // Word Ready必须来自实际Producer，不能因缺收据跳过退休门。
        }
        self.serial.set(released_serial); // 只推进已预声明的原读取退休代次，迟到字体结果不再碰后继Producer。
        pending.retired_serial.set(Some(released_serial));
        self.scheduled.set(None);
        self.deferred.set(false);
        let scene = self.scene.borrow_mut().take();
        let producer = self.producer.borrow_mut().take();
        drop(scene); // 原Scene先归还Reader，随后显式退休Source与已完成字体结果。
        let producer_result = producer.map_or(Ok(()), Producer::retire);
        let worker = self.worker.borrow_mut().take();
        let worker_result = worker.map_or(Ok(()), FontWorker::close);
        let retirement = super::producer::merge_preparation_cleanup(producer_result, worker_result);
        // 字体packet/result也已真实归还；先汇总原Source的最后释放错误，再处理字体关闭失败。
        // 即使清理失败也不留下原读取owner，更不能继续删除canonical后才报告失败。
        self.font_notices.borrow_mut().clear();
        let released = resources
            .as_ref()
            .and_then(|resources| resources.snapshot());
        if let Some(errno) = released.as_ref().and_then(|state| state.first_source_errno) {
            let source = ViewError::Document(
                omni_markdown_core::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            );
            // 原Source首因优先，但已合并的Producer/Worker真实清理失败仍须完整保留；
            // 相同errno不能证明另一个试次来自同一observer，不能据此删除既有typed因果链。
            return super::producer::merge_preparation_cleanup(Err(source.into()), retirement);
        }
        retirement?;
        if resources.is_some() && released.is_none_or(|state| !state.is_released()) {
            // 实际GCB、索引或writer的close/munmap失败保存在原scope；前置健康检查不能代替它。
            return Err(NativeMarkdownPagedBodyError::ResourceReleaseFailed);
        }
        let current =
            |epoch| self.current(epoch) && pending.current_native_at(self, released_serial);
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        let commit = self
            .input
            .insert_text(&permit, text, replacement, &current)
            .map_err(ViewError::from)?;
        if deleting {
            pending.consume_word()?; // 真实发布后只消费这一词；剩余次数必须从本次Commit.state继续。
        }
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
        view.finish_word_delete_commit(commit.as_ref())?;
        if !deleting && !self.input.is_cell() && pending.steps.borrow().is_empty() {
            // 中间已接纳动作继续接管同一位置意图；最后一次Source键入才用其真实收据定位caret。
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
        if pending.steps.borrow().is_empty() {
            let removed = self.pending_word_delete.borrow_mut().take();
            drop(removed); // current已核同一Rc且以下没有Native；不会误取后继队列。
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
        pending.move_rejected.set(false);
        pending.retired_serial.set(None);
        pending.receipt.borrow_mut().take();
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target(); // 下一已接纳输入接管位置意图，不能先为上一Cell收据强制准备远端caret。
        self.schedule()
    }
}
