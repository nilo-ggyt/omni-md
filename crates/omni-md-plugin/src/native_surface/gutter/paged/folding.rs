//! 行号折叠热区与标记只消费同一个已发布 Scene；不在绘制或命中时解析隐藏正文。

use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedTextScene;

impl NativeMarkdownPagedGutter {
    /// 原行号区任一可见软行都定位它的真实逻辑行；已出屏行和 Find 顶栏没有隐藏热区。
    pub(super) fn toggle_fold_at_event(&self, event: &NSEvent) -> Result<()> {
        let scene = self
            .ivars()
            .scene
            .borrow()
            .upgrade()
            .ok_or(Error::Pending)?;
        let body = self
            .native(&scene, || self.body_view())?
            .ok_or(Error::Revoked)?;
        let point = self.native(&scene, || {
            self.convertPoint_fromView(event.locationInWindow(), None)
        })?;
        let bounds = self.native(&scene, || self.bounds())?;
        if point.x < bounds.origin.x
            || point.x >= bounds.origin.x + bounds.size.width
            || point.y < bounds.origin.y
            || point.y >= bounds.origin.y + bounds.size.height
        {
            return Ok(());
        }
        let mut selected = None;
        for row in scene.gutter_rows()? {
            let frame =
                self.native(&scene, || self.convertRect_fromView(row.frame, Some(&body)))?;
            if frame.origin.y <= point.y && point.y < frame.origin.y + frame.size.height {
                selected = Some(row.logical_line);
                break;
            }
        }
        self.require(&scene)?;
        if let Some(line) = selected {
            body.toggle_source_fold_line(scene.key().clone(), line)?;
        } // 同一 Scene 的代次随点击传入原 Input 链；后继文本不能继承这个旧行号。
        Ok(())
    }
}

/// 与原右三角/下三角含义一致，直接进入既有窄 CPU 图，不增字体、字符串或窗口显示列表。
pub(super) fn draw_fold_marker(context: &CGContext, frame: NSRect, right: f64, folded: bool) {
    let x = right - 4.0;
    let y = frame.origin.y + frame.size.height * 0.5;
    CGContext::set_rgb_fill_color(
        Some(context),
        81.0 / 255.0,
        91.0 / 255.0,
        109.0 / 255.0,
        1.0,
    );
    CGContext::begin_path(Some(context));
    if folded {
        CGContext::move_to_point(Some(context), x - 2.0, y - 3.0);
        CGContext::add_line_to_point(Some(context), x + 2.0, y);
        CGContext::add_line_to_point(Some(context), x - 2.0, y + 3.0);
    } else {
        CGContext::move_to_point(Some(context), x - 3.0, y + 2.0);
        CGContext::add_line_to_point(Some(context), x + 3.0, y + 2.0);
        CGContext::add_line_to_point(Some(context), x, y - 2.0);
    }
    CGContext::close_path(Some(context));
    CGContext::fill_path(Some(context));
}
