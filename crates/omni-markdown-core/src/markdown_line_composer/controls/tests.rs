//! 固定期望来自真实Cocoa CTLine控制，不用同一网格公式生成oracle掩盖anchor/零间隔差异。
use super::*;
fn defaults(interval: Option<i64>) -> MarkdownLineParagraphStyle {
    MarkdownLineParagraphStyle {
        tab_stops: (1..=12)
            .map(|n| MarkdownLineTabStop {
                location_26_6: n * 28 * 64,
                alignment: MarkdownLineTabAlignment::Left,
            })
            .collect(),
        default_tab_interval_26_6: interval,
    }
}
fn advance(style: &MarkdownLineParagraphStyle, pen: i64) -> i64 {
    let MarkdownLineTabPlacement::Left { advance_26_6 } = style.tab(pen).unwrap() else {
        panic!("实际已知Left样式不应待决");
    };
    advance_26_6
}
#[test]
fn cocoa_known_zero_interval_stops_after_twelve_stops_and_never_moves_pen_backwards() {
    let style = defaults(Some(0));
    let mut pen = 0;
    for target in [
        28, 56, 84, 112, 140, 168, 196, 224, 252, 280, 308, 336, 336, 336, 336, 336,
    ] {
        pen += advance(&style, pen);
        assert_eq!(pen, target * 64);
    }
    // Native v3在最后stop后先写10个x，再Tab，真实pen保持412.31835938而非退回336。
    let after_text = 412 * 64 + 20;
    assert_eq!(advance(&style, after_text), 0);
    let none = MarkdownLineParagraphStyle {
        tab_stops: Vec::new(),
        default_tab_interval_26_6: Some(0),
    };
    assert_eq!(advance(&none, 0), 0);
    assert_eq!(advance(&none, 76 * 64 + 20), 0);
    assert!(matches!(
        defaults(None).tab(336 * 64).unwrap(),
        MarkdownLineTabPlacement::NeedsDefaultInterval
    ));
}
#[test]
fn cocoa_fractional_interval_is_anchored_to_last_explicit_stop() {
    let style = defaults(Some(2320));
    let mut pen = 336 * 64;
    for target in [23824, 26144, 28464, 30784] {
        pen += advance(&style, pen);
        assert_eq!(pen, target);
    }
    let empty = MarkdownLineParagraphStyle {
        tab_stops: Vec::new(),
        default_tab_interval_26_6: Some(2320),
    };
    let pen = 76 * 64 + 20;
    assert_eq!(pen + advance(&empty, pen), 108 * 64 + 48);
    assert!(
        MarkdownLineParagraphStyle {
            tab_stops: Vec::new(),
            default_tab_interval_26_6: Some(-1)
        }
        .require()
        .is_err()
    );
}
