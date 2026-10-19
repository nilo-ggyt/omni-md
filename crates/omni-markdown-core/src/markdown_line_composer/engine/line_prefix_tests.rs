//! 真实Style→Frontier→Store→Measure调度；LB只在原查询等待后取得独立下一量子。
use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownLayoutFont};

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("按需断行.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        Self {
            _directory: directory,
            document,
            source,
        }
    }
    fn worker(
        &self,
        font: &MarkdownLayoutFont,
        frontier: bool,
        cancellation: MarkdownLineComposeCancellation,
    ) -> (MarkdownLineComposer, MarkdownLineRegisteredFont) {
        let catalog = MarkdownLineFontCatalog::new();
        let registered = catalog.register(font).unwrap();
        let constructor = if frontier {
            MarkdownLineComposer::new_with_frontier
        } else {
            MarkdownLineComposer::new
        };
        let worker = constructor(
            self.source.clone(),
            &self.source.authority(),
            catalog,
            80 * 64,
            cancellation,
        )
        .unwrap();
        (worker, registered)
    }
}

#[cfg(target_os = "macos")]
fn font() -> MarkdownLayoutFont {
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(
        std::path::Path::new("/System/Library/Fonts/SFNSMono.ttf"),
        32 * 1024 * 1024,
    )
    .unwrap();
    MarkdownLayoutFont::resolve(bytes.blob(), ".SFNSMono-Regular", &[]).unwrap()
}

fn style() -> MarkdownLineComposeStyle {
    MarkdownLineComposeStyle {
        points: 16.0,
        language: None,
        features: Vec::new(),
    }
}

fn source_read(worker: &mut MarkdownLineComposer) -> usize {
    worker
        .payload
        .as_mut()
        .unwrap()
        .coordinator
        .dependencies()
        .unwrap()
        .1
        .line_source_bytes_read_for_test()
}

fn respond(
    worker: &mut MarkdownLineComposer,
    request: MarkdownLineStyleRequest,
    font: &MarkdownLineRegisteredFont,
) {
    let range = request.source_range();
    worker
        .provide_style(request.respond_registered(range, font, style()).unwrap())
        .unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn need_style_reads_no_lb_source_and_first_line_work_is_equal_for_4k_8k_4m() {
    let font = font();
    let mut results = Vec::new(); // 只有三个固定测试结果，不进入产品的文本或历史缓存。
    for length in [4096usize, 8192, 4 * 1024 * 1024] {
        let mut text = "hello world ".repeat(length.div_ceil(12));
        text.truncate(length);
        let fixture = Fixture::new(&text);
        let backing = fixture.source.original_backing_observer().unwrap();
        let source_observer = fixture.source.resource_observer();
        let (mut worker, registered) =
            fixture.worker(&font, true, MarkdownLineComposeCancellation::new());
        let resources = worker.worker_resource_observer();
        let (mut bytes, mut first) = (0, None);
        for _ in 0..100_000 {
            let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
            assert!(
                progress.bytes <= 4096
                    && progress.scalars <= 256
                    && progress.records <= 256
                    && progress.shapes <= 1
            );
            bytes += progress.bytes;
            match progress.answer {
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    assert_eq!(source_read(&mut worker), 0);
                    respond(&mut worker, request, &registered);
                }
                MarkdownLineComposeAnswer::LineReady(line) => {
                    first = Some(line);
                    break;
                }
                MarkdownLineComposeAnswer::Yielded => {}
                _ => panic!("真实自然机会首行不能转为假EOF或未认证解析"),
            }
        }
        let first = first.expect("有限前沿必须实际发布LineEnd");
        let line_bytes = source_read(&mut worker);
        assert!(line_bytes > 0 && line_bytes <= 256 && line_bytes < length);
        results.push((
            line_bytes,
            bytes,
            first.source_utf16.end,
            worker.frontier_work().unwrap(),
        ));
        let output = worker.into_output().unwrap();
        assert!(output.source_range().end < length);
        output.close().unwrap();
        drop((registered, fixture));
        assert!(source_observer.payload_released() && resources.is_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    }
    assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    eprintln!("按需LB真实首行 work={results:?} whole_product_pass=false");
}

#[cfg(target_os = "macos")]
#[test]
fn pending_consumption_and_lb_progress_use_separate_quota_one_turns() {
    let font = font();
    let fixture = Fixture::new(&"hello world ".repeat(400));
    let (mut worker, registered) =
        fixture.worker(&font, true, MarkdownLineComposeCancellation::new());
    let budget = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    for _ in 0..200_000 {
        let progress = worker.step(budget).unwrap();
        assert!(
            progress.bytes <= 1
                && progress.scalars <= 1
                && progress.records <= 1
                && progress.metadata_units <= 1
                && progress.shapes <= 1
        );
        if let MarkdownLineComposeAnswer::NeedStyle(request) = progress.answer {
            respond(&mut worker, request, &registered);
        }
        if worker.payload.as_ref().unwrap().line_breaks_pending {
            let stage = std::mem::discriminant(&worker.payload.as_ref().unwrap().stage);
            let before = source_read(&mut worker);
            let next = worker.step(budget).unwrap();
            assert_eq!((next.shapes, next.metadata_units), (0, 0));
            assert!(next.bytes <= 1 && next.scalars <= 1 && next.records <= 1);
            assert_eq!(source_read(&mut worker) - before, next.bytes);
            assert_eq!(
                std::mem::discriminant(&worker.payload.as_ref().unwrap().stage),
                stage
            );
            assert!(!worker.payload.as_ref().unwrap().line_breaks_pending);
            return;
        }
    }
    panic!("实际Measure没有经过原LB等待和独立推进");
}

#[cfg(target_os = "macos")]
fn complete(
    fixture: &Fixture,
    font: &MarkdownLayoutFont,
    frontier: bool,
) -> Vec<(MarkdownComposedLineFacts, Vec<crate::MarkdownShapedGlyph>)> {
    let (mut worker, registered) =
        fixture.worker(font, frontier, MarkdownLineComposeCancellation::new());
    let mut finished = false;
    for _ in 0..200_000 {
        let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
        match progress.answer {
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                respond(&mut worker, request, &registered)
            }
            MarkdownLineComposeAnswer::Yielded | MarkdownLineComposeAnswer::LineReady(_) => {}
            MarkdownLineComposeAnswer::Complete => {
                finished = true;
                break;
            }
            _ => panic!("有限safe原字体不应丢功能或伪造边界"),
        }
    }
    assert!(finished);
    assert_eq!(source_read(&mut worker), fixture.source.byte_len());
    let mut output = worker.into_output().unwrap();
    let observer = output.resource_observer();
    let mut line = output.last_line().unwrap();
    let mut actual = Vec::new(); // 完整有限测试oracle允许留逐行值；产品仍使用唯一原FD。
    while let Some(current) = line {
        let mut glyphs = Vec::new();
        for index in 0..current.facts().parts {
            let part = output.part(&current, index).unwrap().unwrap();
            for ordinal in 0..part.facts().glyphs {
                let mut glyph = [crate::MarkdownShapedGlyph {
                    glyph_id: 0,
                    source_utf16_cluster: 0,
                    flags: 0,
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0,
                }];
                output
                    .read_batch(&part, ordinal..ordinal + 1, &mut glyph)
                    .unwrap();
                glyphs.push(glyph[0]);
            }
        }
        actual.push((current.facts().clone(), glyphs));
        line = output.previous_line(&current).unwrap();
    }
    output.close().unwrap();
    assert!(
        !observer.snapshot().file_open
            && observer.snapshot().mapped_bytes == 0
            && !observer.snapshot().cleanup_failed
    );
    actual.reverse();
    actual
}

#[cfg(target_os = "macos")]
#[test]
fn full_continuation_matches_complete_original_lb_and_shape_oracle() {
    let font = font();
    // 第二项复现编辑短前缀后留下长连续词的实际正文；完整模式是独立断行与字形对照，
    // 不把前沿自身的候选坐标当成正确结果。
    for text in [
        "hello (world) 12,345 next ".repeat(150),
        format!("one {}", "A".repeat(4099)),
    ] {
        let fixture = Fixture::new(&text);
        let actual = complete(&fixture, &font, true);
        let expected = complete(&fixture, &font, false);
        assert_eq!(actual.len(), expected.len());
        for ((line, glyphs), (oracle, expected_glyphs)) in actual.into_iter().zip(expected) {
            assert_eq!(
                (
                    line.source_utf16,
                    line.paragraph_utf16,
                    line.ordinal,
                    line.kind,
                    line.x_advance_26_6,
                    line.y_advance_26_6,
                    line.glyphs
                ),
                (
                    oracle.source_utf16,
                    oracle.paragraph_utf16,
                    oracle.ordinal,
                    oracle.kind,
                    oracle.x_advance_26_6,
                    oracle.y_advance_26_6,
                    oracle.glyphs
                )
            );
            assert_eq!(glyphs, expected_glyphs); // 全字段含原global cluster、offset、advance与unsafe flags。
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn complete_mode_and_non_uniform_rtl_still_prepare_the_complete_line_job_before_style() {
    let font = font();
    // 完整模式保持旧准入；含真实RTL的Source无法取得全源UniformZero无控制证书，
    // 仍须完成原依赖。普通LTR Greek的按需准入由item/open_prefix_tests独立覆盖。
    for (text, frontier) in [("hello world hello", false), ("hello אבג world", true)] {
        let fixture = Fixture::new(text);
        let (mut worker, _registered) =
            fixture.worker(&font, frontier, MarkdownLineComposeCancellation::new());
        let mut seen = false;
        for _ in 0..100_000 {
            if let MarkdownLineComposeAnswer::NeedStyle(_) = worker
                .step(MarkdownLineComposeBudget::default())
                .unwrap()
                .answer
            {
                assert_eq!(source_read(&mut worker), fixture.source.byte_len());
                seen = true;
                break;
            }
        }
        assert!(seen);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn source_eio_precedes_cancel_while_waiting_and_drops_every_actual_owner() {
    let font = font();
    let fixture = Fixture::new(&"hello world ".repeat(400));
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, registered) = fixture.worker(&font, true, cancellation.clone());
    let resources = worker.worker_resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut waiting = false;
    for _ in 0..100_000 {
        if let MarkdownLineComposeAnswer::NeedStyle(request) = worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            respond(&mut worker, request, &registered);
        }
        if worker.payload.as_ref().unwrap().line_breaks_pending {
            waiting = true;
            break;
        }
    }
    assert!(waiting);
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(matches!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    ));
    cancellation.cancel();
    assert!(matches!(
        worker.step(MarkdownLineComposeBudget::default()),
        Err(MarkdownLineComposeError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    let closed = resources.snapshot().unwrap();
    assert!(!closed.line_breaks.unwrap().metadata_file_open && !closed.writer.unwrap().file_open);
    assert_eq!(closed.shapes.active, 0);
    drop((worker, registered, fixture));
    let released = resources.snapshot().unwrap();
    assert!(released.source_payload_released);
    assert_eq!(released.first_source_errno, Some(libc::EIO));
    assert!(!resources.is_released()); // 真实归还不洗掉首I/O失败；总成功票必须继续失败。
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0);
}

#[test]
#[ignore = "需要真实Native Body14字体输入；设置OMNI_FRONTIER_UIFONT_FIXTURES"]
fn actual_body14_natural_and_unbroken_first_line_lb_work_is_equal_for_4k_8k_4m() {
    // 原UIFont控制的name表、轴和ptem保持一起；不以同名字体或mono替代真实正文。
    let root = std::path::PathBuf::from(
        std::env::var_os("OMNI_FRONTIER_UIFONT_FIXTURES").expect("需要真实UIFont输入"),
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("fonts.json")).unwrap()).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["label"] == "body_system")
        .unwrap();
    assert_eq!(row["points"].as_f64().unwrap(), 14.0);
    let axes: Vec<_> = row["axes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|axis| crate::MarkdownLayoutFontAxis {
            tag: (axis["tag"].as_u64().unwrap() as u32).to_be_bytes(),
            value: axis["value"].as_f64().unwrap() as f32,
        })
        .collect();
    let font = MarkdownLayoutFont::resolve_name_table_at_size(
        std::sync::Arc::new(std::fs::read(row["path"].as_str().unwrap()).unwrap()),
        &std::fs::read(root.join(row["name_file"].as_str().unwrap())).unwrap(),
        &axes,
        14.0,
    )
    .unwrap();
    for (prefix, unit) in [("", "hello world "), ("", "A"), ("one ", "A")] {
        let mut observed = Vec::new();
        for length in [4096usize, 8192, 4 * 1024 * 1024] {
            let mut text = unit.repeat(length.div_ceil(unit.len()));
            text.truncate(length);
            // 有限测试输入保留真实短词接长词的接缝；它的自然断点落在原字形分片末端，
            // 不能通过改分隔符或缩小长词来避开生产中的续项路径。
            text.insert_str(0, prefix);
            let fixture = Fixture::new(&text);
            let (mut worker, registered) =
                fixture.worker(&font, true, MarkdownLineComposeCancellation::new());
            let mut first = None;
            let mut bytes = 0;
            for _ in 0..200_000 {
                let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
                bytes += progress.bytes;
                match progress.answer {
                    MarkdownLineComposeAnswer::Yielded => {}
                    MarkdownLineComposeAnswer::NeedStyle(request) => {
                        assert_eq!(source_read(&mut worker), 0);
                        let range = request.source_range();
                        worker
                            .provide_style(
                                request
                                    .respond_registered(
                                        range,
                                        &registered,
                                        MarkdownLineComposeStyle {
                                            points: 14.0,
                                            language: None,
                                            features: Vec::new(),
                                        },
                                    )
                                    .unwrap(),
                            )
                            .unwrap();
                    }
                    MarkdownLineComposeAnswer::LineReady(line) => {
                        first = Some(line);
                        break;
                    }
                    _ => panic!("实际Body14已认证字体必须保留原自然/紧急选行"),
                }
            }
            let first = first.expect("实际Body14必须签真实LineEnd");
            if !prefix.is_empty() {
                assert_eq!(first.source_utf16, 0..prefix.len());
                assert_eq!(first.kind, MarkdownComposedLineKind::Allowed);
            }
            let line_bytes = source_read(&mut worker);
            assert!(line_bytes > 0 && line_bytes <= 256 && line_bytes < length);
            observed.push((
                line_bytes,
                bytes,
                first.source_utf16.end,
                first.x_advance_26_6,
                worker.frontier_work().unwrap(),
            ));
            worker.into_output().unwrap().close().unwrap();
        }
        assert!(observed.windows(2).all(|pair| pair[0] == pair[1]));
        eprintln!(
            "actual_body14_lazy_lb prefix={prefix:?} unit={unit:?} work={observed:?} whole_product_pass=false"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn cancel_before_the_deferred_lb_turn_performs_no_read_and_releases_real_resources() {
    let font = font();
    let fixture = Fixture::new(&"hello world ".repeat(400));
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, registered) = fixture.worker(&font, true, cancellation.clone());
    let resources = worker.worker_resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let mut waiting = false;
    for _ in 0..100_000 {
        if let MarkdownLineComposeAnswer::NeedStyle(request) = worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            respond(&mut worker, request, &registered);
        }
        if worker.payload.as_ref().unwrap().line_breaks_pending {
            waiting = true;
            break;
        }
    }
    assert!(waiting && source_read(&mut worker) == 0);
    cancellation.cancel();
    assert!(matches!(
        worker.step(MarkdownLineComposeBudget::default()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    let closed = resources.snapshot().unwrap();
    assert!(!closed.line_breaks.unwrap().metadata_file_open);
    assert_eq!(closed.line_breaks.unwrap().metadata_peak_map_bytes, 0); // 原LB尚未写首机会页。
    assert!(!closed.writer.unwrap().file_open && closed.shapes.active == 0);
    drop((worker, registered, fixture));
    assert!(resources.is_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}
