//! 真实AAT计划拒绝与Font边界；原长尾保留mmap，不把开放端点或相同Font试次当成闭段。

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

/// 固定64KiB长尾逐块落盘；只用首两个ASCII字素区分真实Font覆盖。
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

/// 原SFNT的真实name表定位同一face；不篡改glyph、cmap、查表或计划的拒绝结果。
fn font(bytes: &'static [u8]) -> MarkdownLayoutFont {
    let name = read_fonts::FontRef::from_index(bytes, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    MarkdownLayoutFont::resolve_name_table_at_size(Arc::new(bytes), &name, &[], 16.0).unwrap()
}

/// 固定Latn样式与原段落方向；完整GCB短oracle和候选使用同样的BOT/EOT规则。
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

/// 只对首一个字素运行旧完整Job取得独立实际字形，绝不对长尾建立完整整形输入。
fn glyphs(
    source: &MarkdownLayoutTextSource,
    font: &MarkdownLayoutFont,
    range: Range<usize>,
) -> Vec<MarkdownShapedGlyph> {
    assert_eq!(range.len(), 1);
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

/// 只推进原协调器，直到真实NeedStyle；每一步仍受原bytes/scalar/metadata量子约束。
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

/// 单次真实Font响应不改变原覆盖、段落、取消或注册职责。
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
/// 首个AAT Font只覆盖真实A字素；x实际glyph0促成另一Font选择，长尾必须回原itemization。
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
/// 同一Font连续成功不能将256配额当成边界；没有真实变化就保留原能力拒绝。
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
            MarkdownLineItemAnswer::NeedsFrontier(MarkdownLineFrontierReason::UnsupportedPlan) => {
                rejected = true;
                break;
            }
            _ => panic!("没有真实Font边界时不得交付任何预算伪段"),
        }
    }
    assert!(rejected && worker.payload.as_ref().unwrap().cursor == 0);
    drop((worker, source, document, file, original));
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
