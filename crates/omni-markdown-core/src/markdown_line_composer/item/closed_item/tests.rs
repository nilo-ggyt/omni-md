//! 真实自然闭项与原完整Job差分；缺字保留为原字体能力事实，不冒充Native功能验收。

use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownShapedGlyph};

#[path = "tests/font_runs.rs"]
mod font_runs;

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));
const ARABIC: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/tests/fonts/in-house/SimpArabicTest.ttf"
));

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("自然闭合小项.md");
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
        frontier: bool,
        cancel: MarkdownLineComposeCancellation,
    ) -> MarkdownLineItemCoordinator {
        let constructor = if frontier {
            MarkdownLineItemCoordinator::new_with_frontier
        } else {
            MarkdownLineItemCoordinator::new
        }; // oracle保留原完整依赖/整形路径；候选必须独立真实推进，不注入阶段。
        constructor(self.source.clone(), &self.source.authority(), cancel).unwrap()
    }
}

fn font(bytes: &'static [u8]) -> MarkdownLayoutFont {
    let name = read_fonts::FontRef::from_index(bytes, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    MarkdownLayoutFont::resolve_name_table_at_size(Arc::new(bytes), &name, &[], 16.0).unwrap()
}
fn style() -> MarkdownLineComposeStyle {
    MarkdownLineComposeStyle {
        presentation: Default::default(),
        points: 16.0,
        language: None,
        features: Vec::new(),
    }
}
fn one() -> MarkdownLineComposeBudget {
    MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    }
}
fn request(
    worker: &mut MarkdownLineItemCoordinator,
    budget: MarkdownLineComposeBudget,
) -> MarkdownLineStyleRequest {
    for _ in 0..1_000_000 {
        let step = worker.step(budget).unwrap();
        assert!(
            step.bytes <= budget.bytes
                && step.scalars <= budget.scalars
                && step.records <= budget.records
                && step.metadata_units <= budget.metadata_units
        );
        match step.answer {
            MarkdownLineItemAnswer::NeedStyle(request) => return request,
            MarkdownLineItemAnswer::Yielded => {}
            _ => panic!("真实样式请求之前不能制造item或EOF"),
        }
    }
    panic!("有限原依赖未交付真实请求");
}

fn collect(
    fixture: &Fixture,
    actual: &MarkdownLayoutFont,
    frontier: bool,
) -> Vec<(MarkdownLineItemFacts, Vec<MarkdownShapedGlyph>)> {
    let catalog = super::super::super::MarkdownLineFontCatalog::new();
    let registered = catalog.register(actual).unwrap();
    let mut worker = fixture.worker(frontier, MarkdownLineComposeCancellation::new());
    let resources = worker.resources.observer();
    let mut result = Vec::new(); // oracle最多数个合成短项；产品没有这份字形/正文历史。
    let mut end = 0;
    let mut complete = false;
    for _ in 0..1_000_000 {
        let step = worker.step(one()).unwrap();
        assert!(
            step.bytes <= 1
                && step.scalars <= 1
                && step.records <= 1
                && step.metadata_units <= 1
                && step.shapes <= 1
        );
        match step.answer {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(range, &registered, style())
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineItemAnswer::ItemReady(facts) => {
                let mut item = worker.take_item().unwrap();
                assert_eq!(facts, *item.facts());
                assert_eq!(facts.source_utf16.start, end);
                assert_eq!(facts.paragraph_utf16, 0..fixture.source.utf16_len());
                assert_eq!(facts.beginning_of_text, facts.source_utf16.start == 0);
                assert_eq!(
                    facts.end_of_text,
                    facts.source_utf16.end == fixture.source.utf16_len()
                );
                assert!(item.source.permits(&fixture.source.authority()));
                assert!(registered.font().permits(&item.font));
                let count = item.job.profile().unwrap().glyph_count;
                let mut glyphs = Vec::new(); // 仅有限测试oracle逐条取得原真实结果，包含flags及缺字。
                for index in 0..count {
                    glyphs.push(item.glyph(index).unwrap().unwrap());
                }
                assert!(item.glyph(count).unwrap().is_none());
                end = facts.source_utf16.end;
                let receipt = item.job.resource_observer();
                drop(item);
                let released = receipt.snapshot();
                assert_eq!(
                    (
                        released.files_open,
                        released.mappings_open,
                        released.mapped_bytes
                    ),
                    (0, 0, 0)
                );
                assert!(!released.cleanup_failed && released.first_error.is_none());
                result.push((facts, glyphs));
            }
            MarkdownLineItemAnswer::EndOfSource => {
                complete = true;
                break;
            }
            MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                panic!("自然有限闭项仍未完成：{reason:?}")
            }
            MarkdownLineItemAnswer::NeedsControl(_) => panic!("本组有限夹具没有产品内联控制"),
        }
    }
    assert!(complete && end == fixture.source.utf16_len());
    drop(worker);
    let released = resources.snapshot().unwrap();
    assert_eq!(
        (released.shapes.active, released.shapes.registered),
        (0, released.shapes.settled)
    );
    assert_eq!(released.shapes.files_created, released.shapes.files_closed);
    assert_eq!(
        released.shapes.mappings_created,
        released.shapes.mappings_released
    );
    result
}

#[test]
fn natural_closed_emoji_ivs_marks_context_and_rtl_match_original_complete_jobs_field_for_field() {
    for (text, bytes, expect_missing) in [
        ("👩🏽‍💻", ROBOTO, true),
        ("葛\u{e0100}", ROBOTO, true),
        ("a\u{0301}", ROBOTO, false),
        ("office Αλφα fi", ROBOTO, false),
        ("中中中中中中中 abcdef Ω甲甲甲甲甲甲甲甲甲", ROBOTO, true),
        ("سلام", ARABIC, false),
    ] {
        let fixture = Fixture::new(text);
        let actual = font(bytes);
        let oracle = collect(&fixture, &actual, false);
        let candidate = collect(&fixture, &actual, true);
        assert_eq!(
            candidate, oracle,
            "自然短项所有glyph/cluster/flags/advance/offset均须等于原Job：{text}"
        );
        let missing = candidate
            .iter()
            .flat_map(|(_, glyphs)| glyphs)
            .filter(|glyph| glyph.glyph_id == 0)
            .count();
        assert_eq!(missing != 0, expect_missing);
        if text == "سلام" {
            assert!(candidate.iter().all(|(facts, _)| facts.direction
                == MarkdownShapingDirection::RightToLeft
                && facts.level & 1 != 0));
        } // 真RTL方向/level沿原UBA而来，不以glyph反转代替；emoji/IVS的Roboto缺字不算功能通过。
        let source_receipt = fixture.source.resource_observer();
        let mapping = fixture.source.original_backing_observer().unwrap();
        drop((fixture, actual));
        assert!(source_receipt.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn original_frontier_rejection_is_classified_without_claiming_missing_emoji_support() {
    use crate::markdown_shaping::frontier::{
        MarkdownShapingFrontierAnswer as Answer, MarkdownShapingFrontierJob,
    };
    for text in ["👩🏽‍💻", "葛\u{e0100}", "a\u{0301}", "office fi"] {
        let fixture = Fixture::new(text);
        let actual = font(ROBOTO);
        let authority = fixture.source.authority();
        let mut worker = fixture.worker(false, MarkdownLineComposeCancellation::new());
        let original = request(&mut worker, MarkdownLineComposeBudget::default());
        let range = original.source_range();
        let mut frontier = MarkdownShapingFrontierJob::new(
            fixture.source.clone(),
            &authority,
            actual.clone(),
            &actual.authority(),
            range.clone(),
            MarkdownShapingOptions {
                direction: if original.level() & 1 == 0 {
                    MarkdownShapingDirection::LeftToRight
                } else {
                    MarkdownShapingDirection::RightToLeft
                },
                script: original.script(),
                language: None,
                points: 16.0,
                beginning_of_text: range.start == original.paragraph_range().start,
                end_of_text: range.end == original.paragraph_range().end,
                features: Vec::new(),
            },
            MarkdownShapingCancellation::default(),
        )
        .unwrap();
        let mut rejected = None;
        for _ in 0..100_000 {
            let step = frontier
                .step(
                    &authority,
                    &actual.authority(),
                    &mut worker.payload.as_mut().unwrap().breaks,
                    MarkdownShapingBudget {
                        bytes: 1,
                        scalars: 1,
                    },
                    1,
                )
                .unwrap();
            match step.answer {
                Answer::Pending => {}
                Answer::NeedsFrontier(reason) => {
                    rejected = Some(reason);
                    break;
                }
                _ => panic!("未认证Roboto计划不能凭短文本或缺字授予前沿许可"),
            }
        }
        assert!(rejected.is_some());
        eprintln!(
            "closed_item_original_frontier text={text:?} reason={rejected:?} native_emoji_parity=false"
        );
    }
}

#[test]
fn open_script_long_natural_item_and_shortened_response_remain_on_original_frontier() {
    let actual = font(ROBOTO);
    for text in ["中文".repeat(4096), "a".repeat(MAX_CLOSED_UTF16 + 1)] {
        let fixture = Fixture::new(&text);
        let mut worker = fixture.worker(true, MarkdownLineComposeCancellation::new());
        let original = request(&mut worker, MarkdownLineComposeBudget::default());
        let range = original.source_range();
        let open = worker.payload.as_ref().unwrap().open_script;
        assert!(open || range.len() > MAX_CLOSED_UTF16);
        worker
            .provide_style(original.respond(range, actual.clone(), style()).unwrap())
            .unwrap();
        let payload = worker.payload.as_ref().unwrap();
        assert!(matches!(payload.stage, Stage::Frontier) && payload.item.is_none());
    } // 超长/开放项不申请完整Job，后继真实拒绝或前沿续体保持原语义。
    let fixture = Fixture::new("office");
    let mut worker = fixture.worker(true, MarkdownLineComposeCancellation::new());
    let original = request(&mut worker, MarkdownLineComposeBudget::default());
    assert!(!worker.payload.as_ref().unwrap().open_script && original.source_range() == (0..6));
    worker
        .provide_style(original.respond(0..1, actual, style()).unwrap())
        .unwrap();
    let payload = worker.payload.as_ref().unwrap();
    assert!(matches!(payload.stage, Stage::Frontier) && payload.item.is_none());
}

#[test]
fn closed_item_uses_original_cancel_bit_and_source_errno_still_precedes_cancel() {
    for poison in [false, true] {
        let fixture = Fixture::new("a\u{0301}");
        let actual = font(ROBOTO);
        let cancel = MarkdownLineComposeCancellation::new();
        let mut worker = fixture.worker(true, cancel.clone());
        let original = request(&mut worker, MarkdownLineComposeBudget::default());
        let range = original.source_range();
        worker
            .provide_style(original.respond(range, actual, style()).unwrap())
            .unwrap();
        assert!(matches!(
            worker.payload.as_ref().unwrap().stage,
            Stage::ClosedInput
        ));
        worker.step(one()).unwrap();
        assert!(matches!(
            worker.payload.as_ref().unwrap().stage,
            Stage::Input
        ));
        let receipt = worker
            .payload
            .as_ref()
            .unwrap()
            .item
            .as_ref()
            .unwrap()
            .job
            .resource_observer();
        if poison {
            fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
            fixture.document.fail_next_original_reclaim_for_test();
            assert!(
                fixture
                    .document
                    .reclaim_original_read_windows_for_test()
                    .is_err()
            );
        } // 真实mmap回收失败后再取消，不能用正常取消覆盖首个Source errno。
        cancel.cancel();
        let error = worker
            .step(one())
            .err()
            .expect("取消必须撤原闭项而非发布字形");
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
        assert!(worker.payload.is_none());
        let released = receipt.snapshot();
        assert_eq!(
            (
                released.files_open,
                released.mappings_open,
                released.mapped_bytes
            ),
            (0, 0, 0)
        );
        assert!(!released.cleanup_failed);
    }
}
