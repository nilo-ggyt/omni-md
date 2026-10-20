//! 原Find请求只绑定一次正常Producer的Source；不以选择提交或旧Ready替代真正可见性。

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputOutcome;
use crate::native_surface::{
    NativeMarkdownController,
    find_bar::{
        MarkdownFindAction, NativeMarkdownFindBackend, NativeMarkdownFindBar,
        NativeMarkdownFindExecution, NativeMarkdownFindExecutionError,
        NativeMarkdownPagedFindBackend, NativeMarkdownPagedFindRevealPort,
        NativeMarkdownPagedFindRevealProgress, NativeMarkdownPagedFindRevealRequest,
        execute_find_backend,
    },
};
use target::SourceTargetRequest;

pub(super) struct PendingFind {
    request: Rc<NativeMarkdownPagedFindRevealRequest>,
    layout_width: f64,
    layout_origin_x: f64,
    bound: Cell<bool>,
    target: RefCell<Option<Rc<SourceTargetRequest>>>,
}

impl NativeMarkdownPagedFindRevealPort for NativeMarkdownPagedBody {
    fn cancel_pending(&self) {
        if let Err(cause) = self.cancel_pending_find() {
            self.fail(cause); // 原Source已失败时先交原失败；普通取消不能吞掉已存在的errno。
        }
    }

    fn begin(
        &self,
        request: Rc<NativeMarkdownPagedFindRevealRequest>,
    ) -> std::result::Result<NativeMarkdownPagedFindRevealProgress, String> {
        self.cancel_pending_find()
            .map_err(|cause| cause.to_string())?;
        self.find_source_health()
            .map_err(|cause| cause.to_string())?;
        if !self.current_body() || !request.is_current() {
            return Err("原Find请求已经失效".into());
        }
        let actual = self
            .view()
            .map_err(|cause| cause.to_string())?
            .capture_scene_request()
            .map_err(|cause| cause.to_string())?;
        if request.epoch() != self.configuration.epoch
            || request.input_state() != actual.input
            || !actual.target.is_body(&actual.document)
            || !Weak::ptr_eq(&request.document(), &actual.document)
            || !Weak::ptr_eq(&actual.document, &self.input.document_identity())
        {
            return Err("原Find请求不属于此正文".into());
        }
        self.find_source_health()
            .map_err(|cause| cause.to_string())?;
        let current = request.is_current() && self.current_body();
        self.find_source_health()
            .map_err(|cause| cause.to_string())?;
        if !current {
            return Err("原Find请求在接收时已经失效".into());
        }
        let pending = Rc::new(PendingFind {
            request,
            layout_width: actual.virtual_viewport.layout_width,
            layout_origin_x: actual.virtual_viewport.layout_origin_x,
            bound: Cell::new(false),
            target: RefCell::new(None),
        });
        // begin在apply_find_commit清Scene后被调用。这里只存原票与真实宽度，不要求旧Ready，
        // 也不捕Source/领取Reader；正常advance选择原Producer后才允许一次绑定。
        drop(self.pending_page_scroll.borrow_mut().take()); // 后来的Find显示意图接管视口，不让旧纯滚动跨其reveal复活。
        *self.pending_find.borrow_mut() = Some(pending.clone());
        if let Err(cause) = self.schedule() {
            self.retire_find(&pending, Some(cause.to_string()));
            return Err(cause.to_string());
        }
        Ok(NativeMarkdownPagedFindRevealProgress::Pending)
    }
}

impl NativeMarkdownPagedBody {
    /// 查找读取正文前结束当前真实单格；同次提交后的输入票仍须有效，不能借后继选区继续旧动作。
    pub(in crate::native_surface) fn finish_cell_before_find(
        &self,
        controller: &NativeMarkdownController,
        intent: u64,
    ) -> Result<()> {
        if self.active_cell_body().is_none() {
            return Ok(()); // 普通正文查找不承担单格提交义务，不因正在绘图而额外拒绝原查找入口。
        }
        let Some(committed) = self.commit_active_cell()? else {
            return Err(ViewError::Revoked.into()); // 原草稿只能完成或失权，不能在提交边界消失后捕获后继输入。
        };
        let current = |epoch| {
            self.current(epoch)
                && self.active_cell_body().is_none()
                && controller.find_input_intent() == intent
                && crate::native_surface::native_markdown_controller_has_foreground_authority(
                    controller,
                )
        };
        // detach会调用原生焦点入口；这些入口重入后必须继续核原after，不能重新捕获当前选择补权。
        let result = match committed.outcome {
            NativeMarkdownPagedInputOutcome::Committed(after) => {
                after.check_current(&current).map_err(ViewError::Input)
            }
            NativeMarkdownPagedInputOutcome::Unchanged(after) => {
                if current(self.configuration.epoch)
                    && self.input.state().is_ok_and(|state| state == after)
                {
                    Ok(())
                } else {
                    Err(ViewError::Revoked)
                }
            }
        };
        // 草稿已经封闭；后续失权不得被调用方当成未提交而重试同一编辑。
        result.map_err(|cause| ViewError::CommittedEffect(Box::new(cause)).into())
    }

    /// 实际composition root可借原Body的唯一Input执行既有后端；不暴露第二Input owner，
    /// 也不改变旧默认Find入口、中文按钮或命令。参数只在本同步动作中借用。
    pub(in crate::native_surface) fn execute_paged_find_action(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
        intent: u64,
        action: MarkdownFindAction,
    ) -> std::result::Result<NativeMarkdownFindExecution, NativeMarkdownFindExecutionError> {
        // 既有查找栏可在编辑单格后继续使用；所有匹配/替换入口都读取本次真实提交后的正文。
        self.finish_cell_before_find(controller, intent)
            .map_err(|cause| NativeMarkdownFindExecutionError::Rejected(cause.to_string()))?;
        let view = self
            .view()
            .map_err(|cause| NativeMarkdownFindExecutionError::Rejected(cause.to_string()))?;
        execute_find_backend(
            bar,
            NativeMarkdownFindBackend::Paged(NativeMarkdownPagedFindBackend {
                view: &view,
                input: &self.input,
                epoch: self.configuration.epoch,
                controller,
                intent,
                reveal: self,
            }),
            action,
        )
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn probe_find_input_weak(
        &self,
    ) -> Weak<NativeMarkdownPagedInput> {
        Rc::downgrade(&self.input) // 只观察唯一原Input释放，不向测试交第二份Input强owner。
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn probe_bound_find_source(
        &self,
    ) -> Option<(
        Weak<NativeMarkdownPagedFindRevealRequest>,
        omni_markdown_core::MarkdownLayoutTextAuthority,
    )> {
        let pending = self.pending_find.borrow().clone()?;
        if !pending.bound.get() {
            return None;
        }
        let target = pending.target.borrow().clone()?;
        Some((Rc::downgrade(&pending.request), target.source.clone()))
    }

    fn find_slot_is(&self, pending: &Rc<PendingFind>) -> bool {
        self.pending_find
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, pending))
    }

    fn find_scope_current(&self, pending: &Rc<PendingFind>) -> bool {
        // 先结束槽借用再进原request的Controller/View门；Native重入不能抢到任何RefCell借用。
        self.current_body()
            && self.find_slot_is(pending)
            && pending.request.is_current()
            && self.find_slot_is(pending)
    }

    pub(super) fn find_source_health(&self) -> Result<()> {
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        }
        let document = self
            .document
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedBodyError::Closed)?;
        document
            .try_borrow()
            .map_err(|_| ViewError::Busy)?
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        Ok(()) // 只有原canonical零端点/原Source健康检查，无正文加载或Native调用。
    }

    fn retire_find(&self, pending: &Rc<PendingFind>, failure: Option<String>) {
        let removed = {
            let mut slot = self.pending_find.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, pending))
            {
                slot.take()
            } else {
                None // 旧回调可以撤自己的票，绝不能清已经安装的后继Find槽。
            }
        };
        drop(removed);
        if let Some(cause) = failure {
            pending.request.fail(cause);
        } else {
            pending.request.cancel();
        }
        let target = pending.target.borrow_mut().take();
        if let Some(target) = target {
            let removed = {
                let mut slot = self.source_target.borrow_mut();
                if slot
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, &target))
                {
                    slot.take()
                } else {
                    None
                }
            };
            drop(removed);
            target.revoke(); // 只清原Find的精确goal；不因相同range而夺走后继目标。
        }
    }

    pub(super) fn cancel_pending_find(&self) -> Result<()> {
        let pending = self.pending_find.borrow().clone();
        if let Some(pending) = pending {
            let health = self.find_source_health();
            if let Err(cause) = health {
                self.retire_find(&pending, Some(cause.to_string()));
                return Err(cause);
            }
            self.retire_find(&pending, None);
        }
        Ok(()) // 先发生的正常取消是终态；后来的错误不复活旧请求。
    }

    pub(super) fn fail_pending_find(&self, cause: &NativeMarkdownPagedBodyError) {
        let pending = self.pending_find.borrow().clone();
        if let Some(pending) = pending {
            self.retire_find(&pending, Some(cause.to_string()));
        }
    }

    pub(super) fn find_pending(&self) -> bool {
        self.pending_find.borrow().is_some()
    }

    pub(super) fn refresh_pending_find(
        &self,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        self.find_source_health()?;
        let current = self.find_scope_current(&pending)
            && actual.input == pending.request.input_state()
            && actual.virtual_viewport.layout_width == pending.layout_width
            && actual.virtual_viewport.layout_origin_x == pending.layout_origin_x
            && Weak::ptr_eq(&actual.document, &pending.request.document());
        self.find_source_health()?;
        let goal_revoked = pending.bound.get()
            && pending
                .target
                .borrow()
                .as_ref()
                .is_none_or(|target| !target.current(self));
        if !current || goal_revoked {
            self.retire_find(&pending, None);
        }
        Ok(())
    }

    pub(super) fn bind_pending_find(
        &self,
        producer: &Producer,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        producer.target_source_health()?;
        if pending.bound.get() {
            return Ok(()); // 已绑定目标绝不领取后继Source，即使旧票后来失效也只走撤权。
        }
        let original_serial = self.serial.get();
        let source = producer.target_source_authority(actual, original_serial)?;
        let scope = || {
            self.find_scope_current(&pending)
                && self.serial.get() == original_serial
                && actual.virtual_viewport.layout_width == pending.layout_width
                && actual.virtual_viewport.layout_origin_x == pending.layout_origin_x
                && actual.input == pending.request.input_state()
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|request| request.same(actual))
                })
                && self.find_scope_current(&pending)
        };
        let result = self.admit_source_target(actual, source, pending.request.range(), &scope);
        producer.target_source_health()?; // 原Producer在栈中；Native回调后的首错不能被查询取消吞掉。
        let current = scope();
        producer.target_source_health()?;
        let target = match result {
            Ok(target) if current => target,
            Ok(target) => {
                target.revoke();
                self.retire_find(&pending, None);
                return Ok(());
            }
            Err(cause) if cause.is_foreground_revocation() => {
                self.retire_find(&pending, None);
                return Ok(()); // 真编辑/resize仍由原PreparationTicket决定后继Source，不能重绑旧Find。
            }
            Err(cause) => return Err(cause),
        };
        producer.target_source_health()?;
        if !self.find_slot_is(&pending) || pending.bound.replace(true) {
            target.revoke();
            return Ok(());
        }
        *pending.target.borrow_mut() = Some(target.clone());
        self.install_source_target(target)?;
        Ok(())
    }

    /// SourceTarget的纯current不能进入Native；只有稳定目标的Native检查才核原Find意图。
    pub(super) fn find_source_target_current(&self, target: &SourceTargetRequest) -> bool {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return true;
        };
        let owns = pending
            .target
            .borrow()
            .as_ref()
            .is_some_and(|actual| std::ptr::eq::<SourceTargetRequest>(&**actual, target));
        !owns || self.find_scope_current(&pending)
    }

    pub(super) fn cancel_stale_find_target(&self, target: &SourceTargetRequest) -> Result<bool> {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        let owns = pending
            .target
            .borrow()
            .as_ref()
            .is_some_and(|actual| std::ptr::eq::<SourceTargetRequest>(&**actual, target));
        if !owns || self.find_scope_current(&pending) {
            return Ok(false);
        }
        self.find_source_health()?;
        self.retire_find(&pending, None);
        Ok(true) // 只表示原Find意图取消；调用方仍须独立认证PreparationTicket和原Source。
    }

    pub(super) fn finish_pending_find(&self) -> Result<()> {
        let pending = self.pending_find.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        let target = pending.target.borrow().clone();
        if target.as_ref().is_some_and(|target| target.pending()) {
            // 唯一调用点advance已依次执行同回合refresh_pending_find、producer.advance和
            // finish_source_target；原Source错误已沿Result进入fail_pending_find，仍在途时
            // 没有新proof可消费。取消/输入/宽度仍由前述原票门撤权，不借后继Source续票。
            return Ok(());
        }
        self.find_source_health()?;
        let current = self.find_scope_current(&pending);
        self.find_source_health()?;
        if !current {
            self.retire_find(&pending, None);
            return Ok(());
        }
        let Some(target) = target else {
            return Ok(());
        };
        let proof = self.take_source_target_request_visible(&target);
        self.find_source_health()?;
        let proof = match proof {
            Ok(Some(proof)) => proof,
            Ok(None) => return Err(ViewError::InvalidRange.into()), // 完成票的proof已消费不是重试许可。
            Err(cause) if cause.is_foreground_revocation() => {
                self.retire_find(&pending, None);
                return Ok(());
            }
            Err(cause) => return Err(cause),
        };
        let result = pending.request.finish_visible(&proof);
        self.find_source_health()?;
        if let Err(cause) = result {
            let current = self.find_scope_current(&pending);
            self.find_source_health()?;
            if !current {
                self.retire_find(&pending, None);
            } else {
                self.retire_find(&pending, Some(cause));
            }
            return Ok(());
        }
        // 完成终态不再满足request.is_current；只移除精确原槽，保原完成收据供调用方读取。
        let mut slot = self.pending_find.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, &pending))
        {
            drop(slot.take());
        }
        Ok(())
    }
}
