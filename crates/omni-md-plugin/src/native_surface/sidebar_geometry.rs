//! Keep search/footer fixed while the native middle list resizes; viewport height never changes the page budget.

use objc2_foundation::{NSPoint, NSRect, NSSize};

pub(super) const MINIMUM_CONTENT_HEIGHT: f64 = 600.0;
pub(super) const MINIMUM_CONTENT_WIDTH: f64 = 900.0;
/// The 58 pt identity/action row and 42 pt formatting row share one height used by body and Find layout.
pub(super) const HEADER_HEIGHT: f64 = 100.0;
/// Body feedback only; the sidebar separately owns its location label and paging controls.
pub(super) const STATUS_HEIGHT: f64 = 28.0;
pub(super) const ROW_HEIGHT: f64 = 30.0;
const LIST_TOP_INSET: f64 = 138.0;
const LIST_BOTTOM: f64 = 62.0;

/// Preserve Core's page budget, including the root row; scrolling neither scans extra entries nor reopens the cursor.
pub(super) const MINIMUM_PAGE_ROWS: usize = 13;

/// In the y-up content root, fixed edges keep the list clear of location/paging below and search/section labels above.
pub(super) fn list_frame(content_height: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(12.0, LIST_BOTTOM),
        NSSize::new(224.0, content_height - LIST_TOP_INSET - LIST_BOTTOM),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum-height scrolling must preserve both fixed areas without hiding the last Core row beneath the footer.
    #[test]
    fn scroll_view_separates_rows_from_search_and_fixed_footer() {
        assert_eq!(MINIMUM_PAGE_ROWS, 13);
        let frame = list_frame(MINIMUM_CONTENT_HEIGHT);
        // The location occupies y34..52 and section label y476..496; shrinking cannot overlap either fixed region.
        assert!(frame.origin.y > 52.0);
        assert!(frame.origin.y + frame.size.height < 476.0);
        // The minimum window requires actual scrolling, without hiding row 13 or silently shrinking the Core page.
        assert!(frame.size.height < MINIMUM_PAGE_ROWS as f64 * (ROW_HEIGHT + 2.0));
    }

    /// Resize changes list height only, leaving page admission and fixed top/bottom coordinates intact.
    #[test]
    fn resize_only_changes_scroll_view_height() {
        for height in [MINIMUM_CONTENT_HEIGHT, 720.0, 1024.0] {
            // Each supported height consumes the same fixed insets instead of altering scan or row limits.
            let frame = list_frame(height);
            assert_eq!(frame.origin.y, LIST_BOTTOM);
            assert_eq!(frame.size.width, 224.0);
            assert_eq!(height - frame.origin.y - frame.size.height, LIST_TOP_INSET);
            assert_eq!(MINIMUM_PAGE_ROWS, 13);
        }
    }
}
