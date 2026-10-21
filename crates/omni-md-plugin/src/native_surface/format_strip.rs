//! Assemble the existing format strip from actual Native controls. Fixed actions
//! neither read document text nor own another editing state.

use super::*;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSBezelStyle, NSCellImagePosition, NSImage,
    NSImageSymbolConfiguration, NSScrollerStyle,
};

pub(super) const HEIGHT: f64 = 42.0;
const BUTTON_HEIGHT: f64 = 29.0;
const MINIMUM_BUTTON_WIDTH: f64 = 29.0;
const HORIZONTAL_PADDING: f64 = 8.0;
const SIDE_PADDING: f64 = 12.0;
const GAP: f64 = 2.0;

/// Preserve everyday actions and their established horizontal order. Existing
/// action tags stay stable, without aliases for actions that have been removed.
const BUTTONS: &[(isize, &str, &str, Option<&str>)] = &[
    (3, "附件", "导入附件并在原光标处插入链接", Some("paperclip")),
    (100, "标题一", "一级标题", None),
    (101, "标题二", "二级标题", None),
    (102, "标题三", "三级标题", None),
    (103, "加粗", "加粗", Some("bold")),
    (104, "斜体", "斜体", Some("italic")),
    (105, "下划线", "下划线", Some("underline")),
    (106, "删除线", "删除线", Some("strikethrough")),
    (
        107,
        "行内代码",
        "插入行内代码",
        Some("chevron.left.forwardslash.chevron.right"),
    ),
    (108, "链接", "插入链接", Some("link")),
    (110, "无序列表", "无序列表", Some("list.bullet")),
    (111, "有序列表", "有序列表", Some("list.number")),
    (112, "任务列表", "任务列表", Some("checklist")),
    (109, "引用", "引用", Some("text.quote")),
    (118, "表格", "插入表格", Some("tablecells")),
    (116, "代码块", "插入代码块", Some("curlybraces")),
    (115, "分隔线", "插入分隔线", Some("minus")),
];

/// Install the sole horizontal strip and return its root to the existing header
/// layout. The content tree owns scroll/content and the original controls owner
/// retains buttons until unified detach; none retain a document or callback closure.
pub(super) fn install(
    controller: &NativeMarkdownController,
    content_root: &NSView,
    ledger: &NativeMarkdownOwnedLedger,
    controls: &mut Vec<Retained<NSControl>>,
    has_document: bool,
    mtm: MainThreadMarker,
) -> Retained<NSScrollView> {
    // Construction's narrow root initializes Native horizontal scrolling. The
    // original header supplies the actual frame before the window is published.
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(1.0, HEIGHT)),
    );
    scroll.setDrawsBackground(false);
    scroll.setHasVerticalScroller(false);
    scroll.setHasHorizontalScroller(true);
    scroll.setScrollerStyle(NSScrollerStyle::Overlay);
    scroll.setAutohidesScrollers(true);
    scroll.setAccessibilityLabel(Some(ns_string!("Markdown 格式工具栏")));
    let content = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(1.0, HEIGHT)),
    );

    // SAFETY: AppKit's immutable font-weight constant is valid for the process
    // lifetime and is used only for these finite static labels.
    let weight = unsafe { NSFontWeightSemibold };
    let font = NSFont::systemFontOfSize_weight(11.0, weight);
    // System symbols represent existing actions while Chinese titles, tooltips,
    // and accessibility names remain available. No shortcut or format state is added.
    let symbols = NSImageSymbolConfiguration::configurationWithPointSize_weight(13.0, weight);
    let tint = NSColor::colorWithSRGBRed_green_blue_alpha(0.61, 0.65, 0.72, 1.0);
    let mut x = SIDE_PADDING;
    for (index, &(tag, title, explanation, symbol)) in BUTTONS.iter().enumerate() {
        // SAFETY: The original main-thread controller implements this fixed
        // NSControl action ABI; unified detach disconnects the target before release.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(controller),
                Some(sel!(performMarkdownWorkspaceAction:)),
                mtm,
            )
        };
        button.setTag(tag);
        button.setBezelStyle(NSBezelStyle::AccessoryBarAction);
        button.setRefusesFirstResponder(true);
        button.setFocusRingType(NSFocusRingType::None);
        button.setEnabled(has_document);
        button.setToolTip(Some(&NSString::from_str(explanation)));
        button.setAccessibilityLabel(Some(&NSString::from_str(explanation)));
        button.setContentTintColor(Some(&tint));
        // Heading levels retain Chinese labels. Compact system symbols keep the
        // everyday format actions usable through the original horizontal viewport.
        button.setFont(Some(&font));
        let image = symbol.and_then(|name| {
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(name),
                Some(&NSString::from_str(explanation)),
            )
            .and_then(|image| image.imageWithSymbolConfiguration(&symbols))
        });
        if let Some(image) = &image {
            button.setImage(Some(image));
            button.setImagePosition(NSCellImagePosition::ImageOnly);
        } // A missing system symbol leaves the original Chinese title usable, never a blank action.
        if index != 0 {
            // Attachments retain their existing permanent border. Other actions
            // show a hover border while the text client keeps keyboard focus.
            button.setShowsBorderOnlyWhileMouseInside(true);
        }
        // Measure only the actual finite button title, using its font and original
        // horizontal padding. Width never depends on document reads or character counts.
        let width = if image.is_some() {
            MINIMUM_BUTTON_WIDTH
        } else {
            (button.attributedTitle().size().width + 2.0 * HORIZONTAL_PADDING)
                .ceil()
                .max(MINIMUM_BUTTON_WIDTH)
        };
        button.setFrame(NSRect::new(
            NSPoint::new(x, (HEIGHT - BUTTON_HEIGHT) / 2.0),
            NSSize::new(width, BUTTON_HEIGHT),
        ));
        ledger
            .include_control("format.button", &button)
            .expect("固定格式按钮和cell纳入原账本");
        content.addSubview(&button);
        // Transfer the real button to the existing control owner; it will detach
        // targets and release the same object with the rest of the editor graph.
        controls.push(button.into_super());
        x += width + GAP;
        if matches!(index, 0 | 3 | 9 | 13) {
            x += 6.0; // Separate the established action groups with space, preserving order and owners.
        }
    }
    // Actual button widths determine content size. A narrow window clips only
    // the scroll viewport without squeezing, wrapping, or replacing actions with a menu.
    content.setFrameSize(NSSize::new(x - GAP + SIDE_PADDING, HEIGHT));
    scroll.setDocumentView(Some(&content));
    let clip = scroll.contentView();
    let scroller = scroll
        .horizontalScroller()
        .expect("格式栏已装配原生水平滚动条");
    for (role, object) in [
        ("format.scroll", &*scroll as &AnyObject),
        ("format.clip", &*clip as &AnyObject),
        ("format.scroller", &*scroller as &AnyObject),
        ("format.content", &*content as &AnyObject),
    ] {
        // Register actual Native owners with the existing release ledger before
        // mounting; reaching Rust Drop alone is not a Native deallocation receipt.
        ledger
            .include(role, object)
            .expect("固定格式视图纳入原账本");
    }
    content_root.addSubview(&scroll);
    scroll
}
