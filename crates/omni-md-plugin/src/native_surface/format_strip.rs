//! 原格式栏的真实原生装配；只保留固定动作控件，不读取正文或管理第二份编辑状态。

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

/// 原横滚格式栏保留日常编辑动作和顺序；既有 tag 不重编号，也不为已删除动作保留别名。
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
    let font = NSFont::systemFontOfSize_weight(11.0, weight);
    // 系统符号仅表达原动作，中文标题、说明与无障碍名称始终保留；不会新增格式状态或快捷键。
    let symbols = NSImageSymbolConfiguration::configurationWithPointSize_weight(13.0, weight);
    let tint = NSColor::colorWithSRGBRed_green_blue_alpha(0.61, 0.65, 0.72, 1.0);
    let mut x = SIDE_PADDING;
    for (index, &(tag, title, explanation, symbol)) in BUTTONS.iter().enumerate() {
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
        // 标题层级继续直接显示中文；常见格式用同一套原生符号，避免窄窗把末端动作藏在屏外。
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
        } // 系统图像不可取得时，原中文标题仍完整可用；不发布没有名称的空白动作。
        if index != 0 {
            // 附件仍保原常驻边框，其他动作只在悬停时显示边框；输入焦点仍属于正文。
            button.setShowsBorderOnlyWhileMouseInside(true);
        }
        // 只测实际按钮的固定短标题；真实字体宽加原左右8pt，不以字符数估计或读取正文。
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
        controls.push(button.into_super());
        x += width + GAP;
        if matches!(index, 0 | 3 | 9 | 13) {
            x += 6.0; // 只用留白区分附件、标题、行内格式、列表和块；原动作顺序与视图所有权不变。
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
