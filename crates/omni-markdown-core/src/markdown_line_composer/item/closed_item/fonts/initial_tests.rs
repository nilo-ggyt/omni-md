//! Real AAT plan rejection and font boundaries. The long suffix stays mmap-backed;
//! neither an open endpoint nor repeated same-font trials establish a closed run.

use super::*;
use crate::MarkdownDocumentBuffer;
use std::io::Write;

const MORX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/tests/fonts/text-rendering-tests/TestMORXOne.ttf"
));
const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));

/// Write the fixed 64-KiB suffix in bounded blocks. Only the first two ASCII
/// graphemes distinguish actual font coverage; no whole-tail shaping oracle is built.
fn fixture(
    prefix: &[u8],
) -> (
    tempfile::NamedTempFile,
    MarkdownDocumentBuffer,
    MarkdownLayoutTextSource,
) {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(prefix).unwrap();
    let block = [b'A'; 4096];
    for _ in 0..16 {
        file.write_all(&block).unwrap();
    }
    let document = MarkdownDocumentBuffer::open(file.path(), 128 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    (file, document, source)
}

/// Resolve the actual SFNT face through its name table without modifying glyphs,
/// cmap, shaping tables, or the real plan's rejection result.
fn font(bytes: &'static [u8]) -> MarkdownLayoutFont {
    let name = read_fonts::FontRef::from_index(bytes, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    MarkdownLayoutFont::resolve_name_table_at_size(Arc::new(bytes), &name, &[], 16.0).unwrap()
}

/// Share Latin style, paragraph direction, and actual BOT/EOT rules between the
/// complete-grapheme short oracle and the candidate.
fn options(source: &MarkdownLayoutTextSource, range: &Range<usize>) -> MarkdownShapingOptions {
    MarkdownShapingOptions {
        direction: MarkdownShapingDirection::LeftToRight,
        script: *b"Latn",
        language: None,
        points: 16.0,
        beginning_of_text: range.start == 0,
        end_of_text: range.end == source.utf16_len(),
        features: Vec::new(),
    }
}

/// Obtain independent real glyphs from the original complete Job for short font
/// runs only. The long suffix never becomes complete shaping input.
fn glyphs(
    source: &MarkdownLayoutTextSource,
    font: &MarkdownLayoutFont,
    range: Range<usize>,
) -> Vec<MarkdownShapedGlyph> {
    assert!(
        range.len() <= 16,
        "独立完整 oracle 只接收夹具中的真实短 Font 段"
    );
    let authority = source.authority();
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &authority,
        font.clone(),
        &font.authority(),
        range.clone(),
        options(source, &range),
    )
    .unwrap();
    // Prepare through the original bounded Source reader before actual shaping;
    // no coordinator state or expected glyph record is injected into this oracle.
    for _ in 0..10000 {
        if job
            .step(
                &authority,
                &font.authority(),
                MarkdownShapingBudget {
                    bytes: 4096,
                    scalars: 256,
                },
            )
            .unwrap()
            .status
            == MarkdownShapingStatus::AwaitingShape
        {
            break;
        }
    }
    job.shape(&authority, &font.authority()).unwrap();
    let count = job.profile().unwrap().glyph_count;
    (0..count)
        .map(|ordinal| {
            job.glyph(&authority, &font.authority(), ordinal)
                .unwrap()
                .unwrap()
        })
        .collect()
}

/// Advance the original coordinator to NeedStyle while respecting its separate
/// byte, scalar, record, and metadata quanta on every step.
fn request(worker: &mut MarkdownLineItemCoordinator) -> MarkdownLineStyleRequest {
    let budget = MarkdownLineComposeBudget::default();
    for _ in 0..100000 {
        let step = worker.step(budget).unwrap();
        assert!(
            step.bytes <= budget.bytes
                && step.scalars <= budget.scalars
                && step.records <= budget.records
                && step.metadata_units <= budget.metadata_units
        );
        match step.answer {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => return request,
            _ => panic!("实际Font请求前不能发布原项或伪造EOF"),
        }
    }
    panic!("原有限Font请求未完成");
}

/// Reply once with the actual font while preserving the original requested range,
/// paragraph, cancellation authority, and registration ownership.
fn respond(
    worker: &mut MarkdownLineItemCoordinator,
    request: MarkdownLineStyleRequest,
    font: &MarkdownLayoutFont,
) {
    let range = request.source_range();
    worker
        .provide_style(
            request
                .respond(
                    range,
                    font.clone(),
                    MarkdownLineComposeStyle {
                        presentation: Default::default(),
                        points: 16.0,
                        language: None,
                        features: Vec::new(),
                    },
                )
                .unwrap(),
        )
        .unwrap();
}

#[test]
/// The AAT font covers A; actual glyph zero for x selects another font. Emit only
/// the naturally closed head, then return the long suffix to original itemization.
fn actual_first_font_boundary_emits_only_closed_head_then_restarts_original_item() {
    let (file, document, source) = fixture(b"Ax");
    let original = font(MORX);
    let next = font(ROBOTO);
    assert!(matches!(
        crate::markdown_layout_font::frontier::FontFrontierPlan::new(
            &original,
            &original.authority(),
            harfrust::Direction::LeftToRight,
            harfrust::Script::LATIN,
            None,
            &[]
        ),
        Err(harfrust::AsciiFrontierRejection::Positioning)
    ));
    // Establish both font coverages with real shaping before answering trials. The
    // original font's plan rejection is independent of its missing x glyph.
    let oracle = glyphs(&source, &original, 0..1);
    assert!(oracle.iter().all(|glyph| glyph.glyph_id != 0));
    assert!(
        glyphs(&source, &original, 1..2)
            .iter()
            .any(|glyph| glyph.glyph_id == 0)
    );
    assert!(
        glyphs(&source, &next, 1..2)
            .iter()
            .all(|glyph| glyph.glyph_id != 0)
    );
    let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
        source.clone(),
        &source.authority(),
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let resources = worker.resources.observer();
    let initial = request(&mut worker);
    assert!(initial.source_range().end > MAX_CLOSED_UTF16 - 1);
    respond(&mut worker, initial, &original);
    let first = request(&mut worker);
    assert_eq!(first.source_range(), 0..1);
    assert!(
        first
            .continuation_font()
            .unwrap()
            .same_payload(&original.authority())
    );
    // Consuming this exact request expires its old probe; the next grapheme must
    // obtain its own authority rather than reuse coordinates from the first trial.
    let stale_probe = worker.font_probe(&first).unwrap();
    respond(&mut worker, first, &original);
    assert!(matches!(
        stale_probe.validate(),
        Err(MarkdownLineComposeError::Poisoned)
    ));
    drop(stale_probe);
    let following = request(&mut worker);
    assert_eq!(following.source_range(), 1..2);
    respond(&mut worker, following, &next);
    let mut ready = None;
    for _ in 0..100000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::ItemReady(facts) => {
                assert_eq!(facts.source_utf16, 0..1);
                let mut item = worker.take_item().unwrap();
                assert!(original.permits(&item.font));
                let count = item.job.profile().unwrap().glyph_count;
                let actual: Vec<_> = (0..count)
                    .map(|index| item.glyph(index).unwrap().unwrap())
                    .collect();
                assert_eq!(actual, oracle);
                ready = Some(item);
                break;
            }
            _ => panic!("真实Font边界必须交付有限首段，不能再扩大成整个长项"),
        }
    }
    let item = ready.expect("真实短Font段未在有限工作内交付");
    // The consumer holds the real child allocation. A subsequent coordinator step
    // must wait until that child is dropped and its file/mapping release is observed.
    let shape = item.job.resource_observer();
    assert!(matches!(
        worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer,
        MarkdownLineItemAnswer::NeedsFrontier(MarkdownLineFrontierReason::PendingConsumer)
    ));
    drop(item);
    let actual = shape.snapshot();
    assert!(
        actual.files_open == 0
            && actual.mappings_open == 0
            && actual.mapped_bytes == 0
            && !actual.cleanup_failed
    );
    let continuation = request(&mut worker);
    assert_eq!(continuation.source_range().start, 1);
    assert!(continuation.source() == &source.authority());
    drop((continuation, worker, source, document, file, original, next));
    assert!(resources.snapshot().unwrap().is_released());
}

#[test]
/// Repeated same-font success cannot turn the 256-unit work allowance into a
/// boundary. Without a real transition, retain the original capability rejection.
fn unchanged_first_font_cannot_publish_an_open_prefix_as_a_closed_run() {
    let (file, document, source) = fixture(b"A");
    let original = font(MORX);
    let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
        source.clone(),
        &source.authority(),
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let initial = request(&mut worker);
    respond(&mut worker, initial, &original);
    let mut rejected = false;
    for _ in 0..100000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => respond(&mut worker, request, &original),
            MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                // The actual font-plan rejection and the same-font work budget are
                // distinct facts. Preserve the complete typed rejection reason.
                assert_eq!(
                    reason,
                    MarkdownLineFrontierReason::Plan(harfrust::AsciiFrontierRejection::Positioning)
                );
                rejected = true;
                break;
            }
            _ => panic!("没有真实Font边界时不得交付任何预算伪段"),
        }
    }
    assert!(rejected);
    drop((worker, source, document, file, original));
}

#[test]
/// A shortened semantic style still needs a real font transition. Choosing a font
/// for its first grapheme cannot hide later missing glyphs or certify its short end.
fn shortened_style_head_uses_real_gcb_font_boundary_and_keeps_same_font_rejection() {
    for (text, changes_font) in [("Ax*A*", true), ("AA*A*", false)] {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(text.as_bytes()).unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 64 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let source_observer = source.resource_observer();
        let backing = document.original_backing_observer().unwrap();
        let original = font(MORX);
        let fallback = font(ROBOTO);
        let original_observer = original.resource_observer();
        let fallback_observer = fallback.resource_observer();

        // The real parser ends this body run at the following emphasis marker; its
        // coverage was not arbitrarily shortened to fit the closed-item budget.
        let mut semantics = crate::MarkdownViewportPresentation::default();
        semantics
            .rebuild(document.generation(), 0..document.utf16_len(), text)
            .unwrap();
        let coverage = semantics.query(0).unwrap().utf16_range;
        assert_eq!(coverage, 0..2);
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            source.clone(),
            &source.authority(),
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let resources = worker.resources.observer();
        let initial = request(&mut worker);
        assert!(initial.source_range().end > coverage.end);
        assert!(initial.paragraph_range().len() < MAX_CLOSED_UTF16);
        worker
            .provide_style(
                initial
                    .respond(
                        coverage,
                        original.clone(),
                        MarkdownLineComposeStyle {
                            presentation: Default::default(),
                            points: 16.0,
                            language: None,
                            features: Vec::new(),
                        },
                    )
                    .unwrap(),
            )
            .unwrap();
        let limit = MarkdownLineComposeBudget {
            bytes: 7,
            scalars: 3,
            records: 3,
            metadata_units: 3,
        };
        let mut completed = false;
        for _ in 0..100_000 {
            let progress = worker.step(limit).unwrap();
            assert!(progress.bytes <= limit.bytes && progress.scalars <= limit.scalars);
            assert!(
                progress.records <= limit.records
                    && progress.metadata_units <= limit.metadata_units
            );
            match progress.answer {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    let probe = worker.font_probe(&request).unwrap();
                    let range = probe.source_range();
                    assert!(range == (0..1) || range == (1..2));
                    // Shape the same complete grapheme with the original font;
                    // select fallback only after actual glyph zero, then verify that
                    // the selected payload really covers the complete grapheme.
                    let primary = glyphs(probe.source().unwrap(), &original, range.clone());
                    let selected = if primary.iter().any(|glyph| glyph.glyph_id == 0) {
                        assert!(changes_font && range == (1..2));
                        assert!(
                            glyphs(probe.source().unwrap(), &fallback, range)
                                .iter()
                                .all(|glyph| glyph.glyph_id != 0)
                        );
                        &fallback
                    } else {
                        &original
                    };
                    respond(&mut worker, request, selected);
                    assert!(probe.validate().is_err()); // Consumed nonces cannot bless later trials.
                }
                MarkdownLineItemAnswer::ItemReady(facts) => {
                    assert!(changes_font);
                    assert_eq!(facts.source_utf16, 0..1);
                    let mut item = worker.take_item().unwrap();
                    let count = item.job.profile().unwrap().glyph_count;
                    let actual = (0..count)
                        .map(|ordinal| item.glyph(ordinal).unwrap().unwrap())
                        .collect::<Vec<_>>();
                    assert_eq!(actual, glyphs(&source, &original, 0..1));
                    assert!(matches!(
                        worker.step(limit).unwrap().answer,
                        MarkdownLineItemAnswer::NeedsFrontier(
                            MarkdownLineFrontierReason::PendingConsumer
                        )
                    ));
                    drop(item); // A font transition does not waive actual child release.
                    assert_eq!(request(&mut worker).source_range().start, 1);
                    completed = true;
                    break;
                }
                MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                    assert!(!changes_font);
                    assert_eq!(
                        reason,
                        MarkdownLineFrontierReason::Plan(
                            harfrust::AsciiFrontierRejection::Positioning
                        )
                    ); // Same-font graphemes cannot use short coverage as natural closure.
                    completed = true;
                    break;
                }
                _ => panic!("尚未消费的原样式尾部不能被宣布完整"),
            }
        }
        assert!(completed);
        // Actual source bytes and backing/font/worker resources remain intact after
        // either a successful transition or the unchanged typed plan rejection.
        assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
        drop((worker, source, document, file, original, fallback));
        assert!(resources.snapshot().unwrap().is_released());
        assert!(source_observer.payload_released());
        assert!(original_observer.payload_released() && fallback_observer.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    }
}

#[test]
/// fallback本身无缺字也必须恢复实际可用的base；整个自然窗口仍同Font时保留原完整字形结果。
fn preferred_font_policy_restores_primary_after_fallback_and_preserves_unchanged_runs() {
    for (text, expected) in [
        ("xA", vec![0..1, 1..2]),
        ("xx", std::iter::once(0..2).collect()),
    ] {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(text.as_bytes()).unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 64 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let source_observer = source.resource_observer();
        let backing = document.original_backing_observer().unwrap();
        let primary = font(MORX);
        let fallback = font(ROBOTO);
        let primary_observer = primary.resource_observer();
        let fallback_observer = fallback.resource_observer();
        let catalog = super::super::super::super::MarkdownLineFontCatalog::new();
        let primary_registered = catalog.register(&primary).unwrap();
        let fallback_registered = catalog.register(&fallback).unwrap();
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            source.clone(),
            &source.authority(),
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let resources = worker.resources.observer();
        let mut emitted = Vec::new();
        let mut ended = false;
        for _ in 0..100_000 {
            let progress = worker.step(MarkdownLineComposeBudget::default()).unwrap();
            match progress.answer {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    let probe = worker.font_probe(&request).unwrap();
                    let primary_glyphs =
                        glyphs(probe.source().unwrap(), &primary, probe.source_range());
                    // 两个真实face都可能覆盖A；策略先实际试base，只有真实缺字才采用fallback。
                    // 这正是“CJK fallback也覆盖空格”时必须保留的优先权，不能沿上个Font继续猜。
                    let selected = if primary_glyphs.iter().any(|glyph| glyph.glyph_id == 0) {
                        assert!(
                            glyphs(probe.source().unwrap(), &fallback, probe.source_range())
                                .iter()
                                .all(|glyph| glyph.glyph_id != 0)
                        );
                        &fallback_registered
                    } else {
                        &primary_registered
                    };
                    worker
                        .provide_style(
                            request
                                .respond_registered(
                                    range,
                                    selected,
                                    MarkdownLineComposeStyle {
                                        presentation: Default::default(),
                                        points: 16.0,
                                        language: None,
                                        features: Vec::new(),
                                    },
                                )
                                .unwrap()
                                .with_preferred_registered_font(&primary_registered)
                                .unwrap(),
                        )
                        .unwrap();
                    assert!(probe.validate().is_err());
                }
                MarkdownLineItemAnswer::ItemReady(facts) => {
                    let mut item = worker.take_item().unwrap();
                    let selected = if text == "xA" && facts.source_utf16.start == 1 {
                        &primary
                    } else {
                        &fallback
                    };
                    assert!(selected.permits(&item.font));
                    let actual = (0..item.job.profile().unwrap().glyph_count)
                        .map(|ordinal| item.glyph(ordinal).unwrap().unwrap())
                        .collect::<Vec<_>>();
                    assert_eq!(
                        actual,
                        glyphs(&source, selected, facts.source_utf16.clone())
                    );
                    assert!(actual.iter().all(|glyph| glyph.glyph_id != 0));
                    emitted.push(facts.source_utf16);
                    drop(item);
                }
                MarkdownLineItemAnswer::EndOfSource => {
                    ended = true;
                    break;
                }
                MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                    panic!("原自然项的真实base策略不能丢失已可用Font: {reason:?}")
                }
                _ => panic!("纯文本小项没有另一个控制协议"),
            }
        }
        assert!(ended);
        assert_eq!(emitted, expected);
        assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
        drop((worker, source, document, file));
        drop((
            primary_registered,
            fallback_registered,
            catalog,
            primary,
            fallback,
        ));
        assert!(resources.snapshot().unwrap().is_released());
        assert!(source_observer.payload_released());
        assert!(primary_observer.payload_released() && fallback_observer.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    }
}

#[cfg(target_os = "macos")]
#[test]
/// 从实际首safePart定位后继起点，验证其首GCB恢复base时不误报异源，也不假消费整part。
fn preferred_font_restores_primary_at_real_bounded_frontier_start() {
    // 第一份样本只提供真实计划选择的公开边界，第二份是独立Doc/Source；不能把前次nonce带入本次。
    // 夹具始终按4KiB写入，候选只消费有限首部，不为了构造问题预解析或加载整段。
    let mut boundary = None;
    for restore_primary in [false, true] {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let block = [b'x'; 4096];
        for _ in 0..16 {
            file.write_all(&block).unwrap();
        }
        if restore_primary {
            // 位置只来自前一独立样本公开交付的真实part；不读取协调器游标或修改私有状态。
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(boundary.expect("已消费原首part") as u64))
                .unwrap();
            file.write_all(b"A").unwrap();
        }
        let document = MarkdownDocumentBuffer::open(file.path(), 128 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let observed = source.resource_observer();
        let backing = document.original_backing_observer().unwrap();
        let primary = font(MORX);
        let primary_observer = primary.resource_observer();
        // MORX确实不覆盖x，原实机Mono确实覆盖它；不修改cmap、AAT计划或Font名称来制造变化。
        // 原文件快照与Core payload各有真实收据，最后必须同时看到close/munmap完成。
        let bytes = crate::MarkdownLayoutFontBytes::capture_file(
            std::path::Path::new("/System/Library/Fonts/SFNSMono.ttf"),
            32 * 1024 * 1024,
        )
        .unwrap();
        let mapping = bytes.resource_observer();
        let name = read_fonts::FontRef::from_index(bytes.as_ref(), 0)
            .unwrap()
            .table_data(read_fonts::types::Tag::new(b"name"))
            .unwrap()
            .as_bytes()
            .to_vec();
        let fallback =
            MarkdownLayoutFont::resolve_name_table_at_size(bytes.blob(), &name, &[], 16.0).unwrap();
        drop(bytes); // Font是这份真实冻结映射的唯一业务owner，临时capture不能保额外强边。
        let fallback_observer = fallback.resource_observer();
        // 和正式调用相同，palette持强注册，响应只带弱身份；优先策略不能额外延长字体映射。
        let catalog = super::super::super::super::MarkdownLineFontCatalog::new();
        let primary_registered = catalog.register(&primary).unwrap();
        let fallback_registered = catalog.register(&fallback).unwrap();
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            source.clone(),
            &source.authority(),
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let resources = worker.resources.observer();
        let budget = MarkdownLineComposeBudget::default();
        let mut ranges = Vec::new();
        let mut restored = false;
        for _ in 0..100_000 {
            let step = worker.step(budget).unwrap();
            assert!(step.bytes <= budget.bytes && step.scalars <= budget.scalars);
            assert!(step.records <= budget.records && step.metadata_units <= budget.metadata_units);
            match step.answer {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    let probe = worker.font_probe(&request).unwrap();
                    let actual = glyphs(probe.source().unwrap(), &primary, probe.source_range());
                    // 每一个原完整GCB实际试base；命中A时立即恢复，不能因为Mono也覆盖A而沿用fallback。
                    // 只有真实glyph0才选另一font，短oracle本身不预判长run末端或伪造闭项权限。
                    let selected = if actual.iter().any(|glyph| glyph.glyph_id == 0) {
                        assert!(
                            glyphs(probe.source().unwrap(), &fallback, probe.source_range())
                                .iter()
                                .all(|glyph| glyph.glyph_id != 0)
                        );
                        &fallback_registered
                    } else {
                        &primary_registered
                    };
                    worker
                        .provide_style(
                            request
                                .respond_registered(
                                    range,
                                    selected,
                                    MarkdownLineComposeStyle {
                                        presentation: Default::default(),
                                        points: 16.0,
                                        language: None,
                                        features: Vec::new(),
                                    },
                                )
                                .unwrap()
                                .with_preferred_registered_font(&primary_registered)
                                .unwrap(),
                        )
                        .unwrap();
                    assert!(probe.validate().is_err());
                }
                MarkdownLineItemAnswer::ItemReady(facts) => {
                    let mut item = worker.take_item().unwrap();
                    let range = facts.source_utf16.clone();
                    if ranges.is_empty() {
                        // 首项允许实际计划选择任意有界长度；断言用户可见输出及独立字形结果，不固化内部块长。
                        assert_eq!(range.start, 0);
                        assert!(range.end > 1 && range.end <= MAX_CLOSED_UTF16);
                        assert!(fallback.permits(&item.font));
                        if restore_primary {
                            assert_eq!(range.end, boundary.unwrap());
                        }
                        // 独立完整oracle只处理已真实交付的有界首part，绝不把64KiB源交给完整整形。
                        let mut oracle = MarkdownShapingJob::new(
                            source.clone(),
                            &source.authority(),
                            fallback.clone(),
                            &fallback.authority(),
                            range.clone(),
                            options(&source, &range),
                        )
                        .unwrap();
                        for _ in 0..10_000 {
                            if oracle
                                .step(
                                    &source.authority(),
                                    &fallback.authority(),
                                    MarkdownShapingBudget {
                                        bytes: budget.bytes,
                                        scalars: budget.scalars,
                                    },
                                )
                                .unwrap()
                                .status
                                == MarkdownShapingStatus::AwaitingShape
                            {
                                break;
                            }
                        }
                        oracle
                            .shape(&source.authority(), &fallback.authority())
                            .unwrap();
                        let count = oracle.profile().unwrap().glyph_count;
                        assert_eq!(item.job.profile().unwrap().glyph_count, count);
                        for index in 0..count {
                            assert_eq!(
                                item.glyph(index).unwrap(),
                                oracle
                                    .glyph(&source.authority(), &fallback.authority(), index)
                                    .unwrap()
                            );
                        }
                        if !restore_primary {
                            boundary = Some(range.end);
                        }
                    } else {
                        // 恢复base的首GCB恰在原真实part后；其右侧x又是真Font变化，A只能作为独立自然短段。
                        assert!(restore_primary);
                        assert_eq!(range, boundary.unwrap()..boundary.unwrap() + 1);
                        assert!(primary.permits(&item.font));
                        let actual = (0..item.job.profile().unwrap().glyph_count)
                            .map(|index| item.glyph(index).unwrap().unwrap())
                            .collect::<Vec<_>>();
                        assert_eq!(actual, glyphs(&source, &primary, range.clone()));
                        restored = true;
                    }
                    ranges.push(range);
                    drop(item); // 原child真实归还后才可退役旧parent；不通过游标或计数假装完成。
                    if !restore_primary || restored {
                        break;
                    }
                }
                MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                    panic!("有界首GCB恢复base不能拒绝：{reason:?}; actual={ranges:?}")
                }
                _ => panic!("只消费有限首part及恢复base的A，未读长尾不能成为EOF"),
            }
        }
        assert!(!ranges.is_empty() && (!restore_primary || restored));
        // 分块核对原磁盘；不为数据完整性检查再把整份样本放入内存。
        use std::io::{Read, Seek, SeekFrom};
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut read = [0; 4096];
        for block_start in (0..65536).step_by(read.len()) {
            file.read_exact(&mut read).unwrap();
            for (offset, byte) in read.iter().enumerate() {
                let expected = if restore_primary && block_start + offset == boundary.unwrap() {
                    b'A'
                } else {
                    b'x'
                };
                assert_eq!(*byte, expected);
            }
        }
        drop((
            worker,
            source,
            document,
            file,
            primary_registered,
            fallback_registered,
            catalog,
            primary,
            fallback,
        ));
        // worker、Doc、palette与所有临时Font强边均已归还；资源结论来自原实际observer，不拿计数推导。
        assert!(resources.snapshot().unwrap().is_released());
        assert!(
            observed.payload_released()
                && primary_observer.payload_released()
                && fallback_observer.payload_released()
        );
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
        assert!(
            mapping.mapping_drop_completed()
                && mapping.unmap_succeeded()
                && mapping.mapped_bytes() == 0
        );
        assert!(
            mapping.source_descriptor_closed()
                && mapping.backing_descriptor_closed()
                && !mapping.failed()
        );
    }
}

#[test]
/// 原首Font试次取消后撤销旧票；真实Source回收EIO仍优先于取消，不能落入普通拒绝。
fn original_source_error_precedes_cancel_during_initial_font_probe() {
    for poison in [false, true] {
        let (file, document, source) = fixture(b"Ax");
        let original = font(MORX);
        let cancellation = MarkdownLineComposeCancellation::new();
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            source.clone(),
            &source.authority(),
            cancellation.clone(),
        )
        .unwrap();
        let initial = request(&mut worker);
        respond(&mut worker, initial, &original);
        let first = request(&mut worker);
        let probe = worker.font_probe(&first).unwrap();
        if poison {
            source.visit_byte_range(0..1, |_| Ok(())).unwrap();
            document.fail_next_original_reclaim_for_test();
            assert!(document.reclaim_original_read_windows_for_test().is_err());
        }
        cancellation.cancel();
        let error = worker
            .step(MarkdownLineComposeBudget::default())
            .err()
            .unwrap();
        if poison {
            assert!(matches!(
                error,
                MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                )
            ));
        } else {
            assert!(matches!(error, MarkdownLineComposeError::Cancelled));
        }
        assert!(worker.payload.is_none() && probe.validate().is_err());
        drop((probe, first, worker, source, document, file, original));
    }
}

#[cfg(target_os = "macos")]
#[test]
/// 跨输入 GSUB 首拒绝后，只在真实 Font 变化处交付组合字和家庭 emoji，长 ASCII 尾仍由原局部计划认证。
fn cross_input_combining_and_family_emoji_close_real_font_runs_before_long_ascii_suffix() {
    fn captured(path: &str, maximum: u64) -> MarkdownLayoutFont {
        // 使用未改写的实机字体表；名称只定位真实 face，计划仍按活动查表认证。
        let bytes =
            crate::MarkdownLayoutFontBytes::capture_file(std::path::Path::new(path), maximum)
                .unwrap();
        let name = read_fonts::FontRef::from_index(bytes.as_ref(), 0)
            .unwrap()
            .table_data(read_fonts::types::Tag::new(b"name"))
            .unwrap()
            .as_bytes()
            .to_vec();
        MarkdownLayoutFont::resolve_name_table_at_size(bytes.blob(), &name, &[], 16.0).unwrap()
    }
    for prefix in ["e\u{301}👩‍👩‍👧‍👦!", "a?👩‍👩‍👧‍👦!"] {
        let mono = captured("/System/Library/Fonts/SFNSMono.ttf", 32 * 1024 * 1024);
        let emoji = captured(
            "/System/Library/Fonts/Apple Color Emoji.ttc",
            512 * 1024 * 1024,
        );
        let (file, document, source) = fixture(prefix.as_bytes());
        let backing = document.original_backing_observer().unwrap();
        let observed = source.resource_observer();
        let prefix_oracle = glyphs(&source, &mono, 0..2);
        let emoji_oracle = glyphs(&source, &emoji, 2..13);
        assert!(prefix_oracle.iter().all(|glyph| glyph.glyph_id != 0));
        assert!(emoji_oracle.iter().all(|glyph| glyph.glyph_id != 0));
        let mut worker = MarkdownLineItemCoordinator::new_with_frontier(
            source.clone(),
            &source.authority(),
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let resources = worker.resources.observer();
        let limit = MarkdownLineComposeBudget {
            bytes: 31,
            scalars: 7,
            records: 7,
            metadata_units: 7,
        };
        let mut emitted = Vec::new();
        for _ in 0..100_000 {
            let step = worker.step(limit).unwrap();
            assert!(step.bytes <= limit.bytes && step.scalars <= limit.scalars);
            assert!(step.records <= limit.records && step.metadata_units <= limit.metadata_units);
            match step.answer {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    let selected = if request.continuation_font().is_some() {
                        // 每次范围均由原 GCB 两端点签发；真实缺字才选择 emoji Font，不能为过门任意换字库。
                        let primary = glyphs(&source, &mono, range.clone());
                        if primary.iter().any(|glyph| glyph.glyph_id == 0) {
                            let fallback = glyphs(&source, &emoji, range);
                            assert!(fallback.iter().all(|glyph| glyph.glyph_id != 0));
                            &emoji
                        } else {
                            &mono
                        }
                    } else {
                        &mono // 初始范围只是样式请求，不对整条长项创建完整整形输入。
                    };
                    respond(&mut worker, request, selected);
                }
                MarkdownLineItemAnswer::ItemReady(facts) => {
                    let mut item = worker.take_item().unwrap();
                    let count = item.job.profile().unwrap().glyph_count;
                    let actual = (0..count)
                        .map(|ordinal| item.glyph(ordinal).unwrap().unwrap())
                        .collect::<Vec<_>>();
                    match emitted.len() {
                        0 => {
                            assert_eq!(facts.source_utf16, 0..2);
                            assert_eq!(actual, prefix_oracle);
                        }
                        1 => {
                            assert_eq!(facts.source_utf16, 2..13);
                            assert_eq!(actual, emoji_oracle);
                        }
                        2 => {
                            // 字体变化后的原 ASCII 计划必须交付有限前沿，不能把 64KiB 尾文整体 shape。
                            assert_eq!(facts.source_utf16.start, 13);
                            assert!(facts.source_utf16.end <= 13 + 256);
                            assert!(mono.permits(&item.font));
                        }
                        _ => unreachable!("有限回归只消费三个实际前沿项"),
                    }
                    emitted.push(facts.source_utf16);
                    drop(item); // 真实 child 归还后原协调器才可继续，不伪造 PendingConsumer 解除。
                    if emitted.len() == 3 {
                        break;
                    }
                }
                MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                    panic!(
                        "真实 Font 变化与可认证 ASCII 后缀不能退化为前沿拒绝: {reason:?}; 已交付原范围={emitted:?}"
                    )
                }
                MarkdownLineItemAnswer::EndOfSource => {
                    panic!("尚未消费的长尾不能被提前宣布完整")
                }
                _ => panic!("有限组合字与 emoji 回归不能转入其他布局协议"),
            }
        }
        assert_eq!(emitted.len(), 3);
        let actual = worker.frontier_work().unwrap();
        assert!(actual.input_bytes < 1024 && actual.shape_calls < 8);
        drop((worker, source, document, file, mono, emoji));
        assert!(resources.snapshot().unwrap().is_released());
        assert!(observed.payload_released());
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    }
}
