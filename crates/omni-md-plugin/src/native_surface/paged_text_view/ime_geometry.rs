//! 系统组字会话的插入点几何交接；只保纯坐标，不延长旧 Scene、Source、字体或像素寿命。
//!
//! 输入法在 setMarkedText 返回后立即查询候选位置，早于异步正文的新排版。此时它需要
//! 本次组合开始的真实插入点，而不是新字形的 Ready 凭证。普通命中与绘制仍只认现行 Scene。

use super::*;

#[derive(Clone, Copy)]
pub(super) struct CompositionAnchor {
    state: NativeMarkdownPagedInputState,
    geometry_epoch: u64,
    location: usize,
    rect: NSRect,
}

impl NativeMarkdownPagedTextView {
    /// 在真正改动 Core 前取得原组合起点；续写仅可沿同客户、同几何、同完整输入延续。
    /// 捕获结束即归还 Scene 强边，Core 提交之后不再访问旧排版或旧文字。
    pub(super) fn capture_composition_anchor(&self, origin: &Origin) -> Option<CompositionAnchor> {
        let location = origin
            .state
            .marked
            .unwrap_or(origin.state.primary_range)
            .location;
        let previous = self.ivars().composition_anchor.get().filter(|anchor| {
            anchor.state == origin.state
                && anchor.geometry_epoch == origin.geometry_epoch
                && anchor.location == location
                && origin.current(self)
        });
        // 首选这次已经实际发布的布局；已有组合的合法在途更新才可交接先前同一插入点。
        // 几何不是文本输入的前置条件：原客户仍可接纳输入，但无真实锚点时不能签坐标成功。
        let scene = match self.scene_for_origin(origin.clone()) {
            Ok(scene) => scene,
            Err(NativeMarkdownPagedTextViewError::Pending) => return previous,
            Err(_) => return None,
        };
        self.composition_anchor_from_scene(&scene, location)
    }

    fn composition_anchor_from_scene(
        &self,
        scene: &SceneUse,
        location: usize,
    ) -> Option<CompositionAnchor> {
        let range = NSRange::new(location, 0);
        self.range_in_source(scene, range).ok()?;
        let value = scene.scene.first_rect(range).ok()?;
        if value.actual_range != range
            || !finite_rect(value.rect, false)
            || value.rect.size.height <= 0.0
            || !scene.current(self)
        {
            return None;
        }
        Some(CompositionAnchor {
            state: scene.origin.state,
            geometry_epoch: scene.origin.geometry_epoch,
            location,
            rect: value.rect,
        })
    }

    /// 只有这笔 Core 提交确实替换原选择/原组字范围，才把原插入点移交给该提交的 marked 状态。
    /// 其他位置替换、折叠变化、换客户或滚动都结束交接，不能把同坐标用于别的编辑。
    pub(super) fn bind_composition_anchor(
        &self,
        origin: &Origin,
        commit: &NativeMarkdownPagedInputCommit,
        anchor: Option<CompositionAnchor>,
    ) {
        self.ivars().composition_anchor.set(None);
        let (Some(mut anchor), Some(marked)) = (anchor, commit.state.marked) else {
            return;
        };
        let replaced = origin.state.marked.unwrap_or(origin.state.primary_range);
        let Some(end) = replaced.location.checked_add(replaced.length) else {
            return;
        };
        let same_range = commit
            .invalidation
            .replaced_utf16_range
            .as_ref()
            .is_some_and(|range| *range == (replaced.location..end));
        let same_provisional =
            !commit.invalidation.characters_changed && origin.state.marked == commit.state.marked;
        if (!same_range && !same_provisional)
            || commit.invalidation.folds_changed
            || anchor.location != replaced.location
            || marked.location != anchor.location
            || self.ivars().geometry_epoch.get() != anchor.geometry_epoch
            || self.ivars().geometry_failed.get()
            || self.ivars().closed.get()
            || self
                .ivars()
                .input
                .borrow()
                .as_ref()
                .is_none_or(|input| input.state().ok() != Some(commit.state))
        {
            return;
        }
        // 仅保存这次已提交 Input 的纯值；后继普通命令会因完整状态不符撤销此锚点。
        anchor.state = commit.state;
        self.ivars().composition_anchor.set(Some(anchor));
    }

    /// 任何不属于原组字交接的提交都撤销锚点；不让旧组合跨 Undo、Find 或客户切换继续存活。
    pub(super) fn retire_other_composition_anchor(&self, state: NativeMarkdownPagedInputState) {
        if self
            .ivars()
            .composition_anchor
            .get()
            .is_some_and(|anchor| anchor.state != state || state.marked.is_none())
        {
            self.ivars().composition_anchor.set(None);
        }
    }

    /// 新布局真正发布后更新组字起点；不强制绘图，也不以这个纯坐标证明 CPU 已完成。
    pub(super) fn refresh_composition_anchor(&self, scene: &SceneUse) {
        let anchor = scene
            .origin
            .state
            .marked
            .and_then(|marked| self.composition_anchor_from_scene(scene, marked.location));
        if scene.current(self) {
            self.ivars().composition_anchor.set(anchor);
        }
    }

    /// 仅供系统组字候选使用的真实插入点；actualRange 明确为空光标，不伪称已排好新字形。
    /// 当前 marked 之外的范围、真正滚动/缩放或另一笔输入均不能借此绕过 Pending。
    pub(super) fn composition_screen_rect(
        &self,
        range: NSRange,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        let origin = self.origin()?;
        let marked = origin
            .state
            .marked
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let anchor = self
            .ivars()
            .composition_anchor
            .get()
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        let marked_end = marked
            .location
            .checked_add(marked.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        if anchor.state != origin.state
            || anchor.geometry_epoch != origin.geometry_epoch
            || anchor.location != marked.location
            || range.location < marked.location
            || end > marked_end
        {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        }
        let window = origin
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let base = origin.step(self, || self.convertRect_toView(anchor.rect, None))?;
        let rect = origin.step(self, || window.convertRectToScreen(base))?;
        if !finite_rect(rect, false) || rect.size.height <= 0.0 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(NativeMarkdownPagedCharacterRect {
            rect,
            actual_range: NSRange::new(anchor.location, 0),
        })
    }
}
