//! 查找栏的查询/替换/选项分行；最小内容宽度由窗口与侧栏的同一预算推导。

use super::super::{NATIVE_MARKDOWN_SIDEBAR_WIDTH, sidebar_geometry::MINIMUM_CONTENT_WIDTH};

/// 只表示自有控件的点单位frame，不含文本、Native对象或回调权限。
#[derive(Clone, Copy, Debug)]
pub(super) struct FindControlFrame {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

/// 两个模式沿同一固定控制顺序；替换模式只是多出一行，不重建输入或选项。
pub(super) struct FindFrames {
    pub(super) query: FindControlFrame,
    pub(super) replacement: FindControlFrame,
    pub(super) buttons: [FindControlFrame; 6],
    pub(super) options: [FindControlFrame; 3],
    pub(super) status: FindControlFrame,
}

/// 活Rc和相同布局代次不能替代原前台权限；关闭/交接发生后旧setter序列必须停止。
pub(super) fn find_layout_is_current(
    expected_revision: u64,
    current_revision: u64,
    visible: bool,
    closed: bool,
    owner_current: bool,
) -> bool {
    expected_revision == current_revision && visible && !closed && owner_current
}

/// 高度只由显式替换模式决定，show/hide各自精确加减同一值。
pub(super) const fn find_bar_height(replace: bool) -> f64 {
    if replace {
        // 查询、替换、选项/结果三行。
        108.0
    } else {
        // 查询、选项/结果两行，不为隐藏的替换字段保留空白。
        76.0
    }
}

/// 最小651pt正文区仍给查询383pt，固定动作与选项不以max硬顶到栏外。
pub(super) fn find_frames(width: f64, replace: bool) -> Option<FindFrames> {
    let minimum = MINIMUM_CONTENT_WIDTH - NATIVE_MARKDOWN_SIDEBAR_WIDTH - 1.0;
    if !width.is_finite() || width < minimum {
        // 未挂载/非法尺寸不产生部分布局，更不改输入值或焦点。
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
            // 导航与关闭只占查询行；顺序保持上一个/下一个/全部选中/替换/全部替换/关闭。
            rect(nav, top, 56.0, 24.0),
            rect(nav + 62.0, top, 56.0, 24.0),
            rect(nav + 124.0, top, 76.0, 24.0),
            rect(nav, 44.0, 72.0, 24.0),
            rect(nav + 78.0, 44.0, 88.0, 24.0),
            rect(width - 36.0, top, 24.0, 24.0),
        ],
        options: [
            // 三项选项在末行左侧，结果在右侧，最小窗仍保持完整中文名称。
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
    /// 反例是旧Rc仍可见且revision未变，但setter同步关闭使owner失效，旧投影不能继续。
    fn live_bar_and_equal_revision_do_not_authorize_after_owner_or_close_changes() {
        assert!(find_layout_is_current(7, 7, true, false, true));
        assert!(!find_layout_is_current(7, 7, true, false, false));
        assert!(!find_layout_is_current(7, 7, true, true, true));
        assert!(!find_layout_is_current(7, 7, false, false, true));
        assert!(!find_layout_is_current(7, 8, true, false, true));
    }

    /// 所有实际可见矩形两两不相交，覆盖曾在最小窗挤出关闭按钮的回归输入。
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
                    // 隐藏模式不参与命中，替换模式必须完整包含第二行的三个控件。
                    visible.extend([frames.replacement, frames.buttons[3], frames.buttons[4]]);
                }
                for (index, a) in visible.iter().enumerate() {
                    assert!(a.width > 0.0 && a.height > 0.0);
                    assert!(a.x >= 12.0 && a.x + a.width <= width - 12.0);
                    assert!(a.y >= 0.0 && a.y + a.height <= find_bar_height(replace));
                    for b in &visible[index + 1..] {
                        // 不依赖控件顺序或同一组公式，直接验证矩形没有任何面积交集。
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
    fn invalid_or_unmounted_width_does_not_publish_a_partial_layout() {
        for width in [650.9, 0.0, -1.0, f64::NAN, f64::INFINITY] {
            // 若宽度未达到声明下限，保持上一完整投影，不以负值或NaN写入AppKit。
            assert!(find_frames(width, false).is_none());
        }
    }
}
