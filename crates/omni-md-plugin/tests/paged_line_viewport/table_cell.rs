//! 真实 Parser publication 到只读格窗口；小夹具仅核原语，不能冒称产品 Grid 已完成。

use super::*;
use objc2_app_kit::NSLayoutManager;
use omni_markdown_core::{
    MarkdownPresentationMode, MarkdownTableCellDisplayPreparation,
    MarkdownTableCellDisplayPreparationStep, MarkdownTableCellDisplaySource, MarkdownTextSelection,
    markdown_document_buffer::{
        MarkdownTableRequestError, MarkdownViewportSourceAuthority, MarkdownViewportSourceStep,
        VerifiedMarkdownViewportSources,
    },
};
use omni_md_plugin::native_surface::paged_line_viewport::{
    NativeMarkdownComposedTableCell, NativeMarkdownComposedViewportPaintBudget as PaintBudget,
    NativeMarkdownComposedViewportPaintCompletion as Completion,
    NativeMarkdownComposedViewportPaintOutcome as PaintOutcome,
};

const TEXT: &str = "| H | C |\n| --- | --- |\n| **Bold** *Italic* [Link](https://example.com) `a\\|b` tail `#101` | second |\n";
const DISPLAY: &str = "Bold Italic Link a|b tail #101";

/// 原请求必须实际完成表格和 inline 发布；不手工填充样式、raw 范围或完成标记。
fn publication(
    document: &MarkdownDocumentBuffer,
) -> (
    Rc<VerifiedMarkdownViewportSources>,
    MarkdownViewportSourceAuthority,
) {
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 71,
        selection: document.selection(),
        cancelled: false,
    };
    let mut request = document
        .viewport_source_request(0..document.utf16_len(), 71, authority.selection)
        .unwrap();
    for _ in 0..1_000 {
        match request.advance(document, authority, 4096).unwrap() {
            MarkdownViewportSourceStep::Progress { .. } => {}
            MarkdownViewportSourceStep::SourcesAvailable { .. } => {
                if let Some(ready) = request.take_sources_for(document, authority).unwrap() {
                    return (Rc::new(ready), authority);
                }
            }
        }
    }
    panic!("有限原表格请求未完成，不能以 Pending 冒充发布");
}

/// 每个真实格独立推进原转换游标；只检查有业务意义的有界读取与最终显示内容。
fn display(
    document: &MarkdownDocumentBuffer,
    publication: Rc<VerifiedMarkdownViewportSources>,
    authority: MarkdownViewportSourceAuthority,
    column: usize,
) -> Rc<MarkdownTableCellDisplaySource> {
    let mut request =
        MarkdownTableCellDisplayPreparation::new(document, publication, authority, 0, 1, column)
            .unwrap();
    for _ in 0..1_000 {
        let (ready, work) = match request.advance(document, false).unwrap() {
            MarkdownTableCellDisplayPreparationStep::Progress { work } => (false, work),
            MarkdownTableCellDisplayPreparationStep::Ready { work } => (true, work),
        };
        assert!(work.source_bytes <= 4096 && work.copied_source_bytes <= 4096);
        assert!(work.structural_transitions <= 512);
        if ready {
            return Rc::new(request.take_ready_for(document).unwrap().unwrap());
        }
    }
    panic!("有限格显示来源未完成，不能签发部分显示源");
}

/// 响应每个真实样式请求时只消费原 display 目录交集；四个实际 Font 共用同一 catalog。
fn compose_display(
    display: &MarkdownTableCellDisplaySource,
    palette: &[Rc<NativeMarkdownLineFont>],
    catalog: MarkdownLineFontCatalog,
) -> MarkdownComposedLines {
    let mut composer = MarkdownLineComposer::new(
        display.source().clone(),
        &display.authority(),
        catalog,
        300 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    for _ in 0..10_000 {
        let answer = composer
            .step(MarkdownLineComposeBudget {
                bytes: 64,
                scalars: 16,
                records: 16,
                metadata_units: 16,
            })
            .unwrap()
            .answer;
        match answer {
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let requested = request.source_range();
                let styled = display.styles_at(requested.start).unwrap().unwrap();
                let coverage = requested.start..requested.end.min(styled.utf16_range.end);
                let styles = styled.styles;
                let slot = if styles.contains(MarkdownPresentationStyles::INLINE_CODE) {
                    3
                } else if styles.contains(MarkdownPresentationStyles::STRONG) {
                    1
                } else if styles.contains(MarkdownPresentationStyles::EMPHASIS) {
                    2
                } else {
                    0
                }; // 真正粗体、斜体与等宽 face 分开注册，不能给同一普通字形补样式位。
                let font = &palette[slot];
                composer
                    .provide_style(
                        request
                            .respond_registered(
                                coverage,
                                font.registered(),
                                MarkdownLineComposeStyle {
                                    presentation: styles,
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
            MarkdownLineComposeAnswer::NeedsControl(_) => {
                panic!("有限单行显示夹具没有控制符，不能伪造控制响应")
            }
            MarkdownLineComposeAnswer::NeedsResolution(_) => {
                panic!("有限空格断行夹具不接受未完成的边界解析")
            }
            // 本夹具未启用真实表格 flow，出现块行请求必须报错，不能吞作普通文字完成。
            MarkdownLineComposeAnswer::NeedsTableRow(_) => {
                panic!("非表格夹具出现真实表格行请求")
            }
            MarkdownLineComposeAnswer::NeedsTabPlacement(_) => {
                panic!("无 Tab 夹具不能伪造制表位置")
            }
            #[cfg(feature = "native-paged-frontier-probe")]
            MarkdownLineComposeAnswer::NeedsFrontier(_) => {
                panic!("完整小夹具不得把未完成整形前沿当作 EOF")
            }
        }
    }
    panic!("有限格的真实组合器未完成，不能截断剩余显示字形");
}

/// 有界 paint 必须走到实际完成；收据保留的 context claim 在调用者核完后立即归还。
fn paint(
    cell: &NativeMarkdownComposedTableCell,
    canvas: &mut Canvas,
    current: &dyn Fn() -> bool,
) -> Completion {
    cell.paint_code_backgrounds(&canvas.context, current)
        .unwrap();
    let mut session = cell.begin_paint(canvas.context.clone(), current).unwrap();
    for _ in 0..1_000 {
        let step = session
            .step(
                PaintBudget {
                    glyphs: 3,
                    metadata_records: 16,
                },
                current,
            )
            .unwrap();
        assert!(step.work.glyphs <= 3 && step.work.metadata_records <= 16);
        match step.outcome {
            PaintOutcome::Pending(next) => session = next,
            PaintOutcome::Complete(completion) => {
                assert!(completion.receipt().glyphs > 0);
                assert_eq!(completion.receipt().missing_glyphs, 0);
                assert!(canvas.ink() > 0);
                return completion;
            }
        }
    }
    panic!("真实格 paint 未终止，不能用前缀墨迹冒充完成");
}

/// canonical oracle 独立取固定夹具的真实 UTF16 位置，不从待测 projection 反推预期。
fn canonical(text: &str) -> usize {
    TEXT[..TEXT.find(text).unwrap()].encode_utf16().count()
}

/// 两个真实格共享外层 Scene 和字体目录，仍各守显示源、canonical 几何与完成收据身份。
pub(super) fn verify(mtm: MainThreadMarker) {
    let (mapping, weak_document, body_observer, display_observers, output_observers, fonts) =
        autoreleasepool(|_| {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            file.write_all(TEXT.as_bytes()).unwrap();
            let document = Rc::new(RefCell::new(
                MarkdownDocumentBuffer::open(file.path(), 65_536).unwrap(),
            ));
            let weak_document = Rc::downgrade(&document);
            let mapping = document.borrow().original_backing_observer().unwrap();
            let body = MarkdownLayoutTextSource::capture(&document.borrow());
            let body_observer = body.resource_observer();
            let (publication, authority) = publication(&document.borrow());
            let displays = [
                display(&document.borrow(), publication.clone(), authority, 0),
                display(&document.borrow(), publication, authority, 1),
            ];
            // 只有这个有限固定夹具可读完整显示 oracle；生产仍通过原分页 Source 消费。
            for (display, expected) in displays.iter().zip([DISPLAY, "second"]) {
                let mut bytes = Vec::new();
                display
                    .source()
                    .write_utf16_range_to(0..display.source().utf16_len(), &mut bytes)
                    .unwrap();
                assert_eq!(bytes, expected.as_bytes());
            }
            let display_observers = displays
                .each_ref()
                .map(|display| display.resource_observer());
            let catalog = MarkdownLineFontCatalog::new();
            let manager = NSLayoutManager::new();
            let mut fonts = Vec::new();
            let palette: Vec<_> = ["Helvetica", "Helvetica-Bold", "Helvetica-Oblique", "Menlo"]
                .into_iter()
                .map(|name| {
                    // SAFETY: 固定本地字体名与空 matrix 只在同步构造期借用，没有文档或用户路径。
                    let native = unsafe {
                        CTFont::with_name(&CFString::from_str(name), 16.0, std::ptr::null())
                    };
                    let captured =
                        NativeMarkdownPagedFont::capture(&native, 512 * 1024 * 1024, mtm, &|| true)
                            .unwrap();
                    fonts.push((
                        captured.bytes_observer(),
                        captured.core_font().resource_observer(),
                    ));
                    Rc::new(
                        NativeMarkdownLineFont::register(captured, &catalog, &|| true)
                            .unwrap()
                            .capture_preview_metrics(&manager, &|| true)
                            .unwrap(),
                    )
                })
                .collect();
            drop(manager); // 系统排版管理器只捕有限标量，不随行窗口保活。
            let outputs = displays.each_ref().map(|display| {
                Rc::new(RefCell::new(compose_display(
                    display,
                    &palette,
                    catalog.clone(),
                )))
            });
            let output_observers = outputs
                .each_ref()
                .map(|output| output.borrow().resource_observer());
            let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
            let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
            let nonce = NativeMarkdownPagedSceneNonce::new();
            let key = NativeMarkdownPagedSceneKey {
                presentation_mode: MarkdownPresentationMode::InlinePreview,
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
                source: body.authority(),
                nonce: nonce.authority(),
            };
            let current = || {
                // Native/current 回调不得跨持 Doc 或任一个真实 Reader 租约。
                assert!(document.try_borrow_mut().is_ok());
                assert!(outputs.iter().all(|output| output.try_borrow_mut().is_ok()));
                true
            };
            let cells = [0, 1].map(|column| {
                NativeMarkdownComposedTableCell::prepare(
                    outputs[column].clone(),
                    key.clone(),
                    displays[column].clone(),
                    palette.clone(),
                    handles(&outputs[column]),
                    NSRect::new(
                        NSPoint::new(16.0 + column as f64 * 340.0, 120.0),
                        NSSize::new(300.0, 160.0),
                    ),
                    &current,
                )
                .unwrap()
            });
            // 显示域 output 不得借真实 Body key 当作全文行窗口，哪怕 Doc/代次相同。
            assert!(matches!(
                NativeMarkdownComposedViewport::prepare(
                    outputs[0].clone(),
                    key.clone(),
                    palette.clone(),
                    handles(&outputs[0]),
                    NSPoint::new(16.0, 280.0),
                    &current
                ),
                Err(Error::Composed(MarkdownLineComposeError::SourceMismatch))
            ));
            let mut canvas = Canvas::new();
            let first = paint(&cells[0], &mut canvas, &current);
            let second = paint(&cells[1], &mut canvas, &current);
            assert!(cells[0].permits_completion(&first, &canvas.context));
            assert!(cells[1].permits_completion(&second, &canvas.context));
            assert!(!cells[0].permits_completion(&second, &canvas.context));
            assert!(!cells[1].permits_completion(&first, &canvas.context));
            assert!(
                !first.permits(&key, &canvas.context) && !second.permits(&key, &canvas.context)
            );
            let other_canvas = Canvas::new();
            assert!(!cells[0].permits_completion(&first, &other_canvas.context));
            // 普通单向字符的正中是合法鼠标落点；两端caret虽同距，却不是同一视觉位置。
            // 用原Parser的代码显示源与真实Menlo度量取得字符框，不能改点避开产品的平局分支，
            // 也不能把这种普通平局作为BiDi歧义传给Grid，令实际点击被拒绝。
            let position = canonical("#101");
            let character = cells[0]
                .first_rect(
                    NSRange::new(position, 1),
                    NativeMarkdownPagedHeadAffinity::Downstream,
                    &current,
                )
                .unwrap();
            assert_eq!(character.actual_range, NSRange::new(position, 1));
            assert!(character.rect.size.width > 0.0 && character.rect.size.height > 0.0);
            // 两端另取原公开零范围几何，证明这是不同视觉位置的普通首GCB，
            // 不靠UTF16差值或等宽字体名称猜caret间距，也不借被删除的代码标记充当端点。
            let start = cells[0]
                .first_rect(
                    NSRange::new(position, 0),
                    NativeMarkdownPagedHeadAffinity::Downstream,
                    &current,
                )
                .unwrap();
            let end = cells[0]
                .first_rect(
                    NSRange::new(position + 1, 0),
                    NativeMarkdownPagedHeadAffinity::Downstream,
                    &current,
                )
                .unwrap();
            assert_ne!(start.rect.origin.x, end.rect.origin.x);
            let midpoint = NSPoint::new(
                character.rect.origin.x + character.rect.size.width / 2.0,
                character.rect.origin.y + character.rect.size.height / 2.0,
            );
            let hit = cells[0].hit(midpoint, &current).unwrap();
            // 平局可沿原稳定主候选落在此字符的任一真实端点，不强制新的视觉导航规则。
            // 但另一个逻辑位置只能由真正重合的caret产生，否则Grid会拒绝合法单击。
            assert!(hit.source_utf16 == position || hit.source_utf16 == position + 1);
            assert!(
                hit.other_source_utf16.is_none(),
                "普通代码字符中点不能产生另一逻辑候选：hit={hit:?}, midpoint={midpoint:?}, start={:?}, end={:?}",
                start.rect,
                end.rect,
            );
            // 返回值必须是 canonical 字符范围，不能把去除 Markdown 标记后的局部下标交给 Find。
            for label in ["Bold", "Italic", "Link", "a\\|b", "tail"] {
                let position = canonical(label);
                let first = cells[0]
                    .first_rect(
                        NSRange::new(position, 1),
                        NativeMarkdownPagedHeadAffinity::Downstream,
                        &current,
                    )
                    .unwrap();
                assert_eq!(first.actual_range, NSRange::new(position, 1));
                assert!(first.rect.size.width > 0.0 && first.rect.size.height > 0.0);
                let hit = cells[0]
                    .hit(
                        NSPoint::new(
                            first.rect.origin.x,
                            first.rect.origin.y + first.rect.size.height / 2.0,
                        ),
                        &current,
                    )
                    .unwrap();
                assert!(hit.source_utf16 == position || hit.other_source_utf16 == Some(position));
                let middle = NSPoint::new(
                    first.rect.origin.x + first.rect.size.width / 2.0,
                    first.rect.origin.y + first.rect.size.height / 2.0,
                );
                let linked = cells[0].link_part_at(middle, &current).unwrap();
                if label == "Link" {
                    assert_eq!(linked, Some(position..position + 4));
                } else {
                    assert!(linked.is_none());
                }
            }
            // 最后一个可见字符的尾端必须停在闭合标记之前；不能把下一显示 run 的 raw.start 当成字符末端。
            for label in ["Bold", "Italic", "Link"] {
                let position = canonical(label) + label.encode_utf16().count() - 1;
                let last = cells[0]
                    .first_rect(
                        NSRange::new(position, 1),
                        NativeMarkdownPagedHeadAffinity::Downstream,
                        &current,
                    )
                    .unwrap();
                assert_eq!(last.actual_range, NSRange::new(position, 1));
                assert!(last.rect.size.width > 0.0 && last.rect.size.height > 0.0);
            }
            // 可见标签确实产生高亮，避免“所有选区都返回空”掩盖隐藏 URL 的边界错误。
            let link_start = canonical("Link");
            let mut visible_rects = 0;
            let visible_count = cells[0]
                .selection_rects(link_start..link_start + 4, &current, |rect| {
                    assert!(document.try_borrow_mut().is_ok());
                    assert!(outputs.iter().all(|output| output.try_borrow_mut().is_ok()));
                    assert!(rect.size.width > 0.0 && rect.size.height > 0.0);
                    visible_rects += 1;
                    Ok(())
                })
                .unwrap();
            assert!(visible_count > 0 && visible_count == visible_rects);
            // 仅选择原 URL 的 canonical 字节时没有显示字符；不得给后继空格或整个链接标签补高亮。
            let url = "https://example.com";
            let url_start = canonical(url);
            let mut hidden_rects = 0;
            let hidden_count = cells[0]
                .selection_rects(
                    url_start..url_start + url.encode_utf16().count(),
                    &current,
                    |_| {
                        hidden_rects += 1;
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!((hidden_count, hidden_rects), (0, 0));
            assert!(matches!(
                cells[0].first_rect(
                    NSRange::new(canonical("second"), 1),
                    NativeMarkdownPagedHeadAffinity::Downstream,
                    &current
                ),
                Err(Error::InvalidRange)
            ));
            // 同一个格源和 outer key 也不能互换另一个窗口实例的完成事实。
            let twin = NativeMarkdownComposedTableCell::prepare(
                outputs[0].clone(),
                key.clone(),
                displays[0].clone(),
                palette.clone(),
                handles(&outputs[0]),
                NSRect::new(NSPoint::new(16.0, 120.0), NSSize::new(300.0, 160.0)),
                &current,
            )
            .unwrap();
            assert!(!twin.permits_completion(&first, &canvas.context));
            let pending = cells[0]
                .begin_paint(canvas.context.clone(), &current)
                .unwrap();
            document
                .borrow_mut()
                .set_selection(MarkdownTextSelection::caret(1))
                .unwrap();
            assert!(matches!(
                cells[0].first_rect(
                    NSRange::new(canonical("Bold"), 1),
                    NativeMarkdownPagedHeadAffinity::Downstream,
                    &current
                ),
                Err(Error::Table(
                    MarkdownTableRequestError::PublicationSuperseded
                ))
            ));
            let failure = pending.step(PaintBudget::default(), &current).unwrap_err();
            assert!(matches!(
                failure.cause,
                NativeMarkdownComposedViewportPaintCause::Window(Error::Table(
                    MarkdownTableRequestError::PublicationSuperseded
                ))
            ));
            assert!(!failure.native_draw_returned);
            assert_eq!(std::fs::read(file.path()).unwrap(), TEXT.as_bytes());
            assert_eq!(document.borrow().generation(), 0);
            drop(first);
            drop(second);
            drop(twin);
            drop(cells);
            for output in outputs {
                Rc::try_unwrap(output)
                    .ok()
                    .unwrap()
                    .into_inner()
                    .close()
                    .unwrap();
            }
            drop(displays);
            drop(palette);
            drop(catalog);
            drop(input);
            drop(body);
            drop(document);
            (
                mapping,
                weak_document,
                body_observer,
                display_observers,
                output_observers,
                fonts,
            )
        });
    assert!(weak_document.upgrade().is_none() && body_observer.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    for observer in display_observers {
        let state = observer.snapshot();
        assert!(
            state.source_payload_released
                && !state.origin_file_open
                && state.origin_buffer_bytes == 0
        );
        assert!(!state.origin_cleanup_failed && !state.origin_reader_failed);
    }
    for observer in output_observers {
        let state = observer.snapshot();
        assert!(!state.file_open && state.mapped_bytes == 0 && state.source_payload_released);
        assert!(!state.cleanup_failed && !state.reader_failed);
    }
    for (bytes, payload) in fonts {
        assert!(bytes.unmap_succeeded() && bytes.mapped_bytes() == 0 && !bytes.failed());
        assert!(payload.payload_released());
    }
    println!(
        "native_table_display_cell real_publication=true real_style_requests=true canonical_geometry=true canonical_link_range=true cell_completion_isolated=true actual_resources_released=true product_grid_complete=false"
    );
}
