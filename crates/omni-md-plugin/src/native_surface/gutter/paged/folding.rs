//! Fold markers and gutter hit regions consume the same published Scene without parsing hidden text during paint or hit-testing.

use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedTextScene;

impl NativeMarkdownPagedGutter {
    /// Any visible soft line resolves its actual logical line; offscreen rows and the Find header acquire no hidden hit region.
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
        } // Carries the same Scene epoch into the original Input chain; successor text cannot inherit this old line number.
        Ok(())
    }
}

/// Draws the established right/down fold triangle into the existing narrow CPU bitmap, without fonts, strings, or window display lists.
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
        // A collapsed block points right, preserving the existing expansion affordance.
        CGContext::move_to_point(Some(context), x - 2.0, y - 3.0);
        CGContext::add_line_to_point(Some(context), x + 2.0, y);
        CGContext::add_line_to_point(Some(context), x - 2.0, y + 3.0);
    } else {
        // An expanded foldable block points down; this changes marker ink only, not fold authority.
        CGContext::move_to_point(Some(context), x - 3.0, y + 2.0);
        CGContext::add_line_to_point(Some(context), x + 3.0, y + 2.0);
        CGContext::add_line_to_point(Some(context), x, y - 2.0);
    }
    CGContext::close_path(Some(context));
    CGContext::fill_path(Some(context));
}
