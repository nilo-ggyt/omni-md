//! 唯一输入事务与有界协议读取；所有Core借用在NSInputContext或几何操作前结束。

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass};
use objc2_foundation::{NSAttributedString, NSPoint, NSRange, NSString};

use super::{
    NativeMarkdownPagedCharacterRect, NativeMarkdownPagedInputCommit,
    NativeMarkdownPagedInputOutcome, NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
    Origin, Result, SceneUse, bounded_viewport, finite_rect,
};

const SUBSTRING_UNITS: usize = 4096;

impl NativeMarkdownPagedTextView {
    /// 延后导航只能通知已提交的原收据；此处新origin仅供显示副作用，不重签导航before或位置。
    pub(in crate::native_surface) fn finish_navigation_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let origin = self
            .origin()
            .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))?;
        commit
            .check_current(&|epoch| origin.session_current(self, epoch))
            .map_err(|cause| {
                NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause.into()))
            })?;
        self.finish_commit_ref(&origin, commit)?;
        origin
            .delegate
            .reveal_committed_selection(commit)
            .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))
    }

    pub(super) fn apply_native_input(
        &self,
        text: &AnyObject,
        replacement: NSRange,
        selected: Option<NSRange>,
    ) -> Result<()> {
        let origin = self.origin()?;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if selected.is_none()
            && replacement.location == objc2_foundation::NSNotFound as usize
            && origin.state.marked.is_none()
            && origin.delegate.defer_insert(text, &permit)?
        {
            return Ok(()); // 只表示按真实顺序接纳；不得立即在删除前的旧caret插字或报告已经提交。
        }
        // 显式替换仍按原范围立即接管。marked也保原同步协议，不能排队后向系统谎报旧hasMarked；
        // pending Word后开始组合的顺序尚未接通，不能据普通输入保序宣称IME已完成。
        let commit = match selected {
            Some(selected) => {
                origin
                    .input
                    .set_marked_native(&permit, text, selected, replacement, &current)?
            }
            None => origin
                .input
                .insert_native(&permit, text, replacement, &current)?,
        };
        self.finish_edit_commit(&origin, commit)
    }

    pub(super) fn apply_unmark(&self) -> Result<()> {
        let origin = self.origin()?;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let outcome = origin.input.unmark(&permit, &current)?;
        self.finish_edit_outcome(&origin, outcome)
    }

    pub(super) fn finish_outcome(
        &self,
        origin: &Origin,
        outcome: NativeMarkdownPagedInputOutcome,
    ) -> Result<()> {
        match outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                self.finish_commit_ref(origin, &commit)
            }
            NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                if state != origin.state || !origin.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Ok(()) // 真无历史/无marked/相同选择不通知显示，也不制造一笔编辑。
            }
        }
    }

    /// 字符输入/组合/删除/历史动作才请求Cell活动端可见；普通程序选区仍只发布原收据。
    pub(super) fn finish_edit_outcome(
        &self,
        origin: &Origin,
        outcome: NativeMarkdownPagedInputOutcome,
    ) -> Result<()> {
        match outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                self.finish_edit_commit(origin, commit)
            }
            unchanged => self.finish_outcome(origin, unchanged), // 无变化不新建定位意图或显示帧。
        }
    }

    /// 同一真实编辑收据先完成Native通知，再让原Cell的唯一Producer定位postcommit caret。
    pub(super) fn finish_edit_commit(
        &self,
        origin: &Origin,
        commit: NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        self.finish_commit_ref(origin, &commit)?;
        if origin.input.is_cell() {
            origin
                .delegate
                .reveal_committed_selection(&commit)
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                })?;
        } // Body原行为保持；Cell显示失败仍明确是已提交后果，绝不重放输入或重捕后继票。
        Ok(())
    }

    /// Find延后定位仍保同一收据；显示通知只借原Commit，不移动后再重签。
    pub(super) fn finish_commit_ref(
        &self,
        origin: &Origin,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let result = (|| {
            let current = |epoch| origin.session_current(self, epoch);
            commit.check_current(&current)?;
            let cpu = self.ivars().cpu.borrow().clone();
            if let Some(cpu) = cpu {
                cpu.cancel_candidate();
            } // 已提交状态撤销旧候选；借用归还后才drop真实Native context。
            commit.check_current(&current)?; // 释放回调可重入；失权仍是CommittedEffect，不能重放Core事务。
            origin.delegate.committed(commit)?;
            commit.check_current(&current)?;
            // 唯一已提交receipt授权通知；不使用旧before，也不重捕后继g/selection补签。
            let geometry = self.ivars().geometry_epoch.get();
            let visible = self.visibleRect();
            commit.check_current(&current)?;
            if geometry != self.ivars().geometry_epoch.get() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let bounds = self.bounds();
            commit.check_current(&current)?;
            if geometry != self.ivars().geometry_epoch.get() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if let Some(visible) = bounded_viewport(visible, bounds) {
                self.setNeedsDisplayInRect(visible);
                commit.check_current(&current)?;
                if geometry != self.ivars().geometry_epoch.get() {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
            } // 尚未挂窗没有实际正面积视口；保留已接受canonical输入，不伪造绘制Ready。
            let context = self
                .ivars()
                .context
                .borrow()
                .clone()
                .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
            commit.check_current(&current)?;
            context.0.invalidateCharacterCoordinates();
            commit.check_current(&current)?;
            Ok(())
        })();
        // 到这里Core已经发布；错误只能停止显示后果，不能让调用方误认正文仍是旧值。
        result.map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))
    }

    pub(super) fn substring(
        &self,
        range: NSRange,
    ) -> Result<Option<(Retained<NSAttributedString>, NSRange)>> {
        let origin = self.origin()?;
        if range.location == objc2_foundation::NSNotFound as usize
            || range.location > origin.state.utf16_len
        {
            return Ok(None);
        }
        let requested_end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        let mut start = range.location;
        let mut end = requested_end
            .min(origin.state.utf16_len)
            .min(start.saturating_add(SUBSTRING_UNITS));
        let mut units = [0u16; SUBSTRING_UNITS];
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let mut unit = [0u16; 1];
        // 同原许可有限读实际UTF16；Cell不新增Snapshot，Body仍复用原Doc窗口。
        if start < origin.state.utf16_len {
            origin
                .input
                .copy_utf16_code_units(&permit, start, &mut unit, &current)?;
            if (0xdc00..=0xdfff).contains(&unit[0]) {
                start = start
                    .checked_sub(1)
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
            }
        }
        end = end.min(start.saturating_add(SUBSTRING_UNITS));
        if end > start && end < origin.state.utf16_len {
            origin
                .input
                .copy_utf16_code_units(&permit, end, &mut unit, &current)?;
            if (0xdc00..=0xdfff).contains(&unit[0]) {
                end -= 1;
            }
        }
        origin
            .input
            .copy_utf16_code_units(&permit, start, &mut units[..end - start], &current)?;
        // 所有Doc/胶囊借用已归还；Foundation初始化只能持固定数组与同一原目标。
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: 固定数组覆盖完整有限length；init同步复制UTF16，指针在整个调用内有效。
        let string = origin.step(self, || unsafe {
            NSString::initWithCharacters_length(
                NSString::alloc(),
                NonNull::from(&mut units[0]),
                end - start,
            )
        })?;
        let value = origin.step(self, || {
            NSAttributedString::initWithString(NSAttributedString::alloc(), &string)
        })?;
        Ok(Some((value, NSRange::new(start, end - start))))
    }

    pub(super) fn range_in_source(&self, scene: &SceneUse, range: NSRange) -> Result<()> {
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        if end > scene.origin.state.utf16_len {
            return Err(NativeMarkdownPagedTextViewError::InvalidRange);
        }
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        scene
            .origin
            .input
            .validate_utf16_range(&permit, range.location..end, &current)?;
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    /// 只有实际Ready scene提供的首行/插入点才转换screen坐标；Pending不制造候选窗锚点。
    pub fn screen_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect> {
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        self.range_in_source(&scene, range)?;
        let mut value = scene.scene.first_rect(range)?;
        if !scene.current(self) || !finite_rect(value.rect, false) || value.rect.size.height <= 0.0
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        self.range_in_source(&scene, value.actual_range)?;
        let window = scene
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let base = scene.step(self, || self.convertRect_toView(value.rect, None))?;
        value.rect = scene.step(self, || window.convertRectToScreen(base))?;
        if !finite_rect(value.rect, false) {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(value)
    }

    /// NSTextInputClient点为screen坐标，不能把window或bitmap坐标直接冒充正文命中。
    pub fn screen_hit(&self, point: NSPoint) -> Result<usize> {
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let scene = self.scene()?;
        let window = scene
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let base = scene.step(self, || window.convertPointFromScreen(point))?;
        let local = scene.step(self, || self.convertPoint_fromView(base, None))?;
        let index = scene.scene.hit(local)?;
        self.range_in_source(&scene, NSRange::new(index, 0))?;
        Ok(index)
    }
}
