//! 搜索与底栏固定，只有中间原生列表随窗口高度伸缩；翻页预算不随视口变化。

use objc2_foundation::{NSPoint, NSRect, NSSize};

pub(super) const MINIMUM_CONTENT_HEIGHT: f64 = 600.0;
pub(super) const MINIMUM_CONTENT_WIDTH: f64 = 900.0;
/// 原顶部58pt身份/操作区与42pt格式栏共用这一总高度，正文和Find只沿同一来源扣除。
pub(super) const HEADER_HEIGHT: f64 = 100.0;
/// 仅正文底部的反馈区，侧栏仍有自己的位置标签和分页控件。
pub(super) const STATUS_HEIGHT: f64 = 28.0;
pub(super) const ROW_HEIGHT: f64 = 30.0;
const LIST_TOP_INSET: f64 = 138.0;
const LIST_BOTTOM: f64 = 62.0;

/// 保留既有Core页预算（目录包含root）；列表滚动不多扫描一项，也不重开分页游标。
pub(super) const MINIMUM_PAGE_ROWS: usize = 13;

/// 内容根为y-up；固定底边避开位置/翻页区，固定顶边避开搜索与分区标题。
pub(super) fn list_frame(content_height: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(12.0, LIST_BOTTOM),
        NSSize::new(224.0, content_height - LIST_TOP_INSET - LIST_BOTTOM),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_view_separates_rows_from_search_and_fixed_footer() {
        assert_eq!(MINIMUM_PAGE_ROWS, 13);
        let frame = list_frame(MINIMUM_CONTENT_HEIGHT);
        // 保存位置y34..52，分区标题y476..496；列表不能借缩窗覆盖任一固定区。
        assert!(frame.origin.y > 52.0);
        assert!(frame.origin.y + frame.size.height < 476.0);
        // 最小窗真实需要滚动，不能把第13项藏到底栏下面或偷偷缩减Core页。
        assert!(frame.size.height < MINIMUM_PAGE_ROWS as f64 * (ROW_HEIGHT + 2.0));
    }

    #[test]
    fn resize_only_changes_scroll_view_height() {
        for height in [MINIMUM_CONTENT_HEIGHT, 720.0, 1024.0] {
            let frame = list_frame(height);
            assert_eq!(frame.origin.y, LIST_BOTTOM);
            assert_eq!(frame.size.width, 224.0);
            assert_eq!(height - frame.origin.y - frame.size.height, LIST_TOP_INSET);
            assert_eq!(MINIMUM_PAGE_ROWS, 13);
        }
    }
}
