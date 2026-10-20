//! 裸Page只逐次执行原NSScrollView动作；等待的仅是同Source真实布局高度，不移动文本选择。

use super::*;
use omni_markdown_core::MarkdownLayoutTextAuthority;

const MAX_DIRECTION_SEGMENTS: usize = 32;

struct DirectionSegment {
    down: bool,
    remaining: usize,
}

/// 只保原弱输入票、实际滚动来源和有限方向段；不保存事件、正文、Source强引用或Native对象。
pub(super) struct PendingPageScroll {
    request: NativeMarkdownPagedSceneRequest,
    pub source: MarkdownLayoutTextAuthority,
    serial: u64,
    layout_epoch: u64,
    origin: Cell<objc2_foundation::NSPoint>,
    segments: RefCell<VecDeque<DirectionSegment>>,
    executing: Cell<bool>,
}

impl PendingPageScroll {
    fn current(&self, owner: &NativeMarkdownPagedBody) -> bool {
        owner.current_body()
            && owner.serial.get() == self.serial
            && owner.layout_epoch.get() == self.layout_epoch
            && Weak::ptr_eq(&self.request.document, &owner.input.document_identity())
            && owner
                .input
                .state()
                .is_ok_and(|state| state == self.request.input)
            && owner
                .pending_page_scroll
                .borrow()
                .as_ref()
                .is_some_and(|pending| std::ptr::eq(&**pending, self))
    }

    fn same_input_geometry(&self, actual: &NativeMarkdownPagedSceneRequest) -> bool {
        actual.target.is_body(&actual.document)
            && Weak::ptr_eq(&actual.document, &self.request.document)
            && actual.input == self.request.input
            && actual.virtual_viewport.layout_width == self.request.virtual_viewport.layout_width
            && actual.virtual_viewport.layout_origin_x
                == self.request.virtual_viewport.layout_origin_x
            && actual.viewport.size.height == self.request.viewport.size.height
    }

    pub(super) fn matches(
        &self,
        owner: &NativeMarkdownPagedBody,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> bool {
        self.current(owner)
            && self.same_input_geometry(actual)
            && actual.virtual_viewport.document_origin == self.origin.get()
    }

    fn current_native(&self, owner: &NativeMarkdownPagedBody) -> bool {
        // 原Native动作主动改变origin；Source/Input/宽高仍守原票，外部滚动在下一轮matches撤销。
        self.current(owner)
            && owner.view().is_ok_and(|view| {
                view.capture_scene_request()
                    .is_ok_and(|actual| self.same_input_geometry(&actual))
            })
            && self.current(owner)
    }

    fn append(&self, down: bool) -> std::result::Result<(), ViewError> {
        if self.executing.get() {
            return Err(ViewError::Busy); // 原Native效果尚在栈内，不接受无法确定先后origin的新动作。
        }
        let mut segments = self.segments.borrow_mut();
        if let Some(last) = segments.back_mut().filter(|last| last.down == down) {
            let remaining = last
                .remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            last.remaining = remaining; // 溢出先拒绝，原已接受次数保持不变；长按同方向不占新段。
        } else {
            if segments.len() == MAX_DIRECTION_SEGMENTS {
                return Err(ViewError::Busy); // 方向段满明确拒绝，不接受后丢掉反向或重排次序。
            }
            segments.push_back(DirectionSegment { down, remaining: 1 });
        }
        Ok(())
    }

    pub(super) fn needs_next_page(&self) -> bool {
        self.segments
            .borrow()
            .front()
            .is_some_and(|segment| segment.down)
    }
}

impl NativeMarkdownPagedBody {
    pub(super) fn accept_page_scroll(
        &self,
        actual: &NativeMarkdownPagedSceneRequest,
        down: bool,
    ) -> Result<()> {
        self.find_source_health()?; // 普通前台撤权不能掩盖原Source已经存在的I/O首错。
        let previous = self.pending_page_scroll.borrow().clone();
        if let Some(previous) = previous.as_ref() {
            if previous.executing.get() {
                return Err(ViewError::Busy.into());
            }
            if previous.matches(self, actual) {
                previous.append(down)?;
                self.schedule()?;
                return Ok(());
            }
        }
        let serial = self.serial.get();
        let source = {
            let slot = self.producer.borrow();
            slot.as_ref()
                .ok_or(ViewError::Pending)?
                .target_source_authority(actual, serial)?
        }; // 只借已存在Producer；没有原Source时明确Pending，不捕第二正文补票。
        if !self.current_body()
            || !actual.target.is_body(&actual.document)
            || !Weak::ptr_eq(&actual.document, &self.input.document_identity())
            || self.input.state().map_err(ViewError::Input)? != actual.input
        {
            return Err(ViewError::Revoked.into());
        }
        self.cancel_pending_find()?;
        let latest = self.view()?.capture_scene_request()?;
        self.find_source_health()?;
        if !latest.same(actual) || self.serial.get() != serial {
            return Err(ViewError::Revoked.into());
        }
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target(); // 纯滚动是新的视口意图，旧Find/选择reveal不能把它拉回原caret。
        self.pending_page_scroll
            .replace(Some(Rc::new(PendingPageScroll {
                request: actual.clone(),
                source,
                serial,
                layout_epoch: self.layout_epoch.get(),
                origin: Cell::new(actual.virtual_viewport.document_origin),
                segments: RefCell::new(VecDeque::from([DirectionSegment { down, remaining: 1 }])),
                executing: Cell::new(false),
            })));
        let old_scene = self.scene.borrow_mut().take();
        drop(old_scene); // 原业务Scene立即撤销，未完成旧像素不能充当滚动最终帧；Native旧完整图仍可显示。
        self.schedule()
    }

    pub(super) fn refresh_page_scroll(&self, actual: &NativeMarkdownPagedSceneRequest) {
        let pending = self.pending_page_scroll.borrow().clone();
        if pending
            .as_ref()
            .is_some_and(|pending| !pending.matches(self, actual))
        {
            drop(self.pending_page_scroll.borrow_mut().take()); // 编辑、宽度、外部scroll或新owner接管后不重签旧队列。
        }
    }

    /// 每轮最多一个已经接受的方向；次数在Native前消费，失败也绝不重放潜在已发生的滚动。
    pub(super) fn consume_page_scroll(&self, pending: &Rc<PendingPageScroll>) -> Result<()> {
        let actual = self.view()?.capture_scene_request()?;
        if !pending.matches(self, &actual) {
            return Err(ViewError::Revoked.into());
        }
        let down = {
            let mut segments = pending.segments.borrow_mut();
            let segment = segments.front_mut().ok_or(ViewError::Revoked)?;
            let down = segment.down;
            segment.remaining -= 1;
            if segment.remaining == 0 {
                segments.pop_front();
            }
            down
        };
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Closed)?;
        pending.executing.set(true);
        let result = viewport.scroll_page(down, &|| pending.current_native(self));
        pending.executing.set(false); // 所有RefCell借用已结束才进Native，返回先结束执行期再处理原错误。
        pending.origin.set(result?);
        if pending.segments.borrow().is_empty() {
            let mut slot = self.pending_page_scroll.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, pending))
            {
                slot.take(); // 只归还自己的队列，旧回调不能取走后继请求。
            }
        }
        self.schedule() // 即使真实EOF空操作也一轮一次；后继动作不能在单回调中成千次阻塞主线程。
    }
}
