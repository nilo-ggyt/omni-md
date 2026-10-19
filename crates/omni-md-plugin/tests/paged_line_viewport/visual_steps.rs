//! 实际字体与L1/L2行图上的视觉动作；固定期望来自短混合串的字形顺序，不用被测返回反推。

use super::*;
use NativeMarkdownPagedHeadAffinity::{Downstream, Upstream};
use omni_md_plugin::native_surface::paged_input::NativeMarkdownBidiCaretSide;

pub(super) fn verify(
    window: &NativeMarkdownComposedViewport,
    output: &Rc<RefCell<MarkdownComposedLines>>,
    palette: &[Rc<NativeMarkdownLineFont>],
    text: &str,
    current: &dyn Fn() -> bool,
) {
    if text == "office אבג office" {
        assert_eq!(
            window
                .paragraph_base_level_at(7, Downstream, current)
                .unwrap(),
            0
        );
        for (right, start, expected) in [
            (
                true,
                6,
                [
                    (7, 0, false),
                    (9, 1, true),
                    (8, 1, true),
                    (10, 0, true),
                    (11, 0, false),
                ],
            ),
            (
                false,
                11,
                [
                    (10, 0, true),
                    (8, 1, false),
                    (9, 1, false),
                    (10, 1, false),
                    (6, 0, true),
                ],
            ),
        ] {
            let mut head = start;
            let mut affinity = Downstream;
            let mut side = None;
            let mut x = window
                .caret_geometry_with_side(head, affinity, side, current)
                .unwrap()
                .primary
                .origin
                .x;
            for (expected_head, level, logical_after) in expected {
                let target = window
                    .visual_neighbor(head, affinity, side, right, current)
                    .unwrap();
                assert_eq!(
                    (target.utf16, target.side.level, target.side.logical_after),
                    (expected_head, level, logical_after)
                );
                let caret = window
                    .caret_geometry_with_side(
                        target.utf16,
                        target.affinity,
                        Some(target.side),
                        current,
                    )
                    .unwrap();
                let ime = window
                    .first_rect_with_caret_side(
                        NSRange::new(target.utf16, 0),
                        target.affinity,
                        Some(target.side),
                        current,
                    )
                    .unwrap();
                assert_eq!(ime.rect, caret.primary);
                assert!(caret.secondary.is_none());
                assert!(
                    if right {
                        caret.primary.origin.x > x
                    } else {
                        caret.primary.origin.x < x
                    },
                    "实际向{right:?}视觉GCB必须沿同一真实行的相应x方向移动：{x}→{}",
                    caret.primary.origin.x
                );
                (head, affinity, side, x) = (
                    target.utf16,
                    target.affinity,
                    Some(target.side),
                    caret.primary.origin.x,
                );
            }
        }
        // 同一个logical 10有两个真实x，不能用soft-wrap侧替代其level/字素侧。
        let low = window
            .caret_geometry_with_side(
                10,
                Downstream,
                Some(NativeMarkdownBidiCaretSide {
                    level: 0,
                    logical_after: true,
                }),
                current,
            )
            .unwrap();
        let high = window
            .caret_geometry_with_side(
                10,
                Downstream,
                Some(NativeMarkdownBidiCaretSide {
                    level: 1,
                    logical_after: false,
                }),
                current,
            )
            .unwrap();
        assert_ne!(low.primary.origin.x, high.primary.origin.x);
        assert_eq!(low.line_ordinal, high.line_ordinal);
        assert!(matches!(
            window.visual_neighbor(
                7,
                Downstream,
                Some(NativeMarkdownBidiCaretSide {
                    level: 99,
                    logical_after: false
                }),
                true,
                current
            ),
            Err(Error::Pending)
        ));
        window.validate_ready(current).unwrap(); // 坏侧只拒本动作，不污染原Source或Reader。
    }
    if text == "office e\u{301} office" {
        let target = window
            .visual_neighbor(7, Downstream, None, true, current)
            .unwrap();
        assert_eq!(target.utf16, 9); // base+重音是一个真实GCB，不能先停在scalar边界8。
        let before = window
            .visual_neighbor(9, Downstream, Some(target.side), false, current)
            .unwrap();
        assert_eq!(before.utf16, 7);
    }
    let rows = handles(output);
    if rows.len() > 1 && text.is_ascii() {
        let boundary = rows[0].facts().source_utf16.end;
        let before = window
            .caret_geometry_with_side(boundary, Upstream, None, current)
            .unwrap();
        let target = window
            .visual_neighbor(boundary, Upstream, None, true, current)
            .unwrap();
        assert_eq!(target.utf16, boundary + 1);
        assert_eq!(target.affinity, Downstream);
        let after = window
            .caret_geometry_with_side(target.utf16, target.affinity, Some(target.side), current)
            .unwrap();
        assert_eq!(before.line_ordinal + 1, after.line_ordinal);
        assert!(before.primary.origin.y > after.primary.origin.y);
        // 原CM跨软折行仍只做一次真实GCB移动；行入口的同offset亲和性不能另占一次按键。
        // 只准备首行的同源窗口不准把它的末尾当作全文终点。
        let first = NativeMarkdownComposedViewport::prepare(
            output.clone(),
            window.key().clone(),
            palette.to_vec(),
            handles(output).into_iter().take(1),
            NSPoint::new(16.0, 304.0),
            current,
        )
        .unwrap();
        assert!(matches!(
            first.visual_neighbor(boundary, Upstream, None, true, current),
            Err(Error::Pending)
        ));
    }
    assert!(matches!(
        window.visual_neighbor(0, Downstream, None, true, &|| false),
        Err(Error::Revoked)
    ));
}
