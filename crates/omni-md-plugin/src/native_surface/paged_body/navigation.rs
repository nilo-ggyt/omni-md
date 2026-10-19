//! 文档头尾的已提交选择只保存一次原请求；Source和实际Y继续归唯一Producer与SourceTarget。

use super::super::{
    paged_input::NativeMarkdownVerticalGoal,
    paged_text_view::{
        NativeMarkdownPagedNavigation as Nav, NativeMarkdownPagedNavigationContext,
        NativeMarkdownPagedNavigationGeometry,
    },
};
use super::*;
use objc2_foundation::NSRange;

#[derive(Clone, Copy, PartialEq)]
/// 原caret所属行的实际文档坐标；不保存Scene/Reader，也不使用平均行高。
pub(super) struct VerticalSeed {
    pub ordinal: usize,
    pub top: f64,
    pub bottom: f64,
    pub caret_y: f64,
    pub x: f64,
    pub goal: Option<NativeMarkdownVerticalGoal>,
}

/// 原输入尚未提交的一串真实垂直动作；只存方向段/有限选择，失权后不能补签后继Scene。
pub(super) struct PendingVertical {
    pub request: NativeMarkdownPagedSceneRequest,
    pub geometry: NativeMarkdownPagedNavigationGeometry,
    pub serial: u64,
    pub seed: Cell<VerticalSeed>,
    steps: RefCell<VecDeque<VerticalRun>>,
    selection: Cell<omni_markdown_core::MarkdownTextSelection>,
    processing: Cell<bool>,
    pub resolved: Cell<Option<(usize, f64, f64)>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct VerticalMotion {
    pub action: Nav,
    pub extend: bool,
}

struct VerticalRun {
    motion: VerticalMotion,
    remaining: usize,
}

impl PendingVertical {
    /// 原队列只读当前一条；同方向次数不展开成事件或正文缓存。
    pub(super) fn motion(&self) -> Option<VerticalMotion> {
        self.steps.borrow().front().map(|step| step.motion)
    }

    fn append(&self, motion: VerticalMotion) -> std::result::Result<(), ViewError> {
        if self.processing.get() {
            return Err(ViewError::Busy); // 原步正在Native计算或最终提交，不能接受先后位置尚未确定的重入动作。
        }
        let mut steps = self.steps.borrow_mut();
        if let Some(last) = steps.back_mut().filter(|last| last.motion == motion) {
            let remaining = last
                .remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            last.remaining = remaining; // 溢出先拒绝，原已接受次数不变；混合Shift或反向绝不压错。
        } else {
            if steps.len() == 32 {
                return Err(ViewError::Busy); // 只限制未消费方向段，长按同方向不因次数32被截断。
            }
            steps.push_back(VerticalRun {
                motion,
                remaining: 1,
            });
        }
        Ok(())
    }

    /// 先预看紧接的一步，才能按其是否保目标列从本次真实caret生成seed。
    fn next_motion(&self) -> Option<VerticalMotion> {
        let steps = self.steps.borrow();
        let first = steps.front()?;
        if first.remaining > 1 {
            Some(first.motion)
        } else {
            steps.get(1).map(|next| next.motion)
        }
    }

    /// 仅在原Scene已命中且原票仍有效后消费一步；BOF/EOF也逐次执行真实clamp。
    fn consume_motion(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let first = steps.front_mut().ok_or(ViewError::Revoked)?;
        first.remaining -= 1;
        if first.remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    pub(super) fn current(&self, owner: &NativeMarkdownPagedBody) -> bool {
        owner.current_body()
            && owner.serial.get() == self.serial
            && owner.layout_epoch.get() == self.geometry.layout_epoch
            && Weak::ptr_eq(&self.request.document, &owner.input.document_identity())
            && owner
                .input
                .state()
                .is_ok_and(|state| state == self.request.input)
            && owner
                .pending_vertical
                .borrow()
                .as_ref()
                .is_some_and(|pending| std::ptr::eq(&**pending, self))
    }
    pub(super) fn matches(
        &self,
        owner: &NativeMarkdownPagedBody,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> bool {
        self.current(owner)
            && actual.target.is_body(&actual.document)
            && Weak::ptr_eq(&actual.document, &self.request.document)
            && actual.input == self.request.input
            && actual.bounds_width == self.request.bounds_width
            && actual.viewport.size == self.request.viewport.size
    }
    pub(super) fn current_native(&self, owner: &NativeMarkdownPagedBody) -> bool {
        self.current(owner)
            && owner.view().is_ok_and(|view| {
                view.capture_scene_request()
                    .is_ok_and(|actual| self.matches(owner, &actual))
            })
            && self.current(owner)
    }
}

impl NativeMarkdownPagedBody {
    /// 同一输入在可见窗口直接完成；真正离屏时才登记一个具体续体，不先改选择。
    pub(super) fn accept_vertical_navigation(
        &self,
        actual: &NativeMarkdownPagedSceneRequest,
        action: Nav,
        extend: bool,
    ) -> std::result::Result<(), ViewError> {
        if !matches!(action, Nav::Up | Nav::Down | Nav::PageUp | Nav::PageDown) {
            return Err(ViewError::UnsupportedCommand); // 该入口仅接四个垂直动作，其他语义仍走各自已有命令。
        }
        actual
            .input
            .directed_selection()
            .ok_or(ViewError::Pending)?;
        if !self.current_body()
            || !actual.target.is_body(&actual.document)
            || !Weak::ptr_eq(&actual.document, &self.input.document_identity())
            || self.input.state()? != actual.input
        {
            return Err(ViewError::Revoked);
        }
        let scene = self.scene.borrow().clone();
        let previous = self.pending_vertical.borrow().clone();
        if let Some(previous) = previous
            .as_ref()
            .filter(|pending| pending.matches(self, actual))
        {
            previous.append(VerticalMotion { action, extend })?;
            return self.schedule().map_err(|_| ViewError::Revoked); // 不换原Rc/seed，后一步必须从前一步实际结果接续。
        }
        let (geometry, seed, immediate) = if let Some(scene) = scene
            .filter(|scene| scene.key().request.same(actual) && self.scene_current(scene.key()))
        {
            let geometry = scene.navigation_geometry()?;
            let current = |epoch| {
                self.current(epoch)
                    && self.scene_current(scene.key())
                    && self.input.state().is_ok_and(|state| state == actual.input)
            };
            let permit = self.input.capture(self.configuration.epoch, &current)?;
            let (upstream, goal) = self.input.navigation_selection_facts(&permit, &current)?;
            let context = NativeMarkdownPagedNavigationContext {
                geometry: geometry.clone(),
                head_affinity: upstream.map(
                    super::super::paged_text_view::NativeMarkdownPagedHeadAffinity::from_upstream,
                ),
                vertical_goal: goal.filter(|goal| {
                    action.preserves_vertical_goal() && geometry.permits_goal(*goal)
                }),
                bidi_side: self.input.navigation_bidi_side(&permit, &current)?,
            };
            let seed = scene.vertical_seed(
                actual.input.selection.head_utf16,
                &context,
                action.preserves_vertical_goal(),
            )?;
            let immediate = match scene.navigate(action, extend, &context) {
                Ok(target) => Some(target),
                Err(ViewError::Pending | ViewError::InvalidRange) => None, // 原caret真实，缺的是邻行/页面；不得拿当前窗口边缘代替目标。
                Err(cause) => return Err(cause),
            };
            if !current(self.configuration.epoch) {
                return Err(ViewError::Revoked);
            }
            (geometry, seed, immediate)
        } else {
            return Err(ViewError::Pending);
        };
        drop(self.pending_vertical.borrow_mut().take()); // 只归还已失效的旧域；同域动作已在上面保序追加。
        drop(self.pending_page_scroll.borrow_mut().take()); // 新相对选择接管视口，旧纯滚动不能稍后抢回显示。
        self.revoke_source_target();
        drop(self.selection_reveal.borrow_mut().take());
        let pending = Rc::new(PendingVertical {
            request: actual.clone(),
            geometry,
            serial: self.serial.get(),
            seed: Cell::new(seed),
            steps: RefCell::new(VecDeque::from([VerticalRun {
                motion: VerticalMotion { action, extend },
                remaining: 1,
            }])),
            selection: Cell::new(actual.input.selection),
            processing: Cell::new(false),
            resolved: Cell::new(None),
        });
        self.pending_vertical.replace(Some(pending.clone()));
        if let Some(target) = immediate {
            pending.processing.set(true);
            let result = self.commit_vertical_navigation(&pending, target);
            pending.processing.set(false);
            return result;
        }
        self.schedule().map_err(|_| ViewError::Revoked)
    }

    pub(super) fn refresh_vertical_navigation(&self, actual: &NativeMarkdownPagedSceneRequest) {
        let pending = self.pending_vertical.borrow().clone();
        if pending
            .as_ref()
            .is_some_and(|pending| !pending.matches(self, actual))
        {
            drop(self.pending_vertical.borrow_mut().take()); // 编辑、鼠标选择、宽度或原owner改变即撤销，不能留下永久Pending。
        }
    }

    /// Producer已取得真实目标行并滚动，只有同一新Scene可命中原目标列并提交选择。
    pub(super) fn finish_vertical_navigation(&self) -> Result<()> {
        let pending = self.pending_vertical.borrow().clone();
        let scene = self.scene.borrow().clone();
        let (Some(pending), Some(scene)) = (pending, scene) else {
            return Ok(());
        };
        if !pending.matches(self, &scene.key().request)
            || pending.geometry.source != scene.key().source
        {
            return Ok(());
        }
        let Some((ordinal, _, _)) = pending.resolved.get() else {
            return Ok(());
        };
        if pending.processing.replace(true) {
            return Err(ViewError::Busy.into());
        }
        let result = (|| -> Result<()> {
            let motion = pending.motion().ok_or(ViewError::Revoked)?;
            let seed = pending.seed.get();
            let target = scene.vertical_line_target(
                ordinal,
                seed.x,
                pending.selection.get(),
                motion.extend,
                seed.goal,
            )?;
            let next = pending.next_motion();
            let next_seed = if let Some(next) = next {
                let context = NativeMarkdownPagedNavigationContext {
                    geometry: pending.geometry.clone(),
                    head_affinity: target.head_affinity,
                    vertical_goal: target
                        .vertical_goal
                        .filter(|_| next.action.preserves_vertical_goal()),
                    bidi_side: target.bidi_side,
                };
                Some(scene.vertical_seed(
                    target.selection.head_utf16,
                    &context,
                    next.action.preserves_vertical_goal(),
                )?)
            } else {
                None
            }; // 下一步只从本步真实caret/行框起算，不把原before或净行数当作中间结果。
            if !pending.current_native(self) {
                return Err(ViewError::Revoked.into());
            }
            pending.consume_motion()?;
            if let Some(seed) = next_seed {
                pending.selection.set(target.selection); // 仅待提交标量，不创建第二Input或改canonical原before。
                pending.seed.set(seed);
                pending.resolved.set(None);
                let old = self.scene.borrow_mut().take();
                drop(old); // 中间命中不是整串完成帧；旧CPU候选失权，继续同一Producer排下一真实行。
                return self.schedule();
            }
            self.commit_vertical_navigation(&pending, target)?;
            Ok(())
        })();
        pending.processing.set(false);
        result
    }

    fn commit_vertical_navigation(
        &self,
        pending: &Rc<PendingVertical>,
        target: super::super::paged_text_view::NativeMarkdownPagedNavigationTarget,
    ) -> std::result::Result<(), ViewError> {
        let current = |epoch| self.current(epoch) && pending.current_native(self);
        let permit = self.input.capture(self.configuration.epoch, &current)?;
        let outcome = self.input.set_selection_intent(
            &permit,
            target.selection,
            target
                .head_affinity
                .map(super::super::paged_text_view::NativeMarkdownPagedHeadAffinity::is_upstream),
            target.vertical_goal,
            &current,
        )?;
        let removed = {
            let mut slot = self.pending_vertical.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, pending))
            {
                slot.take()
            } else {
                None
            }
        };
        drop(removed); // 选择已提交后不再要求旧before；只有原commit可通知显示，不能重放动作。
        match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => self
                .view()
                .map_err(|_| ViewError::Closed)?
                .finish_navigation_commit(&commit),
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => Ok(()),
        }
    }

    /// 只保存这次已提交选择的实际弱请求；没有Ready时也不另捕Source或读取离屏正文。
    pub(super) fn accept_selection_reveal(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        commit.check_current(&|epoch| self.current(epoch))?;
        let request = self
            .view()
            .map_err(|_| ViewError::Closed)?
            .capture_scene_request()?;
        commit.check_current(&|epoch| self.current(epoch))?;
        if request.input != commit.state
            || !self.input.target_domain()?.same(&request.target)
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
            || request.input.directed_selection().is_none()
        {
            return Err(ViewError::Revoked);
        } // 只接同一Body/Cell完整postcommit状态；Cell局部坐标仍由原domain约束，不借canonical身份。
        let old = self.selection_reveal.replace(Some(request));
        drop(old); // 只归还原弱请求，不保留旧Native事件、Source或历史副本。
        self.schedule().map_err(|_| ViewError::Revoked)
    }

    /// 唯一Producer选定后才绑定一次原Source；原请求失效只撤销，不将后继状态补签为旧导航。
    pub(super) fn bind_selection_reveal(
        &self,
        producer: &Producer,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self.selection_reveal.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        producer.target_source_health()?;
        if !pending.same(actual) {
            drop(self.selection_reveal.borrow_mut().take());
            return Ok(()); // 输入、鼠标或外部几何已接管；不捕后继请求继续旧意图。
        }
        let serial = self.serial.get();
        let source = producer.target_source_authority(actual, serial)?;
        let scope = || {
            let owns = self
                .selection_reveal
                .borrow()
                .as_ref()
                .is_some_and(|request| request.same(&pending));
            owns && self.current_body()
                && self.serial.get() == serial
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|request| request.same(&pending))
                })
                && self
                    .selection_reveal
                    .borrow()
                    .as_ref()
                    .is_some_and(|request| request.same(&pending))
        }; // 所有槽借用在Native getter前结束；后置核防重入替换正在消费的原意图。
        let result = self.admit_source_target(
            actual,
            source,
            NSRange::new(pending.input.selection.head_utf16, 0),
            &scope,
        );
        producer.target_source_health()?;
        let current = scope();
        producer.target_source_health()?; // 真源首错优先，普通输入/宽度撤权不能洗掉EIO。
        let target = match result {
            Ok(target) if current => target,
            Ok(target) => {
                target.revoke();
                return Ok(());
            }
            Err(cause) if cause.is_foreground_revocation() => return Ok(()),
            Err(cause) => return Err(cause),
        };
        let removed = {
            let mut slot = self.selection_reveal.borrow_mut();
            if slot.as_ref().is_some_and(|request| request.same(&pending)) {
                slot.take()
            } else {
                None
            }
        };
        if removed.is_none() {
            target.revoke();
            return Ok(()); // 旧回调不能取走后继槽，也不能将自己的目标安装到后继状态。
        }
        drop(removed);
        // EOF仍是零长度真实文档端点；原ActualTarget必须推进到complete后才可lookup最后行。
        self.install_source_target(target)
    }
}
