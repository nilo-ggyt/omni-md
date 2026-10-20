//! 声明的有限合成Source/Scene几何控制；真实Native字体与像素，不声明实际产品视口或parity。

use std::{
    cell::{Cell, RefCell},
    io::Write,
    rc::Rc,
};

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFRange, CFRetained, CFString};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    kCGColorSpaceSRGB,
};
use objc2_core_text::CTFont;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::{
    MarkdownComposedLine, MarkdownComposedLines, MarkdownDocumentBuffer, MarkdownLayoutTextSource,
    MarkdownLineComposeAnswer, MarkdownLineComposeBudget, MarkdownLineComposeCancellation,
    MarkdownLineComposeError, MarkdownLineComposeStyle, MarkdownLineComposer,
    MarkdownLineFontCatalog, MarkdownPresentationStyles,
};
use omni_md_plugin::native_surface::{
    paged_fonts::NativeMarkdownPagedFont,
    paged_input::NativeMarkdownPagedInput,
    paged_line_fonts::NativeMarkdownLineFont,
    paged_line_viewport::{
        NativeMarkdownComposedViewport, NativeMarkdownComposedViewportError as Error,
        NativeMarkdownComposedViewportPaintCause,
    },
    paged_text_view::{
        NativeMarkdownPagedHeadAffinity, NativeMarkdownPagedSceneKey,
        NativeMarkdownPagedSceneNonce, NativeMarkdownPagedSceneRequest,
        NativeMarkdownPagedVirtualViewport,
    },
};

const WIDTH: usize = 720;
const HEIGHT: usize = 320;
#[path = "paged_line_viewport/paint_steps.rs"]
mod paint_steps;
#[path = "paged_line_viewport/table_cell.rs"]
mod table_cell;
#[path = "paged_line_viewport/visual_steps.rs"]
mod visual_steps;
struct Canvas {
    context: CFRetained<CGContext>,
    pixels: Vec<u8>,
}
impl Canvas {
    fn new() -> Self {
        let mut pixels = vec![255u8; WIDTH * HEIGHT * 4];
        // SAFETY: 框架sRGB静态常量进程期有效，不读取用户图片或路径。
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        // SAFETY: 固定完整RGBA8 allocation，context字段先析构，Vec移动不移动底层像素。
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                WIDTH,
                HEIGHT,
                8,
                WIDTH * 4,
                Some(&space),
                CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .unwrap();
        CGContext::set_rgb_fill_color(Some(&context), 0.0, 0.0, 0.0, 1.0);
        Self { context, pixels }
    }
    fn ink(&mut self) -> usize {
        CGContext::flush(Some(&self.context));
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0].min(pixel[1]).min(pixel[2]) < 250)
            .count()
    }
}

fn compose(
    source: &MarkdownLayoutTextSource,
    palette: &[Rc<NativeMarkdownLineFont>],
    catalog: MarkdownLineFontCatalog,
    width: i64,
    presentation: MarkdownPresentationStyles,
) -> MarkdownComposedLines {
    let mut composer = MarkdownLineComposer::new(
        source.clone(),
        &source.authority(),
        catalog,
        width,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    for _ in 0..100_000 {
        match composer
            .step(MarkdownLineComposeBudget {
                bytes: 64,
                scalars: 16,
                records: 16,
                metadata_units: 16,
            })
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                // 有限mixed夹具按真实Script请求选已捕获字体；名称相同不能代替实际glyph覆盖。
                let font = if request.script() == *b"Hebr" {
                    palette.get(1).unwrap_or(&palette[0])
                } else {
                    &palette[0]
                };
                composer
                    .provide_style(
                        request
                            .respond_registered(
                                range,
                                font.registered(),
                                MarkdownLineComposeStyle {
                                    presentation,
                                    points: font.native().point_size() as f32,
                                    language: None,
                                    features: Vec::new(),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::Complete => return composer.into_output().unwrap(),
            MarkdownLineComposeAnswer::NeedsResolution(request) => {
                panic!("本有限空格断行夹具不得伪造unsafe解析：{request:?}")
            }
            MarkdownLineComposeAnswer::NeedsControl(request) => {
                panic!("无控制符夹具出现未处理请求：{request:?}")
            }
            // 本夹具未启用真实表格 flow，出现块行请求必须报错，不能吞作普通文字完成。
            MarkdownLineComposeAnswer::NeedsTableRow(_) => {
                panic!("非表格夹具出现真实表格行请求")
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(_) => {
                panic!("无制表符的有限夹具不能伪造Tab定位")
            }
            #[cfg(feature = "native-paged-frontier-probe")]
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("未启用前沿的有限完整oracle不得出现前沿请求：{reason:?}")
            }
        }
    }
    panic!("有限真实组合器没有完成，不能以超时/Pending冒充成功");
}

fn handles(output: &RefCell<MarkdownComposedLines>) -> Vec<MarkdownComposedLine> {
    let mut output = output.borrow_mut();
    let mut rows = Vec::new(); // 明确有限测试oracle；生产窗口不遍历/保留全部文档Line。
    let mut line = output.last_line().unwrap();
    while let Some(current) = line {
        assert!(rows.len() < 32);
        line = output.previous_line(&current).unwrap();
        rows.push(current);
    }
    rows.reverse();
    rows
}

/// 实际封存行验证换行两侧、有限窗口缺页和BiDi双位置；不以伪造行几何替代生产查询。
fn verify_caret_geometry(
    window: &NativeMarkdownComposedViewport,
    output: &Rc<RefCell<MarkdownComposedLines>>,
    palette: &[Rc<NativeMarkdownLineFont>],
    text: &str,
    mixed: bool,
    current: &dyn Fn() -> bool,
) {
    use NativeMarkdownPagedHeadAffinity::{Downstream, Upstream};
    let length = text.encode_utf16().count();
    // 真BOF/EOF没有文档外的另一行；两种侧必须读取同一个实际边缘，而非缓存末尾。
    for position in [0, length] {
        let before = window.caret_geometry(position, Upstream, current).unwrap();
        let after = window
            .caret_geometry(position, Downstream, current)
            .unwrap();
        assert_eq!(before.line_ordinal, after.line_ordinal);
        assert_eq!(before.primary, after.primary);
        assert_eq!(before.secondary, after.secondary);
    }
    let rows = handles(output);
    if rows.len() > 1 {
        let position = rows[0].facts().source_utf16.end;
        assert!(position < length);
        let before = window.caret_geometry(position, Upstream, current).unwrap();
        let after = window
            .caret_geometry(position, Downstream, current)
            .unwrap();
        assert_eq!(before.line_ordinal, rows[0].facts().ordinal);
        assert_eq!(after.line_ordinal, rows[1].facts().ordinal);
        assert!(before.primary.origin.y > after.primary.origin.y);
        // 实际IME零范围出口必须消费同一侧，不能把两次查询都放到下一行。
        assert_eq!(
            window
                .first_rect_with_affinity(NSRange::new(position, 0), Upstream, current)
                .unwrap()
                .rect,
            before.primary
        );
        assert_eq!(
            window
                .first_rect_with_affinity(NSRange::new(position, 0), Downstream, current)
                .unwrap()
                .rect,
            after.primary
        );
        // 单行前缀只准备了上游；不拿它的末尾给下游/全文EOF补签权限。
        let first = NativeMarkdownComposedViewport::prepare(
            output.clone(),
            window.key().clone(),
            palette.to_vec(),
            handles(output).into_iter().take(1),
            NSPoint::new(16.0, 304.0),
            current,
        )
        .unwrap();
        assert!(first.caret_geometry(position, Upstream, current).is_ok());
        assert!(matches!(
            first.caret_geometry(position, Downstream, current),
            Err(Error::Pending)
        ));
        assert!(matches!(
            first.caret_geometry(length, Upstream, current),
            Err(Error::Pending)
        ));
        let frame = window.line_frame(1).unwrap();
        // 后一行单独准备时相反：实际top取原度量；不能把窗口起点冒充真实BOF。
        let second = NativeMarkdownComposedViewport::prepare(
            output.clone(),
            window.key().clone(),
            palette.to_vec(),
            handles(output).into_iter().skip(1).take(1),
            NSPoint::new(16.0, frame.origin.y + frame.size.height),
            current,
        )
        .unwrap();
        assert!(second.caret_geometry(position, Downstream, current).is_ok());
        assert!(matches!(
            second.caret_geometry(position, Upstream, current),
            Err(Error::Pending)
        ));
    }
    if mixed {
        // 本夹具真实LTR→Hebrew边界有两个视觉位置；换行侧不能静默消掉BiDi歧义。
        for affinity in [Upstream, Downstream] {
            let boundary = window.caret_geometry(7, affinity, current).unwrap();
            assert!(
                boundary
                    .secondary
                    .is_some_and(|rect| rect != boundary.primary)
            );
            assert!(
                window
                    .caret_geometry(8, affinity, current)
                    .unwrap()
                    .secondary
                    .is_none()
            );
        }
    }
    if let Some(byte) = text.find('\u{301}') {
        // base和组合重音之间虽是scalar边界，却不是原GCB，不能成为编辑插入点。
        let position = text[..byte].encode_utf16().count();
        assert!(matches!(
            window.caret_geometry(position, Downstream, current),
            Err(Error::Geometry(_))
        ));
    }
    assert!(matches!(
        window.caret_geometry(length + 1, Downstream, current),
        Err(Error::InvalidRange)
    ));
    assert!(matches!(
        window.caret_geometry(0, Downstream, &|| false),
        Err(Error::Revoked)
    ));
}

fn finite_case(
    mtm: MainThreadMarker,
    text: &str,
    width: i64,
    over_budget: bool,
    mixed: bool,
    presentation: MarkdownPresentationStyles,
) {
    assert!(text.len() <= 16_384); // 原语控制输入上限，不能把整段读取逻辑引入产品。
    let (mapping, source_observer, font_bytes, font_observer, output_observer, weak_doc) =
        autoreleasepool(|_| {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            file.write_all(text.as_bytes()).unwrap();
            let document = Rc::new(RefCell::new(
                MarkdownDocumentBuffer::open(file.path(), 65_536).unwrap(),
            ));
            let mapping = document.borrow().original_backing_observer().unwrap();
            let weak_doc = Rc::downgrade(&document);
            let source = MarkdownLayoutTextSource::capture(&document.borrow());
            let source_observer = source.resource_observer();
            // SAFETY: 本地字体名存活至同步构造结束；null matrix保留实际非整数32.1pt，不借旧float字体。
            let native = unsafe {
                CTFont::with_name(&CFString::from_str("Helvetica"), 32.1, std::ptr::null())
            };
            let captured =
                NativeMarkdownPagedFont::capture(&native, 512 * 1024 * 1024, mtm, &|| true)
                    .unwrap();
            let mut font_bytes = vec![captured.bytes_observer()];
            let mut font_observer = vec![captured.core_font().resource_observer()];
            let catalog = MarkdownLineFontCatalog::new();
            let font =
                Rc::new(NativeMarkdownLineFont::register(captured, &catalog, &|| true).unwrap());
            let mut palette = vec![font.clone()];
            if mixed {
                // SAFETY: 本地字体名和空matrix有效，实际40.3pt用于检验混合字体真实行高。
                let base = unsafe {
                    CTFont::with_name(&CFString::from_str("Helvetica"), 40.3, std::ptr::null())
                };
                // SAFETY: 明确有限Hebrew夹具为完整3个UTF16单元；只使用该真实fallback返回的Font。
                let hebrew =
                    unsafe { base.for_string(&CFString::from_str("אבג"), CFRange::new(0, 3)) };
                let captured =
                    NativeMarkdownPagedFont::capture(&hebrew, 512 * 1024 * 1024, mtm, &|| true)
                        .unwrap();
                font_bytes.push(captured.bytes_observer());
                font_observer.push(captured.core_font().resource_observer());
                palette.push(Rc::new(
                    NativeMarkdownLineFont::register(captured, &catalog, &|| true).unwrap(),
                ));
            }
            let output = Rc::new(RefCell::new(compose(
                &source,
                &palette,
                catalog,
                width,
                presentation,
            )));
            let output_observer = output.borrow().resource_observer();
            let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
            let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
            let nonce = NativeMarkdownPagedSceneNonce::new();
            // 几何是本原语控制声明的有限画布；Source/Input/nonce均真实，但不冒充Native View签发的Ready。
            let key = NativeMarkdownPagedSceneKey {
                presentation_mode: omni_markdown_core::MarkdownPresentationMode::Source,
                request: NativeMarkdownPagedSceneRequest {
                    document: Rc::downgrade(&document),
                    target: input.target_domain().unwrap(),
                    input: input.state().unwrap(),
                    viewport: bounds,
                    bounds_width: bounds.size.width,
                    frame_in_parent: bounds,
                    geometry_epoch: 1,
                    virtual_viewport: NativeMarkdownPagedVirtualViewport {
                        document_origin: NSPoint::ZERO,
                        layout_origin_x: 0.0,
                        layout_width: bounds.size.width,
                        scroll_epoch: 1,
                    },
                },
                source: source.authority(),
                nonce: nonce.authority(),
            };
            let active = Cell::new(true);
            let callback_count = Cell::new(0);
            let current = || {
                // 所有可重入检查都必须归还真实Reader租约，不能只靠注释声称无跨Native借用。
                assert!(output.try_borrow_mut().is_ok());
                callback_count.set(callback_count.get() + 1);
                active.get()
            };
            // 同Doc/generation/字节的新capture不授予旧Output读取权；Source错误优先于bool撤权。
            let foreign_source = MarkdownLayoutTextSource::capture(&document.borrow());
            let mut foreign_key = key.clone();
            foreign_key.source = foreign_source.authority();
            assert!(matches!(
                NativeMarkdownComposedViewport::prepare(
                    output.clone(),
                    foreign_key,
                    palette.clone(),
                    handles(&output),
                    NSPoint::new(16.0, 304.0),
                    &|| false
                ),
                Err(Error::Composed(MarkdownLineComposeError::SourceMismatch))
            ));
            drop(foreign_source);
            let mut invalid_key = key.clone();
            invalid_key.request.virtual_viewport.document_origin.y = f64::INFINITY;
            assert!(matches!(
                NativeMarkdownComposedViewport::prepare(
                    output.clone(),
                    invalid_key,
                    palette.clone(),
                    handles(&output),
                    NSPoint::new(16.0, 304.0),
                    &current
                ),
                Err(Error::InvalidGeometry)
            ));
            let window = Rc::new(
                NativeMarkdownComposedViewport::prepare(
                    output.clone(),
                    key,
                    palette.clone(),
                    handles(&output),
                    NSPoint::new(16.0, 304.0),
                    &current,
                )
                .unwrap(),
            );
            let mut canvas = Canvas::new();
            if over_budget {
                let error = window.paint(&canvas.context, &current).unwrap_err();
                assert!(matches!(
                    error.cause,
                    NativeMarkdownComposedViewportPaintCause::Window(Error::Pending)
                ));
                assert!(!error.native_draw_returned && canvas.ink() == 0); // 超预算不画前缀，不发布半图。
                paint_steps::over_sync_limit(&window, &output, &palette[0], &current);
            } else {
                window.validate_ready(&current).unwrap();
                assert!(matches!(
                    window.validate_ready(&|| false),
                    Err(Error::Revoked)
                ));
                verify_caret_geometry(&window, &output, &palette, text, mixed, &current);
                visual_steps::verify(&window, &output, &palette, text, &current);
                let receipt = window.paint(&canvas.context, &current).unwrap();
                assert!(receipt.glyphs > 0 && receipt.batches > 0 && receipt.missing_glyphs == 0);
                if presentation.contains(MarkdownPresentationStyles::HIDDEN_MARKER) {
                    assert_eq!(canvas.ink(), 0); // 真正32.1pt的非空glyph全部透明，不能靠0.1pt字体看似“无墨迹”。
                } else {
                    assert!(canvas.ink() > 0); // 两个装饰夹具只有空格；没有实际线像素就不能用字形墨迹蒙混。
                }
                paint_steps::matches_finite_oracle(
                    &window,
                    &canvas.pixels,
                    receipt.glyphs,
                    &current,
                );
                let first = window.first_rect(NSRange::new(0, 1), &current).unwrap();
                assert_eq!(first.actual_range, NSRange::new(0, 1));
                assert!(first.rect.size.width > 0.0 && first.rect.size.height > 0.0);
                let hit = window
                    .hit(
                        NSPoint::new(
                            first.rect.origin.x,
                            first.rect.origin.y + first.rect.size.height / 2.0,
                        ),
                        &current,
                    )
                    .unwrap();
                assert_eq!(hit.source_utf16, 0);
                if mixed {
                    // Hebrew起点在视觉右侧；点首GCB左端应命中逻辑下一边界，不能沿LTR平均。
                    let hebrew = window.first_rect(NSRange::new(7, 1), &current).unwrap();
                    let hit = window
                        .hit(
                            NSPoint::new(
                                hebrew.rect.origin.x,
                                hebrew.rect.origin.y + hebrew.rect.size.height / 2.0,
                            ),
                            &current,
                        )
                        .unwrap();
                    assert!(hit.source_utf16 == 8 || hit.other_source_utf16 == Some(8));
                    let first = palette[0].metrics().unwrap();
                    let second = palette[1].metrics().unwrap();
                    let expected = first.ascent.max(second.ascent)
                        + first.descent.max(second.descent)
                        + first.leading.max(second.leading);
                    assert!((window.line_frame(0).unwrap().size.height - expected).abs() < 1e-9);
                }
                let mut rects = 0;
                window
                    .selection_rects(0..text.encode_utf16().count(), &current, |rect| {
                        assert!(output.try_borrow_mut().is_ok());
                        assert!(rect.size.width >= 0.0 && rect.size.height > 0.0);
                        rects += 1;
                        Ok(())
                    })
                    .unwrap();
                assert!(rects >= window.line_count());
                assert!(matches!(
                    window.hit(NSPoint::new(-1.0, -1.0), &current),
                    Err(Error::Pending)
                ));
                let mut called = 0;
                assert!(matches!(
                    window.selection_rects(0..text.encode_utf16().count(), &current, |_| {
                        called += 1;
                        active.set(false);
                        Ok(())
                    }),
                    Err(Error::Revoked)
                ));
                assert_eq!(called, 1); // 后继撤权不能继续交付其它行的选择框。
                println!(
                    "native_paged_line_viewport lines={} glyphs={} batches={} real_ink={} real_native_metrics=true bounded_hit=true streamed_selection=true callback_borrow_free=true actual_view_geometry=false formal=false",
                    window.line_count(),
                    receipt.glyphs,
                    receipt.batches,
                    canvas.ink()
                );
            }
            assert!(callback_count.get() > 0);
            let canonical = document.borrow();
            assert_eq!(
                (
                    canonical.generation(),
                    canonical.byte_len(),
                    canonical.utf16_len()
                ),
                (0, text.len(), text.encode_utf16().count())
            );
            drop(canonical); // 归还真实Doc借用后才拆Native字体/输出资源，不把只读控制变成跨析构借用。
            drop(window);
            Rc::try_unwrap(output)
                .ok()
                .unwrap()
                .into_inner()
                .close()
                .unwrap();
            drop(source);
            drop(font);
            drop(palette);
            drop(input);
            drop(document);
            (
                mapping,
                source_observer,
                font_bytes,
                font_observer,
                output_observer,
                weak_doc,
            )
        });
    let output = output_observer.snapshot();
    assert!(
        weak_doc.upgrade().is_none()
            && source_observer.payload_released()
            && font_observer
                .iter()
                .all(|observer| observer.payload_released())
    );
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    assert!(font_bytes.iter().all(|observer| observer.unmap_succeeded()
        && observer.mapped_bytes() == 0
        && !observer.failed()));
    assert!(
        !output.file_open
            && output.mapped_bytes == 0
            && !output.cleanup_failed
            && !output.reader_failed
            && output.source_payload_released
    );
}

fn main() {
    let mtm = MainThreadMarker::new().expect("原生字体控制必须在实际主线程运行");
    table_cell::verify(mtm);
    finite_case(
        mtm,
        "office office office office office office office office ",
        180 * 64,
        false,
        false,
        MarkdownPresentationStyles::default(),
    );
    finite_case(
        mtm,
        "office אבג office",
        600 * 64,
        false,
        true,
        MarkdownPresentationStyles::default(),
    );
    finite_case(
        mtm,
        "office e\u{301} office",
        600 * 64,
        false,
        false,
        MarkdownPresentationStyles::default(),
    );
    finite_case(
        mtm,
        &"x ".repeat(2_500),
        100_000 * 64,
        true,
        false,
        MarkdownPresentationStyles::default(),
    );
    // 明确的typed样式只验证真实绘制原语；普通<u>与删除线语法尚无完整RootLeaf发布证明。
    finite_case(
        mtm,
        "XXXXX",
        600 * 64,
        false,
        false,
        MarkdownPresentationStyles::HIDDEN_MARKER,
    );
    finite_case(
        mtm,
        "     ",
        600 * 64,
        false,
        false,
        MarkdownPresentationStyles::UNDERLINE,
    );
    finite_case(
        mtm,
        "     ",
        600 * 64,
        false,
        false,
        MarkdownPresentationStyles::STRIKETHROUGH,
    );
    println!(
        "native_paged_line_viewport budget_rejected_before_draw=true actual_sources_unmapped=true actual_fonts_unmapped=true actual_output_fd_closed=true no_body_textkit=true formal=false"
    );
}
