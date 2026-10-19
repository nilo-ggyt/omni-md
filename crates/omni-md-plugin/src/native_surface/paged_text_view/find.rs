//! Find只消费已Ready的同票首位置；已选中和真正进入可见区是两个独立事实。

use super::*;

/// 只能由实际View认证构造；弱Key不保活scene、Source、文档或原生视图。
pub struct NativeMarkdownPagedFindVisibleProof {
    key: NativeMarkdownPagedSceneKey,
    range: NSRange,
    first_rect: NSRect,
}
impl NativeMarkdownPagedFindVisibleProof {
    #[must_use]
    pub fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }
    #[must_use]
    pub fn range(&self) -> NSRange {
        self.range
    }
    #[must_use]
    pub fn first_rect(&self) -> NSRect {
        self.first_rect
    }
}

impl NativeMarkdownPagedTextView {
    /// 只取现成Ready scene的原key；不布局、不滚动，也不为旧请求挑选一个后继nonce。
    ///
    /// # Errors
    /// 尚无Ready、原Source或输入/几何失权时直接返回对应错误。
    pub fn capture_ready_scene_key(&self) -> Result<NativeMarkdownPagedSceneKey> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(scene.key.clone())
    }

    /// 扫描visitor可持有Doc只读借用；这里严格不调用Native、delegate或可重入current。
    /// None允许原Reveal有授权的离屏滚动，最终可见性仍必须用完整Key与私有proof确认。
    #[must_use]
    pub fn find_input_is_current(
        &self,
        input: &Rc<NativeMarkdownPagedInput>,
        epoch: u64,
        expected: &NativeMarkdownPagedInputState,
        geometry_epoch: Option<u64>,
    ) -> bool {
        if self.ivars().closed.get()
            || self.ivars().epoch != epoch
            || self.ivars().geometry_failed.get()
            || geometry_epoch.is_some_and(|epoch| self.ivars().geometry_epoch.get() != epoch)
        {
            return false;
        }
        let owns_input = self.ivars().input.try_borrow().is_ok_and(|owner| {
            owner
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, input))
        });
        owns_input && input.state().is_ok_and(|actual| actual == *expected)
    }

    /// 只返回当前纯几何代次，不读取frame/bounds或触发任何Native布局。
    #[must_use]
    pub fn find_geometry_epoch(&self) -> Option<u64> {
        (!self.ivars().closed.get() && !self.ivars().geometry_failed.get())
            .then(|| self.ivars().geometry_epoch.get())
    }

    /// Core已经提交的Find操作沿原输入owner通知已有显示调度，不重签或重放文本事务。
    ///
    /// # Errors
    /// View关闭、owner/原状态不符或显示后果失权均保留CommittedEffect，不能解释为正文未修改。
    pub fn apply_find_commit(
        &self,
        input: &Rc<NativeMarkdownPagedInput>,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = (|| -> Result<Origin> {
            let origin = self.origin()?;
            if !Rc::ptr_eq(&origin.input, input) || origin.state != commit.state {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            commit.check_current(&|epoch| origin.session_current(self, epoch))?;
            Ok(origin)
        })()
        .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))?;
        self.finish_commit_ref(&origin, commit)
    }

    /// 仅同一个实际Ready Key的首scalar或插入点进入真实clip，才产生Find可见证明。
    ///
    /// 本方法不排版、不发起Reveal、不滚动、不重试。离屏或尚无scene时保持Pending，
    /// 原匹配后面的某一可见行不能替代原首位置；裁切只决定可见部分，不改用户源范围。
    ///
    /// # Errors
    /// 原票失效、范围无效或首位置尚未可见时拒绝，不把当前选择、非空缓存当作定位成功。
    pub fn verify_find_target_visible(
        &self,
        expected: &NativeMarkdownPagedSceneKey,
        range: NSRange,
    ) -> Result<NativeMarkdownPagedFindVisibleProof> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        if !expected.same(&scene.key) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        self.range_in_source(&scene, range)?;
        let first_end = if range.length == 0 {
            range.location
        } else {
            let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
            let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
            let mut units = [0u16; 1];
            scene.origin.input.copy_utf16_code_units(
                &permit,
                range.location,
                &mut units,
                &current,
            )?;
            let unit = units[0];
            range
                .location
                .checked_add(if (0xd800..=0xdbff).contains(&unit) {
                    2
                } else {
                    1
                })
                .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?
        }; // 原目标只借一个真实UTF16单元；借用先结束，下面scene/Native方法才能安全重入。
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let first = scene.scene.first_rect(range)?;
        self.range_in_source(&scene, first.actual_range)?;
        let actual_end = first
            .actual_range
            .location
            .checked_add(first.actual_range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        if first.actual_range.location > range.location || actual_end < first_end {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        } // 不能用同一match内的后继可见行、表格后面的文字或虚拟EOF充当首glyph。
        if !finite_rect(first.rect, false)
            || first.rect.size.height <= 0.0
            || !finite_endpoints(first.rect)
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let visible = scene.step(self, || self.visibleRect())?;
        let bounds = scene.step(self, || self.bounds())?;
        let visible = bounded_viewport(visible, bounds)
            .filter(|visible| *visible == expected.request.viewport)
            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
        let first_rect = visible_first_rect(first.rect, visible)
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        if !scene.current(self) || !expected.same(&scene.key) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(NativeMarkdownPagedFindVisibleProof {
            key: scene.key.clone(),
            range,
            first_rect,
        })
    }
}

fn finite_endpoints(rect: NSRect) -> bool {
    (rect.origin.x + rect.size.width).is_finite() && (rect.origin.y + rect.size.height).is_finite()
}

/// 零宽caret仍是有效竖线；非空字形必须有正宽交集，边缘擦过不算真正可见。
fn visible_first_rect(first: NSRect, clip: NSRect) -> Option<NSRect> {
    if !finite_rect(clip, true) || !finite_endpoints(clip) {
        return None;
    }
    let x = first.origin.x.max(clip.origin.x);
    let y = first.origin.y.max(clip.origin.y);
    let right = (first.origin.x + first.size.width).min(clip.origin.x + clip.size.width);
    let top = (first.origin.y + first.size.height).min(clip.origin.y + clip.size.height);
    if top <= y
        || (first.size.width == 0.0 && (first.origin.x < clip.origin.x || first.origin.x > right))
        || (first.size.width != 0.0 && right <= x)
    {
        return None;
    }
    Some(NSRect::new(
        NSPoint::new(x, y),
        objc2_foundation::NSSize::new(right - x, top - y),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSSize;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    #[test]
    fn first_glyph_requires_real_intersection_while_edge_caret_remains_visible() {
        let clip = rect(10.0, 10.0, 100.0, 100.0);
        assert_eq!(
            visible_first_rect(rect(0.0, 50.0, 30.0, 20.0), clip),
            Some(rect(10.0, 50.0, 20.0, 20.0))
        );
        assert!(visible_first_rect(rect(0.0, 50.0, 10.0, 20.0), clip).is_none());
        for x in [10.0, 110.0] {
            assert_eq!(
                visible_first_rect(rect(x, 50.0, 0.0, 20.0), clip),
                Some(rect(x, 50.0, 0.0, 20.0))
            );
        } // 插入点允许贴住clip边界；没有宽度的caret不被误认成没有文本位置。
        for x in [9.0, 111.0] {
            assert!(visible_first_rect(rect(x, 50.0, 0.0, 20.0), clip).is_none());
        }
        assert!(visible_first_rect(rect(20.0, 0.0, 20.0, 10.0), clip).is_none());
        assert!(visible_first_rect(rect(20.0, 110.0, 20.0, 10.0), clip).is_none());
    }

    #[test]
    fn invalid_or_overflowing_clip_cannot_certify_a_position() {
        let first = rect(20.0, 20.0, 1.0, 18.0);
        for clip in [
            rect(0.0, 0.0, 0.0, 20.0),
            rect(0.0, 0.0, 20.0, -1.0),
            rect(f64::NAN, 0.0, 20.0, 20.0),
            rect(f64::MAX, 0.0, f64::MAX, 20.0),
        ] {
            assert!(visible_first_rect(first, clip).is_none());
        }
    }
}
