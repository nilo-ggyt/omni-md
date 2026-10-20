//! 正式worker到Native字体、真实CPU绘制和GCB插入点的接线；不以手造目录替代生产组合。
use super::*;
use std::cell::RefCell;

use omni_markdown_core::{
    MarkdownComposedLinePart, MarkdownComposedLines, MarkdownLineComposeAnswer,
    MarkdownLineComposeBudget, MarkdownLineComposeCancellation, MarkdownLineComposeError,
    MarkdownLineComposeStyle, MarkdownLineComposer, MarkdownLineFontCatalog,
};
use omni_md_plugin::native_surface::{
    paged_glyph_paint::paint_composed_glyphs,
    paged_line_fonts::NativeMarkdownLineFont,
    paged_line_geometry::{NativeMarkdownLineGeometryError, composed_part_caret},
};

fn compose(
    source: &MarkdownLayoutTextSource,
    catalog: MarkdownLineFontCatalog,
    font: &NativeMarkdownLineFont,
) -> MarkdownComposedLines {
    let mut worker = MarkdownLineComposer::new(
        source.clone(),
        &source.authority(),
        catalog,
        220 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let mut next = 0;
    loop {
        let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
        match progress.answer {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(
                                range,
                                font.registered(),
                                MarkdownLineComposeStyle {
                                    presentation: Default::default(),
                                    points: font.native().point_size() as f32,
                                    language: None,
                                    features: Vec::new(),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::LineReady(facts) => {
                assert_eq!(facts.source_utf16.start, next);
                assert!(facts.source_utf16.end > next);
                next = facts.source_utf16.end;
            }
            MarkdownLineComposeAnswer::Complete => {
                assert_eq!(next, source.utf16_len());
                break;
            }
            // 此有限夹具有真实空格断点；未处理的控制/unsafe不能被测试当作成功行吞掉。
            MarkdownLineComposeAnswer::NeedsControl(_)
            | MarkdownLineComposeAnswer::NeedsTableRow(_)
            | MarkdownLineComposeAnswer::NeedsTabPlacement(_)
            | MarkdownLineComposeAnswer::NeedsResolution(_) => {
                panic!("正式Native接线夹具尚未产生完整真实软行");
            }
            #[cfg(feature = "native-paged-frontier-probe")]
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                // 本夹具只走有限完整oracle；启用前沿feature不授权跳过尚未完成的行。
                panic!("有限Native接线oracle出现未完成前沿：{reason:?}");
            }
        }
    }
    worker.into_output().unwrap()
}

pub(super) fn run(mtm: MainThreadMarker) {
    let name = CFString::from_str("Helvetica");
    // SAFETY: 固定有效名称和正32pt，无外部矩阵；返回原Native字体只在主线程使用。
    let native = unsafe { CTFont::with_name(&name, 32.0, std::ptr::null()) };
    let font = NativeMarkdownPagedFont::capture(&native, MAX_FONT_BYTES, mtm, &|| true).unwrap();
    let bytes = font.bytes_observer();
    let payload = font.core_font().resource_observer();
    let catalog = MarkdownLineFontCatalog::new();
    let font = NativeMarkdownLineFont::register(font, &catalog, &|| true).unwrap();
    let metrics = font.metrics().unwrap();
    let text = "office AV a\u{301} ".repeat(128); // 只创建固定合成测试源；产品输出不持全文字符串。
    let (_file, document) = fixture(&text);
    let backing = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_observer = source.resource_observer();
    let output = RefCell::new(compose(&source, catalog.clone(), &font));
    let output_observer = output.borrow().resource_observer();
    let current = || {
        assert!(output.try_borrow_mut().is_ok()); // current可重入；所有Native边必须先归还Reader借用。
        true
    };
    let mut canvas = Canvas::new();
    let mut line = output.borrow_mut().last_line().unwrap();
    let mut count = 0;
    let mut end = source.utf16_len();
    let mut first_part: Option<MarkdownComposedLinePart> = None;
    let mut painted = 0;
    while let Some(actual) = line {
        assert_eq!(actual.facts().source_utf16.end, end);
        for ordinal in 0..actual.facts().parts {
            let part = output.borrow_mut().part(&actual, ordinal).unwrap().unwrap();
            font.require(part.font_registration()).unwrap();
            if actual.facts().ordinal < 4 && part.facts().glyphs != 0 {
                let mut origin = NativeMarkdownPagedGlyphOrigin {
                    baseline: CGPoint {
                        x: 8.0,
                        y: HEIGHT as f64
                            - metrics.ascent
                            - (actual.facts().ordinal as f64
                                * (metrics.ascent + metrics.descent + metrics.leading)),
                    },
                    pen_x_26_6: part.facts().origin_x_26_6,
                    pen_y_26_6: part.facts().origin_y_26_6,
                };
                let mut start = 0;
                while start < part.facts().glyphs {
                    let end = (start + 256).min(part.facts().glyphs);
                    let receipt = paint_composed_glyphs(
                        &output,
                        &part,
                        NativeMarkdownPagedGlyphPaintRequest {
                            source: &source.authority(),
                            font_authority: &font.native().core_font().authority(),
                            native_font: font.native(),
                            glyph_range: start..end,
                            origin,
                        },
                        &canvas.context,
                        &current,
                    )
                    .unwrap();
                    assert_eq!(receipt.glyph_range, start..end);
                    origin.pen_x_26_6 = receipt.final_pen_x_26_6;
                    origin.pen_y_26_6 = receipt.final_pen_y_26_6;
                    start = end;
                    painted += end - receipt.glyph_range.start;
                }
                assert_eq!(
                    origin.pen_x_26_6,
                    part.facts().origin_x_26_6 + part.facts().x_advance_26_6
                );
            }
            if actual.facts().ordinal == 0 && ordinal == 0 {
                first_part = Some(part); // 只保第一个有限弱handle，禁止把全部行或glyph搬进Vec。
            }
        }
        end = actual.facts().source_utf16.start;
        count += 1;
        line = output.borrow_mut().previous_line(&actual).unwrap();
    }
    assert_eq!(end, 0);
    assert!(count > 20 && painted > 0 && canvas.pixel_facts().0 > 0);
    let part = first_part.unwrap();
    let baseline = CGPoint { x: 8.0, y: 80.0 };
    let start = composed_part_caret(&output, &part, &font, 0, baseline, &current).unwrap();
    assert_eq!(start.baseline.x, baseline.x);
    let before = composed_part_caret(&output, &part, &font, 2, baseline, &current).unwrap();
    let inside = composed_part_caret(&output, &part, &font, 3, baseline, &current).unwrap();
    let after = composed_part_caret(&output, &part, &font, 4, baseline, &current).unwrap();
    assert!(before.baseline.x < inside.baseline.x && inside.baseline.x < after.baseline.x);
    // a与组合附加符之间不是GCB；真实低层目录必须拒绝，不能按UTF16平均放置光标。
    assert!(matches!(
        composed_part_caret(&output, &part, &font, 11, baseline, &current),
        Err(NativeMarkdownLineGeometryError::InvalidPosition)
    ));

    let foreign = RefCell::new(compose(&source, catalog.clone(), &font));
    let foreign_observer = foreign.borrow().resource_observer();
    assert!(matches!(
        composed_part_caret(&foreign, &part, &font, 0, baseline, &|| true),
        Err(NativeMarkdownLineGeometryError::Composed(
            MarkdownLineComposeError::SourceMismatch
        ))
    ));
    output.into_inner().close().unwrap(); // 消费唯一Reader后，仍存活的弱part不能向另一输出借权限。
    assert!(composed_part_caret(&foreign, &part, &font, 0, baseline, &|| true).is_err());
    assert!(matches!(
        foreign.into_inner().close(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    let rejected = foreign_observer.snapshot();
    assert!(!rejected.file_open && rejected.mapped_bytes == 0 && !rejected.cleanup_failed);
    // 错票永久撤权并已归还唯一Reader；不能把随后重复close的Poisoned误作真实资源泄漏。
    let closed = output_observer.snapshot();
    assert!(
        !closed.file_open
            && closed.mapped_bytes == 0
            && !closed.cleanup_failed
            && !closed.reader_failed
    );
    drop(source);
    drop(document);
    drop(font);
    drop(catalog);
    assert!(source_observer.payload_released() && payload.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    assert!(bytes.unmap_succeeded() && bytes.mapped_bytes() == 0 && !bytes.failed());
    println!(
        "native_composed_lines actual_worker=true lines={count} actual_painted_glyphs={painted} real_ligature_caret=true invalid_gcb_rejected=true foreign_output_rejected=true actual_fd_close=true actual_unmap=true whole_product=false formal=false"
    );
}
