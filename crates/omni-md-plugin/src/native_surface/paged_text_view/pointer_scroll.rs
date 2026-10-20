//! 真实拖选越出视口后的连续滚动；仅保最后一个有限点和原手势弱身份，不保存或重放NSEvent。

use super::*;
use dispatch2::MainThreadBound;
use objc2::rc::Weak as NativeWeak;
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use std::ptr::NonNull;

/// 每个实际输入客户最多一份期限；timer闭包只弱借view，不拥有Source、文稿、字体或Scene。
pub(super) struct DragScroll {
    timer: Retained<NSTimer>,
    point: NSPoint,
    intent: Weak<()>,
    clicks: usize,
}

impl Drop for DragScroll {
    fn drop(&mut self) {
        // 这个值已从原槽提取，析构只结束自己拥有的系统期限。
        // invalidate 可同步重入；不能再查 view 的槽，更不能取走重入刚建立的新 timer。
        self.timer.invalidate();
    }
}

impl NativeMarkdownPagedTextView {
    /// 实际Drag已完成正常选择解释；这里仅用其有限位置开始/更新原手势的连续滚动。
    /// intent 在原事件入口、任何 Native 调用之前捕获；本方法不得重新认领一个较新的 Down。
    pub(super) fn update_drag_scroll(&self, event: &NSEvent, intent: &Weak<()>) {
        let forward = self.ivars().cell_pointer_forward.borrow().clone();
        if let Some(forward) = forward {
            if Weak::ptr_eq(&forward.intent, intent)
                && let Some(parent) = forward.parent.upgrade()
                && parent.cell_pointer_is_current(intent)
            {
                parent.update_drag_scroll(event, intent);
            }
            return; // 真实回调尚在栈内，可交当前父客户；不延长已经seal的旧草稿。
        }
        let result = (|| {
            if !self.cell_pointer_is_current(intent) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let origin = self.origin()?;
            let current = || {
                self.cell_pointer_is_current(intent)
                    && origin.current(self)
                    && self.cell_pointer_is_current(intent)
            };
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let point = event.locationInWindow();
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let clicks = self
                .ivars()
                .pointer_motion
                .get()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?
                .clicks;
            self.start_drag_scroll(point, intent, clicks)
        })();
        // 真实移交或关闭可能已撤原客户；这是原期限自然结束，不为失权的旧view记录伪输入错误。
        if !matches!(
            &result,
            Err(NativeMarkdownPagedTextViewError::Closed
                | NativeMarkdownPagedTextViewError::Revoked)
        ) {
            self.record(result);
        }
    }

    /// 任一方向越出实际有限正文才建 timer；两轴都回到正文、手势结束或候选开始即归还期限。
    fn start_drag_scroll(&self, point: NSPoint, intent: &Weak<()>, clicks: usize) -> Result<()> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if !self.cell_pointer_is_current(intent) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let origin = self.origin()?;
        let current = || {
            self.cell_pointer_is_current(intent)
                && origin.current(self)
                && self.cell_pointer_is_current(intent)
        };
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if origin.state.marked.is_some() {
            self.stop_drag_scroll_intent(intent);
            return Ok(()); // 系统候选期间鼠标归输入法，不能让旧拖选改变候选锚点。
        }
        let local = self.convertPoint_fromView(point, None);
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let bounds = self.bounds();
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let right = bounds.origin.x + bounds.size.width;
        let top = bounds.origin.y + bounds.size.height;
        if !local.x.is_finite()
            || !local.y.is_finite()
            || !bounds.origin.x.is_finite()
            || !bounds.origin.y.is_finite()
            || !bounds.size.width.is_finite()
            || !bounds.size.height.is_finite()
            || bounds.size.width < 0.0
            || bounds.size.height < 0.0
            || !right.is_finite()
            || !top.is_finite()
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if local.x >= bounds.origin.x
            && local.x <= right
            && local.y >= bounds.origin.y
            && local.y <= top
        {
            self.stop_drag_scroll_intent(intent);
            return Ok(());
        }
        let previous = {
            let mut slot = self.ivars().drag_scroll.borrow_mut();
            if let Some(active) = slot.as_mut()
                && Weak::ptr_eq(&active.intent, intent)
            {
                active.point = point;
                active.clicks = clicks;
                return Ok(()); // 高频真实Drag覆盖一个点，不为每个事件建立timer或队列项。
            }
            slot.take()
        };
        drop(previous); // 只归还刚提取的旧期限；重入产生的新槽在下方发布前仍必须完整保留。
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let weak = MainThreadBound::new(NativeWeak::from(self), self.mtm());
        let block = block2::RcBlock::new(move |timer: NonNull<NSTimer>| {
            let mtm = MainThreadMarker::new().expect("drag timer runs on the main thread");
            // SAFETY: Foundation在同步timer回调期间持有该参数；本闭包不保存参数或转交其他线程。
            let timer = unsafe { timer.as_ref() };
            if let Some(view) = weak.get(mtm).load() {
                view.drag_scroll_tick(timer);
            } else {
                timer.invalidate(); // owner已销毁也终止系统期限，弱闭包不能成为常驻空循环。
            }
        });
        // SAFETY: 主线程的原生重复期限；闭包只有MainThreadBound<Weak>，不保业务或输入对象。
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.05, true, &block) };
        let mut created = Some(DragScroll {
            timer: timer.clone(),
            point,
            intent: intent.clone(),
            clicks,
        });
        if !current() {
            drop(created);
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let installed = {
            let mut slot = self.ivars().drag_scroll.borrow_mut();
            if slot.is_some() {
                false // Native 建立期限期间发生的新动作已经有 owner，旧栈不得覆盖它。
            } else {
                *slot = created.take();
                true
            }
        };
        drop(created); // 未发布的期限也只释放自己；绝不在持槽借用时进入 Native 析构。
        if !installed {
            return Ok(());
        }
        if !current() || !self.owns_drag_scroll(&timer, intent) {
            self.stop_original_drag_scroll(&timer, intent);
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let run_loop = NSRunLoop::mainRunLoop();
        if !current() || !self.owns_drag_scroll(&timer, intent) {
            self.stop_original_drag_scroll(&timer, intent);
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: 只把这个实际主线程timer加入主runloop；CommonModes覆盖正常事件和Native跟踪。
        unsafe { run_loop.addTimer_forMode(&timer, NSRunLoopCommonModes) };
        if !current() || !self.owns_drag_scroll(&timer, intent) {
            self.stop_original_drag_scroll(&timer, intent);
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    /// 一轮最多滚动一个实际视口，然后把同一点交回原有限终点槽；下一帧未Ready就等待，不抢排版。
    fn drag_scroll_tick(&self, timer: &NSTimer) {
        // 这是 Foundation 独立进入的真实回调栈；原 close 必须等它归还 Input/Scene 借用。
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let active = self
            .ivars()
            .drag_scroll
            .borrow()
            .as_ref()
            .and_then(|active| {
                std::ptr::eq::<NSTimer>(&*active.timer, timer)
                    .then(|| (active.point, active.intent.clone(), active.clicks))
            });
        let Some((point, intent, clicks)) = active else {
            timer.invalidate(); // 迟到旧timer不能撤销这个view后来的新期限。
            return;
        };
        if !self.cell_pointer_is_current(&intent) {
            self.stop_original_drag_scroll(timer, &intent);
            return;
        }
        let result = (|| {
            let origin = self.origin()?;
            let current = || {
                self.owns_drag_scroll(timer, &intent)
                    && self.drag_scroll_point_is_current(point, &intent)
                    && self.cell_pointer_is_current(&intent)
                    && origin.current(self)
                    && self.cell_pointer_is_current(&intent)
                    && self.drag_scroll_point_is_current(point, &intent)
                    && self.owns_drag_scroll(timer, &intent)
            };
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if origin.state.marked.is_some() {
                self.stop_original_drag_scroll(timer, &intent);
                return Ok(());
            }
            let pointer = self.drag_scroll_pointer(&origin, point, intent.clone(), clicks)?;
            if self.hand_child_pointer(&origin, &pointer)? {
                return Ok(()); // 原活动格须先真实提交并移交，不能滚动已结束的局部输入图。
            }
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let request = match self.scene() {
                Ok(scene) if current() && scene.origin.state == origin.state => {
                    scene.key.request.clone()
                }
                Ok(_) => return Err(NativeMarkdownPagedTextViewError::Revoked),
                Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(()),
                Err(cause) => return Err(cause),
            }; // 归还真实 Scene 强边以后才滚动，旧 publication 不横跨 Native 几何换代。
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let changed = origin.delegate.drag_autoscroll(&request, point, &intent)?;
            if !changed {
                self.stop_original_drag_scroll(timer, &intent);
                return Ok(()); // 原clip的BOF/EOF约束或已回正文；新真实Drag可重新开始。
            }
            // Native滚动已换几何；重新借当前输入，但手势仍必须是同一个Down，不能借后继动作续权。
            let after = self.origin()?;
            if !self.cell_pointer_is_current(&intent)
                || !self.owns_drag_scroll(timer, &intent)
                || !self.drag_scroll_point_is_current(point, &intent)
                || !Rc::ptr_eq(&origin.input, &after.input)
                || origin.state != after.state
                || !after.input.target_domain()?.same(&request.target)
                || !after.current(self)
                || !self.cell_pointer_is_current(&intent)
                || !self.drag_scroll_point_is_current(point, &intent)
                || !self.owns_drag_scroll(timer, &intent)
            {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let pointer = self.drag_scroll_pointer(&after, point, intent.clone(), clicks)?;
            self.defer_pointer_location(&after, pointer)
        })();
        if let Err(cause) = result {
            if matches!(&cause, NativeMarkdownPagedTextViewError::Pending) {
                return; // 原首组/Scene还在准备，保同一期限等待，不能把尚未完成记成输入失败。
            }
            if self.drag_scroll_point_is_current(point, &intent) {
                self.stop_original_drag_scroll(timer, &intent);
            } // 同一手势的新 Drag 已替换终点时，旧 tick 只退出，不能停掉仍服务最新点的期限。
            if !matches!(
                &cause,
                NativeMarkdownPagedTextViewError::Closed
                    | NativeMarkdownPagedTextViewError::Revoked
            ) {
                self.record(Err(cause));
            }
        }
    }

    /// 连续期限只继承原Down已映射的caret/word种子；不从当前选择反推新手势或读取源字符。
    fn drag_scroll_pointer(
        &self,
        origin: &Origin,
        point: NSPoint,
        intent: Weak<()>,
        clicks: usize,
    ) -> Result<NativeMarkdownPagedPointerLocation> {
        use commands::DragAnchor;
        let anchor = match self.ivars().drag_anchor.get() {
            Some(DragAnchor::Caret {
                anchor_utf16,
                generation,
            }) if generation == origin.state.generation => {
                Some(NativeMarkdownPagedCellDragAnchor::Caret { anchor_utf16 })
            }
            Some(DragAnchor::Word { anchor, generation })
                if generation == origin.state.generation =>
            {
                if matches!(anchor, NativeMarkdownPagedWordAnchor::Word { .. }) {
                    return Err(NativeMarkdownPagedTextViewError::Pending);
                } // 真正首组尚未交回不能先滚动；几何换代会撤原查询，也不能由裸 seed 重扩首词。
                Some(NativeMarkdownPagedCellDragAnchor::Word(anchor))
            }
            _ => return Err(NativeMarkdownPagedTextViewError::Pending),
        };
        Ok(NativeMarkdownPagedPointerLocation {
            window_point: point,
            clicks, // 三击段落与双击词均保真实 Down 的粒度，不能由 Caret/Word 槽反推点击次数。
            extending: true,
            anchor,
            dragging: true,
            intent,
            mouse_up: false,
        })
    }

    /// Up/取消先接走旧期限，撤完原手势元数据后才 Drop；析构不能再访问后继 owner 的槽。
    pub(super) fn take_drag_scroll(&self) -> Option<DragScroll> {
        self.ivars().drag_scroll.borrow_mut().take()
    }

    /// 焦点或菜单等显式用户边界结束当下期限；旧 tick 不走这个会取整槽的入口。
    pub(super) fn stop_drag_scroll(&self) {
        drop(self.take_drag_scroll());
    }

    fn owns_drag_scroll(&self, timer: &NSTimer, intent: &Weak<()>) -> bool {
        self.ivars()
            .drag_scroll
            .borrow()
            .as_ref()
            .is_some_and(|active| {
                std::ptr::eq::<NSTimer>(&*active.timer, timer)
                    && Weak::ptr_eq(&active.intent, intent)
            })
    }

    /// 原滚动副作用逐步核最后一个有限终点；重入的新 Drag 不因 Input 恰好未变化而被旧点覆盖。
    pub(in crate::native_surface) fn drag_scroll_point_is_current(
        &self,
        point: NSPoint,
        intent: &Weak<()>,
    ) -> bool {
        self.ivars()
            .drag_scroll
            .borrow()
            .as_ref()
            .is_some_and(|active| active.point == point && Weak::ptr_eq(&active.intent, intent))
    }

    /// 原事件在正文内或候选中结束自己的期限；不能触碰 Native 重入后另一次 Down 的 timer。
    fn stop_drag_scroll_intent(&self, intent: &Weak<()>) {
        let old = {
            let mut slot = self.ivars().drag_scroll.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|active| Weak::ptr_eq(&active.intent, intent))
            {
                slot.take()
            } else {
                None
            }
        };
        drop(old);
    }

    /// 每个回调只能拿走它自己的 timer 和手势；Native 副作用后的失败不获准停止新期限。
    fn stop_original_drag_scroll(&self, timer: &NSTimer, intent: &Weak<()>) {
        let old = {
            let mut slot = self.ivars().drag_scroll.borrow_mut();
            if slot.as_ref().is_some_and(|active| {
                std::ptr::eq::<NSTimer>(&*active.timer, timer)
                    && Weak::ptr_eq(&active.intent, intent)
            }) {
                slot.take()
            } else {
                None
            }
        };
        drop(old);
    }

    /// child提交后只移交原最新窗口点；目标仍核同一实际手势，不保旧输入或移交系统事件。
    pub(super) fn transfer_drag_scroll_to(&self, parent: &NativeMarkdownPagedTextView) {
        let original = self.take_drag_scroll();
        let target = original
            .as_ref()
            .map(|active| (active.point, active.intent.clone(), active.clicks));
        drop(original); // 原 child 已交 Rc；invalidate 可能重入，之后仍只准同一个弱身份继续。
        if let Some((point, intent, clicks)) = target {
            let result = parent.start_drag_scroll(point, &intent, clicks);
            if !matches!(
                &result,
                Err(NativeMarkdownPagedTextViewError::Closed
                    | NativeMarkdownPagedTextViewError::Revoked)
            ) {
                parent.record(result);
            }
        }
    }

    /// 原严格关闭观察实际存在的期限；弱票不延长timer、view或手势寿命。
    pub(in crate::native_surface) fn drag_scroll_weak(&self) -> Option<NativeWeak<AnyObject>> {
        let timer = self
            .ivars()
            .drag_scroll
            .borrow()
            .as_ref()
            .map(|active| active.timer.clone());
        timer.map(|timer| NativeWeak::from(<NSTimer as AsRef<AnyObject>>::as_ref(&timer)))
    }
}
