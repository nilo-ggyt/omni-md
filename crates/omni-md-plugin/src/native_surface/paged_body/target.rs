//! Source目标仅是同一Body/Cell输入域的一次原始意图；实际Y和可见证明均由真实生产路径签发。

use super::*;
use crate::native_surface::{
    paged_input::{NativeMarkdownPagedInputState, NativeMarkdownPagedTargetDomain},
    paged_text_view::NativeMarkdownPagedFindVisibleProof,
};
use objc2_foundation::NSRange;
use omni_markdown_core::MarkdownLayoutTextAuthority;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Terminal {
    Pending,
    Visible,
    Revoked,
}

/// 只保固定请求和弱身份；调用方不能构造实际位置、Source或可见证明。
pub struct NativeMarkdownPagedBodySourceTarget(Rc<SourceTargetRequest>);

impl NativeMarkdownPagedBodySourceTarget {
    /// 仅报告原请求状态；完成/撤销后不会把空proof槽误报成在途工作。
    pub fn is_pending(&self) -> bool {
        self.0.terminal.get() == Terminal::Pending
    }
}

pub(super) struct SourceTargetRequest {
    owner: Weak<NativeMarkdownPagedBody>,
    pub(super) source: MarkdownLayoutTextAuthority,
    document: Weak<RefCell<MarkdownDocumentBuffer>>,
    domain: NativeMarkdownPagedTargetDomain,
    state: NativeMarkdownPagedInputState,
    serial: u64,
    layout_epoch: u64,
    bounds_width: f64,
    pub(super) range: NSRange,
    pub(super) first_end: usize,
    terminal: Cell<Terminal>,
    proof: RefCell<Option<NativeMarkdownPagedFindVisibleProof>>,
}

impl SourceTargetRequest {
    pub(super) fn revoke(&self) {
        self.terminal.set(Terminal::Revoked);
        drop(self.proof.borrow_mut().take());
    }
    pub(super) fn pending(&self) -> bool {
        self.terminal.get() == Terminal::Pending
    }
    pub(super) fn matches(
        &self,
        owner: &NativeMarkdownPagedBody,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> bool {
        self.current(owner)
            && self.domain.same(&actual.target)
            && Weak::ptr_eq(&self.document, &actual.document)
            && actual.input == self.state
            && actual.bounds_width == self.bounds_width
    }
    pub(super) fn current(&self, owner: &NativeMarkdownPagedBody) -> bool {
        self.terminal.get() != Terminal::Revoked
            && owner.current_body()
            && Weak::ptr_eq(&self.owner, &owner.weak)
            && owner.serial.get() == self.serial
            && owner.layout_epoch.get() == self.layout_epoch
            && Weak::ptr_eq(&self.document, &owner.input.document_identity())
            && owner
                .input
                .target_domain()
                .is_ok_and(|actual| self.domain.same(&actual))
            && owner.input.state().is_ok_and(|state| state == self.state)
    }
    pub(super) fn current_native(&self, owner: &NativeMarkdownPagedBody) -> bool {
        if !self.current(owner) || !owner.find_source_target_current(self) {
            return false;
        }
        // 稳定目标允许自身滚动；实际Native宽度仍在每个可重入几何动作前后核原值。
        let matches = owner
            .view()
            .and_then(|view| view.capture_scene_request().map_err(Into::into))
            .is_ok_and(|actual| self.matches(owner, &actual));
        matches && self.current(owner) && owner.find_source_target_current(self)
    }
}

impl NativeMarkdownPagedBody {
    /// 捕获当前真实Ready输入域的一次目标意图，不变更选择，也不授予裸Range实际位置权限。
    /// 尚无首屏时明确Pending；本显式候选不替代正式Find Bar或导航命令。
    pub fn request_source_target(
        &self,
        range: NSRange,
    ) -> Result<NativeMarkdownPagedBodySourceTarget> {
        if self.preparing.get() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let view = self.view()?;
        let key = view.capture_ready_scene_key()?;
        let source = {
            let producer = self.producer.borrow();
            let producer = producer.as_ref().ok_or(ViewError::Pending)?;
            if !producer.owns_target_source(&key) {
                return Err(ViewError::Revoked.into());
            }
            producer.target_source_authority(&key.request, self.serial.get())?
        };
        let request =
            self.admit_source_target(&key.request, source, range, &|| self.scene_current(&key))?;
        self.install_source_target(request.clone())?;
        Ok(NativeMarkdownPagedBodySourceTarget(request))
    }

    /// 只有原Producer给出的同Source身份和原调用scope可以领取目标。没有Ready也能用于
    /// 已提交Find的正常后继Source；本入口不捕Source、不领取Reader、不授予可见性。
    pub(super) fn admit_source_target(
        &self,
        actual: &NativeMarkdownPagedSceneRequest,
        source: MarkdownLayoutTextAuthority,
        range: NSRange,
        scope: &impl Fn() -> bool,
    ) -> Result<Rc<SourceTargetRequest>> {
        let state = actual.input;
        let serial = self.serial.get();
        let layout_epoch = self.layout_epoch.get();
        let domain = self.input.target_domain().map_err(ViewError::Input)?;
        if !domain.same(&actual.target)
            || !Weak::ptr_eq(&actual.document, &self.input.document_identity())
        {
            return Err(ViewError::Revoked.into());
        }
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(ViewError::InvalidRange)?;
        if end > state.utf16_len {
            return Err(ViewError::InvalidRange.into());
        }
        let current = |epoch| {
            epoch == self.configuration.epoch
                && self.current_body()
                && self.serial.get() == serial
                && self.layout_epoch.get() == layout_epoch
                && self
                    .input
                    .target_domain()
                    .is_ok_and(|actual| domain.same(&actual))
                && self.input.state().is_ok_and(|value| value == state)
                && scope()
        };
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::Input)?;
        let mut first_end = range.location;
        if range.location < state.utf16_len {
            let mut units = [0u16; 2];
            self.input
                .copy_utf16_code_units(&permit, range.location, &mut units[..1], &current)
                .map_err(ViewError::Input)?;
            if (0xdc00..=0xdfff).contains(&units[0]) {
                return Err(ViewError::InvalidRange.into());
            }
            let width = if (0xd800..=0xdbff).contains(&units[0]) {
                // 只补读原首scalar的低代理项；不得为长匹配读取或shape整个源范围。
                self.input
                    .copy_utf16_code_units(&permit, range.location + 1, &mut units[1..], &current)
                    .map_err(ViewError::Input)?;
                if !(0xdc00..=0xdfff).contains(&units[1]) {
                    return Err(ViewError::InvalidRange.into());
                }
                2
            } else {
                1
            };
            if range.length != 0 {
                first_end = range
                    .location
                    .checked_add(width)
                    .ok_or(ViewError::InvalidRange)?;
                if first_end > end {
                    return Err(ViewError::InvalidRange.into());
                }
            }
        }
        let accepted = current(self.configuration.epoch);
        // Cell原scope先核Source首错，再区分普通内容撤权；不从canonical Weak给局部坐标续权。
        let actual_domain = self.input.target_domain().map_err(ViewError::Input)?;
        if !accepted || !domain.same(&actual_domain) {
            return Err(ViewError::Revoked.into());
        }
        Ok(Rc::new(SourceTargetRequest {
            owner: self.weak.clone(),
            source,
            document: actual.document.clone(),
            domain,
            state,
            serial,
            layout_epoch,
            bounds_width: actual.bounds_width,
            range,
            first_end,
            terminal: Cell::new(Terminal::Pending),
            proof: RefCell::new(None),
        }))
    }

    pub(super) fn install_source_target(&self, request: Rc<SourceTargetRequest>) -> Result<()> {
        if !request.current(self) {
            return Err(ViewError::Revoked.into());
        }
        drop(self.pending_page_scroll.borrow_mut().take()); // 只有已认证的新Source目标才撤旧滚动，拒绝的外来请求不能取消当前动作。
        let old = self.source_target.replace(Some(request.clone()));
        if let Some(old) = old {
            old.revoke();
        }
        drop(self.scene.borrow_mut().take()); // CPU旧完整帧保留，旧业务Reader先归还再推进新prefix。
        if let Err(cause) = self.schedule() {
            request.revoke();
            return Err(cause);
        }
        Ok(())
    }

    /// 单次取走原View已签发的可见证明，不补发已消费的proof。
    /// 返回None且is_pending为true表示原续体尚未完成；返回None且is_pending为false
    /// 表示原proof已经消费，不能据此重发目标或继续等待。撤权/关闭仍返回对应错误。
    pub fn take_source_target_visible(
        &self,
        target: &NativeMarkdownPagedBodySourceTarget,
    ) -> Result<Option<NativeMarkdownPagedFindVisibleProof>> {
        self.take_source_target_request_visible(&target.0)
    }

    pub(super) fn take_source_target_request_visible(
        &self,
        target: &Rc<SourceTargetRequest>,
    ) -> Result<Option<NativeMarkdownPagedFindVisibleProof>> {
        if self.input.is_cell() {
            // 原Cell事实先核共享scope/原映射首错；正常内容换代不读取已过期的旧reader。
            let actual = self.input.target_domain().map_err(ViewError::Input)?;
            if actual.same(&target.domain)
                && let Some(producer) = self.producer.borrow().as_ref()
            {
                producer.target_source_health()?;
            } // 仅借已有Producer，不capture新Source，也不为已换内容的旧目标补签。
        } else {
            // Body仍核原canonical零端点，Find的正文入口和数据域不因Cell接线而放开。
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
        }
        if !target.current_native(self) {
            target.revoke();
            return Err(ViewError::Revoked.into());
        }
        let proof = target.proof.borrow_mut().take();
        if let Some(proof) = &proof {
            self.view()?
                .verify_find_target_visible(proof.key(), target.range)?;
            if !target.current_native(self) {
                target.revoke();
                return Err(ViewError::Revoked.into());
            }
        }
        Ok(proof)
    }

    pub(super) fn source_target_pending(&self) -> bool {
        self.source_target
            .borrow()
            .as_ref()
            .is_some_and(|request| request.pending())
    }
    pub(super) fn refresh_source_target(&self, actual: &NativeMarkdownPagedSceneRequest) {
        let target = self.source_target.borrow().clone();
        if let Some(target) = target
            && !target.matches(self, actual)
        {
            target.revoke();
        }
    }
    pub(super) fn revoke_source_target(&self) {
        let target = self.source_target.borrow_mut().take();
        if let Some(target) = target {
            target.revoke();
        }
    }
    pub(super) fn finish_source_target(&self) -> Result<()> {
        let target = self.source_target.borrow().clone();
        let scene = self.scene.borrow().clone();
        let (Some(target), Some(scene)) = (target, scene) else {
            return Ok(());
        };
        if !target.pending() || !target.matches(self, &scene.key().request) {
            return Ok(());
        }
        if target.source != scene.key().source {
            target.revoke();
            return Ok(());
        }
        let result = self
            .view()?
            .verify_find_target_visible(scene.key(), target.range);
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // 原Source首错高于verify期间的普通前台撤权。
        }
        if !target.current_native(self) {
            target.revoke();
            self.schedule()?;
            return Ok(()); // 实际编辑/宽度已换代只撤此旧目标；后继显示沿已有原续体前进。
        }
        match result {
            Ok(proof) => {
                if !target.current_native(self) {
                    target.revoke();
                    return Ok(());
                }
                *target.proof.borrow_mut() = Some(proof);
                target.terminal.set(Terminal::Visible);
            }
            Err(ViewError::Pending) => {
                return Err(ViewError::Pending.into());
            }
            Err(cause) => return Err(cause.into()),
        }
        Ok(())
    }
}
