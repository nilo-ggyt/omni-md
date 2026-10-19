//! 原格式栏的真实原生装配；只保留固定动作控件，不读取正文或管理第二份编辑状态。

use super::*;
use objc2_app_kit::{NSAttributedStringNSStringDrawing, NSBezelStyle, NSScrollerStyle};

pub(super) const HEIGHT: f64 = 42.0;
const BUTTON_HEIGHT: f64 = 29.0;
const MINIMUM_BUTTON_WIDTH: f64 = 29.0;
const HORIZONTAL_PADDING: f64 = 8.0;
const SIDE_PADDING: f64 = 12.0;
const GAP: f64 = 2.0;

/// 附件后保持原React的十九项顺序；tag只进入已有命令分派，不在本层实现格式规则。
const BUTTONS: &[(isize, &str, &str)] = &[
    (3, "附件", "导入附件并在原光标处插入链接"),
    (100, "H1", "一级标题"),
    (101, "H2", "二级标题"),
    (102, "H3", "三级标题"),
    (103, "B", "加粗"),
    (104, "I", "斜体"),
    (105, "U", "下划线"),
    (106, "S", "删除线"),
    (107, "</>", "行内代码"),
    (108, "↗", "链接"),
    (110, "•", "无序列表"),
    (111, "1.", "有序列表"),
    (112, "☑", "任务列表"),
    (109, "❞", "引用"),
    (118, "▦", "表格"),
    (113, "ƒ", "行内公式"),
    (117, "∑", "块级公式"),
    (114, "x²", "上标"),
    (116, "{ }", "代码块"),
    (115, "—", "分隔线"),
];

/// 构造唯一横向格式区，将按钮交回原controls所有者；返回根控件只供已有页眉布局观察。
/// content视图树持有scroll/content，原controls保活实际按钮直到统一detach，均不持Doc或闭包。
pub(super) fn install(
    controller: &NativeMarkdownController,
    content_root: &NSView,
    ledger: &NativeMarkdownOwnedLedger,
    controls: &mut Vec<Retained<NSControl>>,
    has_document: bool,
    mtm: MainThreadMarker,
) -> Retained<NSScrollView> {
    // 施工中的窄根只触发原生横滚装配；页眉在发布窗口前写入唯一真实frame，不靠弹簧再布局。
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

    // SAFETY: AppKit固定字体权重常量在进程内始终有效，只用于这些静态短标签的标准字体。
    let weight = unsafe { NSFontWeightSemibold };
    let format_font = NSFont::monospacedSystemFontOfSize_weight(11.0, weight);
    let attachment_font = NSFont::systemFontOfSize_weight(11.0, weight);
    let tint = NSColor::colorWithSRGBRed_green_blue_alpha(0.61, 0.65, 0.72, 1.0);
    let mut x = SIDE_PADDING;
    for (index, &(tag, title, explanation)) in BUTTONS.iter().enumerate() {
        // SAFETY: 原主线程controller实现固定NSControl action ABI；统一detach先断开这条借用边。
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
        if index == 0 {
            // 附件沿原独立中文入口使用系统字体；常驻边框保留与后续格式符号的原分隔。
            button.setFont(Some(&attachment_font));
        } else {
            // 格式符号保持原等宽字形与hover边框；鼠标点击不会将输入焦点转到按钮。
            button.setFont(Some(&format_font));
            button.setShowsBorderOnlyWhileMouseInside(true);
        }
        // 只测实际按钮的固定短标题；真实字体宽加原左右8pt，不以字符数估计或读取正文。
        let width = (button.attributedTitle().size().width + 2.0 * HORIZONTAL_PADDING)
            .ceil()
            .max(MINIMUM_BUTTON_WIDTH);
        button.setFrame(NSRect::new(
            NSPoint::new(x, (HEIGHT - BUTTON_HEIGHT) / 2.0),
            NSSize::new(width, BUTTON_HEIGHT),
        ));
        ledger
            .include_control("format.button", &button)
            .expect("固定格式按钮和cell纳入原账本");
        content.addSubview(&button);
        controls.push(button.into_super());
        x += width + GAP;
        if index == 0 {
            x += 6.0; // 附件后的原额外间距不转化为新分组或新增控件。
        }
    }
    // 内容宽由全部真实按钮决定，窄窗只裁切滚动视口；动作不会压缩、折行或被折叠菜单替换。
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
        ledger
            .include(role, object)
            .expect("固定格式视图纳入原账本");
    }
    content_root.addSubview(&scroll);
    scroll
}
