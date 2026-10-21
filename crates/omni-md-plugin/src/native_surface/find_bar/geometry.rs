//! Separate query/replacement/options rows using the same window/sidebar budget to derive minimum body width.

use super::super::{NATIVE_MARKDOWN_SIDEBAR_WIDTH, sidebar_geometry::MINIMUM_CONTENT_WIDTH};

/// Point-unit geometry for owned controls only, with no text, native handle, or callback authority.
#[derive(Clone, Copy, Debug)]
pub(super) struct FindControlFrame {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

/// Both modes share a fixed control order; replacement adds one row without rebuilding input or options.
pub(super) struct FindFrames {
    pub(super) query: FindControlFrame,
    pub(super) replacement: FindControlFrame,
    pub(super) buttons: [FindControlFrame; 6],
    pub(super) options: [FindControlFrame; 3],
    pub(super) status: FindControlFrame,
}

/// A live Rc and equal layout generation cannot replace original foreground authority; close/handoff revokes old setters.
pub(super) fn find_layout_is_current(
    expected_revision: u64,
    current_revision: u64,
    visible: bool,
    closed: bool,
    owner_current: bool,
) -> bool {
    expected_revision == current_revision && visible && !closed && owner_current
}

/// Explicit replacement mode determines height; show/hide use the same exact occupied value.
pub(super) const fn find_bar_height(replace: bool) -> f64 {
    if replace {
        // Three rows hold query, replacement, and options/results.
        108.0
    } else {
        // Two rows hold query and options/results, without blank space for a hidden replacement field.
        76.0
    }
}

/// The minimum 651 pt body leaves a 383 pt query field, keeping fixed controls inside the bar without forced minimum overflow.
pub(super) fn find_frames(width: f64, replace: bool) -> Option<FindFrames> {
    let minimum = MINIMUM_CONTENT_WIDTH - NATIVE_MARKDOWN_SIDEBAR_WIDTH - 1.0;
    if !width.is_finite() || width < minimum {
        // Unmounted/invalid dimensions publish no partial layout and never change input values or focus.
        return None;
    }
    let rect = |x, y, width, height| FindControlFrame {
        x,
        y,
        width,
        height,
    };
    let top = find_bar_height(replace) - 32.0;
    let query_width = width - 268.0;
    let nav = query_width + 24.0;
    Some(FindFrames {
        query: rect(12.0, top, query_width, 24.0),
        replacement: rect(12.0, 44.0, query_width, 24.0),
        buttons: [
            // Navigation/close occupy the query row; order remains previous/next/select-all/replace/replace-all/hide.
            rect(nav, top, 56.0, 24.0),
            rect(nav + 62.0, top, 56.0, 24.0),
            rect(nav + 124.0, top, 76.0, 24.0),
            rect(nav, 44.0, 72.0, 24.0),
            rect(nav + 78.0, 44.0, 88.0, 24.0),
            rect(width - 36.0, top, 24.0, 24.0),
        ],
        options: [
            // Three options occupy the last row's left side and results the right, preserving full localized names at minimum width.
            rect(12.0, 12.0, 104.0, 20.0),
            rect(124.0, 12.0, 108.0, 20.0),
            rect(240.0, 12.0, 88.0, 20.0),
        ],
        status: rect(344.0, 12.0, width - 356.0, 20.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Even visible old Rc/equal revision cannot authorize continued layout after setter reentry revokes its owner.
    fn live_bar_and_equal_revision_do_not_authorize_after_owner_or_close_changes() {
        assert!(find_layout_is_current(7, 7, true, false, true));
        assert!(!find_layout_is_current(7, 7, true, false, false));
        assert!(!find_layout_is_current(7, 7, true, true, true));
        assert!(!find_layout_is_current(7, 7, false, false, true));
        assert!(!find_layout_is_current(7, 8, true, false, true));
    }

    /// Every visible rectangle stays disjoint, including the minimum-width regression that previously displaced the hide control.
    #[test]
    fn both_modes_fit_all_visible_controls_without_overlap_at_minimum_and_large_widths() {
        for width in [651.0, 651.5, 871.0, 1_351.0, 3_847.0] {
            for replace in [false, true] {
                let frames = find_frames(width, replace).unwrap();
                let mut visible = vec![frames.query, frames.status];
                visible.extend(frames.options);
                visible.extend([
                    frames.buttons[0],
                    frames.buttons[1],
                    frames.buttons[2],
                    frames.buttons[5],
                ]);
                if replace {
                    // Hidden replacement controls are excluded; replacement mode must fit all three second-row controls.
                    visible.extend([frames.replacement, frames.buttons[3], frames.buttons[4]]);
                }
                for (index, a) in visible.iter().enumerate() {
                    assert!(a.width > 0.0 && a.height > 0.0);
                    assert!(a.x >= 12.0 && a.x + a.width <= width - 12.0);
                    assert!(a.y >= 0.0 && a.y + a.height <= find_bar_height(replace));
                    for b in &visible[index + 1..] {
                        // Check actual rectangle separation independently of ordering or the construction formulas.
                        assert!(
                            a.x + a.width <= b.x
                                || b.x + b.width <= a.x
                                || a.y + a.height <= b.y
                                || b.y + b.height <= a.y
                        );
                    }
                }
                assert!(frames.query.width >= 380.0);
                assert!(frames.status.width >= 290.0);
            }
        }
    }

    #[test]
    /// Invalid dimensions preserve the last complete projection instead of sending partial frames to AppKit.
    fn invalid_or_unmounted_width_does_not_publish_a_partial_layout() {
        for width in [650.9, 0.0, -1.0, f64::NAN, f64::INFINITY] {
            // Unsupported width keeps the previous complete projection, without writing negative or NaN geometry to AppKit.
            assert!(find_frames(width, false).is_none());
        }
    }
}
