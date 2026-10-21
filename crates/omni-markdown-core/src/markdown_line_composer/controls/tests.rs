//! Fixed expectations come from actual Cocoa CTLine controls. Reusing the
//! production grid formula as an oracle could conceal anchor/zero-interval errors.
use super::*;
/// Reproduce the finite captured system stop list while varying only its known interval.
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
/// Consume only a completed left-aligned placement; a pending answer cannot stand in for it.
fn advance(style: &MarkdownLineParagraphStyle, pen: i64) -> i64 {
    let MarkdownLineTabPlacement::Left { advance_26_6 } = style.tab(pen).unwrap() else {
        panic!("实际已知Left样式不应待决");
    };
    advance_26_6
}
#[test]
fn cocoa_known_zero_interval_stops_after_twelve_stops_and_never_moves_pen_backwards() {
    // The known zero interval stops forward movement after the actual twelve
    // explicit stops, including subsequent tabs with no remaining explicit stop.
    let style = defaults(Some(0));
    let mut pen = 0;
    for target in [
        28, 56, 84, 112, 140, 168, 196, 224, 252, 280, 308, 336, 336, 336, 336, 336,
    ] {
        pen += advance(&style, pen);
        assert_eq!(pen, target * 64);
    }
    // The captured native case inserts ten x glyphs after the final stop. Its
    // following tab keeps the pen at 412.31835938 rather than moving back to 336.
    let after_text = 412 * 64 + 20;
    assert_eq!(advance(&style, after_text), 0);
    let none = MarkdownLineParagraphStyle {
        // A known zero default is still complete when there are no explicit stops.
        tab_stops: Vec::new(),
        default_tab_interval_26_6: Some(0),
    };
    assert_eq!(advance(&none, 0), 0);
    assert_eq!(advance(&none, 76 * 64 + 20), 0);
    assert!(matches!(
        // Unknown defaults are a separate pending contract, not the known zero case.
        defaults(None).tab(336 * 64).unwrap(),
        MarkdownLineTabPlacement::NeedsDefaultInterval
    ));
}
#[test]
fn cocoa_fractional_interval_is_anchored_to_last_explicit_stop() {
    // These fixed 26.6 positions were observed after the last explicit stop;
    // their origin must not be replaced by a document-zero grid.
    let style = defaults(Some(2320));
    let mut pen = 336 * 64;
    for target in [23824, 26144, 28464, 30784] {
        pen += advance(&style, pen);
        assert_eq!(pen, target);
    }
    let empty = MarkdownLineParagraphStyle {
        // With no explicit stop, zero is the actual implicit-grid origin.
        tab_stops: Vec::new(),
        default_tab_interval_26_6: Some(2320),
    };
    let pen = 76 * 64 + 20;
    assert_eq!(pen + advance(&empty, pen), 108 * 64 + 48);
    assert!(
        // A negative captured interval is rejected rather than normalized into spacing.
        MarkdownLineParagraphStyle {
            tab_stops: Vec::new(),
            default_tab_interval_26_6: Some(-1)
        }
        .require()
        .is_err()
    );
}
