//! 原Native段落样式到真实零glyph Tab part的差分；短固定CTLine只作为离线oracle。
use super::*;
use std::cell::RefCell;

use objc2::{AnyThread, runtime::AnyObject};
use objc2_app_kit::{
    NSFont, NSFontAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
};
use objc2_core_text::CTLine;
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary, NSString,
};
use omni_markdown_core::{
    MarkdownComposedLinePartKind, MarkdownLineComposeAnswer, MarkdownLineComposeBudget,
    MarkdownLineComposeCancellation, MarkdownLineComposeStyle, MarkdownLineComposer,
    MarkdownLineFontCatalog,
};
use omni_md_plugin::native_surface::{
    paged_glyph_paint::paint_composed_glyphs, paged_line_fonts::NativeMarkdownLineFont,
    paged_line_geometry::composed_part_caret, paged_paragraph_style::capture_paged_paragraph_style,
};

pub(super) fn run(mtm: MainThreadMarker) {
    let base = NSFont::systemFontOfSize(15.0);
    let native =
        NativeMarkdownPagedFont::capture(base.as_ref(), MAX_FONT_BYTES, mtm, &|| true).unwrap();
    let bytes = native.bytes_observer();
    let catalog = MarkdownLineFontCatalog::new();
    let font = NativeMarkdownLineFont::register(native, &catalog, &|| true).unwrap();
    let tabs = "\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\tx";
    for (mode, text) in [
        (0, tabs),
        (1, tabs),
        (2, tabs),
        (3, "\t\t\t\t\t\t\t\t\t\t\t\txxxxxxxxxx\tz"),
    ] {
        let style = NSMutableParagraphStyle::new();
        if mode == 1 {
            // 最后显式stop之后继续使用原36.25点间隔，专门捕获错误的全局倍数原点。
            style.setDefaultTabInterval(36.25);
        }
        if mode == 2 {
            // 没有显式stop且原默认间隔为0；真实Native行为是保持pen，不是补造空格。
            style.setTabStops(Some(&NSArray::new()));
        }
        let paragraph = capture_paged_paragraph_style(&style, Some(false), mtm, &|| true).unwrap();
        let (_file, document) = fixture(text);
        let backing = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let source_observer = source.resource_observer();
        let mut worker = MarkdownLineComposer::new(
            source.clone(),
            &source.authority(),
            catalog.clone(),
            2000 * 64,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let mut produced = 0;
        loop {
            match worker
                .step(MarkdownLineComposeBudget::default())
                .unwrap()
                .answer
            {
                MarkdownLineComposeAnswer::Yielded => {}
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    // 文字部分仍由原实际UIFont注册响应；控制区不能借它伪造glyph。
                    let range = request.source_range();
                    worker
                        .provide_style(
                            request
                                .respond_registered(
                                    range,
                                    font.registered(),
                                    MarkdownLineComposeStyle {
                                        presentation: Default::default(),
                                        points: 15.0,
                                        language: None,
                                        features: Vec::new(),
                                    },
                                )
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineComposeAnswer::NeedsControl(request) => {
                    // 原nonce、同源范围和真实段落设置随响应归还，不以公开Tab范围补签。
                    worker
                        .provide_control(
                            request
                                .respond(font.registered(), 15.0, paragraph.core_style().clone())
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineComposeAnswer::LineReady(_) => {
                    // 只有真实LineEnd计数；中间item或控制响应完成都不能代替视觉行。
                    produced += 1;
                }
                MarkdownLineComposeAnswer::Complete => break, // 完整固定短源必须归一行，不省略末尾x/z。
                MarkdownLineComposeAnswer::NeedsResolution(_)
                | MarkdownLineComposeAnswer::NeedsTableRow(_)
                | MarkdownLineComposeAnswer::NeedsTabPlacement(_) => {
                    panic!("原实际left/default Tab必须真实完成，不允许pending代替原生行为")
                }
                #[cfg(feature = "native-paged-frontier-probe")]
                MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                    // 原短Tab oracle未申请前沿；不能把控制项的未完成结果记作行完成。
                    panic!("有限Tab oracle出现未完成前沿：{reason:?}");
                }
            }
        }
        assert_eq!(produced, 1);
        let output = RefCell::new(worker.into_output().unwrap());
        let observer = output.borrow().resource_observer();
        let line = output.borrow_mut().last_line().unwrap().unwrap();
        assert!(output.borrow_mut().previous_line(&line).unwrap().is_none());

        // 独立oracle使用同一原UIFont、同段落样式和全部固定短输入，不从Core事实重造预期。
        let string = NSString::from_str(text);
        assert!(string.length() <= 32);
        let font_object: &AnyObject = base.as_ref();
        let style_object: &AnyObject = style.as_ref();
        // SAFETY: 公开属性key分别配实际NSFont/NSParagraphStyle，固定短文本和属性均在本栈保活。
        let attributes: objc2::rc::Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
            NSDictionary::from_slices(
                &[NSFontAttributeName, NSParagraphStyleAttributeName],
                &[font_object, style_object],
            )
        };
        // SAFETY: 真实字符串与正确类型字典完整有效，无越界范围或借用外部存储。
        let attributed = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &string,
                Some(&attributes),
            )
        };
        // SAFETY: 公开Foundation/CFAttributedString桥接，独立line只排版上述≤32 units固定oracle。
        let native_line = unsafe { CTLine::with_attributed_string(attributed.as_ref()) };
        let baseline = CGPoint { x: 0.0, y: 80.0 };
        let mut canvas = Canvas::new();
        let mut covered = 0;
        let mut controls = 0;
        // 逐part验证来源连续；固定小输入允许穷举所有GCB位置，产品回调仍只查请求位置。
        for ordinal in 0..line.facts().parts {
            let part = output.borrow_mut().part(&line, ordinal).unwrap().unwrap();
            assert_eq!(part.facts().source_utf16.start, covered);
            for position in part
                .facts()
                .source_utf16
                .clone()
                .chain(std::iter::once(part.facts().source_utf16.end))
            {
                let caret =
                    composed_part_caret(&output, &part, &font, position, baseline, &|| true)
                        .unwrap();
                // SAFETY: position已核在原固定短字符串内；secondaryOffset公开允许null。
                let expected = unsafe {
                    native_line.offset_for_string_index(position as _, std::ptr::null_mut())
                };
                let tolerance = (position + 1) as f64 / 128.0 + 1e-9;
                assert!(
                    (caret.baseline.x - expected).abs() <= tolerance,
                    "真实Tab对照 mode={mode} position={position} core={} native={expected}",
                    caret.baseline.x
                );
            }
            if part.facts().kind == MarkdownComposedLinePartKind::Glyphs {
                // 只把真正文字交原sealed painter；首字位置同时检验Tab累积pen正确。
                paint_composed_glyphs(
                    &output,
                    &part,
                    NativeMarkdownPagedGlyphPaintRequest {
                        source: &source.authority(),
                        font_authority: &font.native().core_font().authority(),
                        native_font: font.native(),
                        glyph_range: 0..part.facts().glyphs,
                        origin: NativeMarkdownPagedGlyphOrigin {
                            baseline,
                            pen_x_26_6: part.facts().origin_x_26_6,
                            pen_y_26_6: part.facts().origin_y_26_6,
                        },
                    },
                    &canvas.context,
                    &|| true,
                )
                .unwrap();
            } else {
                assert_eq!(part.facts().kind, MarkdownComposedLinePartKind::Tab);
                assert_eq!(part.facts().glyphs, 0);
                controls += 1; // 每个真实Tab保源和几何，没有提交.notdef或空间替身。
            }
            covered = part.facts().source_utf16.end;
        }
        assert_eq!(covered, string.length());
        assert_eq!(controls, if mode == 3 { 13 } else { 16 });
        assert!(canvas.pixel_facts().0 > 0);
        output.into_inner().close().unwrap();
        let closed = observer.snapshot();
        assert!(
            !closed.file_open
                && closed.mapped_bytes == 0
                && !closed.cleanup_failed
                && !closed.reader_failed
        );
        drop(source);
        drop(document);
        assert!(source_observer.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
        println!(
            "native_control_tabs mode={mode} exact_native_style=true real_control_parts={controls} native_caret_oracle=true actual_ink=true actual_close_unmap=true formal=false"
        );
    }
    drop(font);
    drop(catalog);
    assert!(bytes.unmap_succeeded() && bytes.mapped_bytes() == 0 && !bytes.failed());
}
