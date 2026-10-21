//! Compare real naturally closed items with original complete Jobs. Missing glyphs
//! remain evidence of the selected font's coverage, not native feature acceptance.

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
        // Own a real isolated file, document, and captured Source. No user vault is
        // involved; the fixture directory outlives all readers until fixture drop.
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
        }; // Both paths advance their real dependencies; no stages are injected.
        constructor(self.source.clone(), &self.source.authority(), cancel).unwrap()
    }
}

fn font(bytes: &'static [u8]) -> MarkdownLayoutFont {
    // Resolve the actual SFNT face through its name table, retaining real cmap and
    // shaping tables so missing glyphs and script behavior come from the shaper.
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
    // Advance only through the coordinator's public bounded steps. A finite guard
    // catches a stalled fixture without inventing an item or EOF before NeedStyle.
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
    let mut result = Vec::new(); // Only this small oracle retains prior item results.
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
                // Reply with the exact requested range and the same live font
                // registration; both candidate and oracle use this actual payload.
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
                // Read real glyph records, including flags and glyph zero, before
                // releasing this one finite item's file/mapping-backed Job.
                let mut glyphs = Vec::new();
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
    // The worker's resource observer checks actual shape files and mappings after
    // all consumer-owned items and the coordinator have been released.
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
    // Combining sequences, IVS, ligatures, mixed scripts, and Arabic exercise the
    // original complete shaping oracle rather than guessed glyph-count snapshots.
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
        } // Direction/level come from real UBA, not reversing glyphs. Missing Roboto
        // emoji/IVS coverage is deliberately not claimed as native feature parity.
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
        // Feed the original request's script/direction/paragraph facts into the real
        // frontier Job; a short string alone cannot certify its rejected font plan.
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
        // Preserve the actual typed rejection while explicitly declining any claim
        // that this Core/font fixture validates platform emoji rendering.
        assert!(rejected.is_some());
        eprintln!(
            "closed_item_original_frontier text={text:?} reason={rejected:?} native_emoji_parity=false"
        );
    }
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
        // Actual preparation opens the Job observed below; private stage names are
        // not the cancellation contract. Its real files/mappings must still close.
        worker.step(one()).unwrap();
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
        } // The existing injection makes real backing reclaim report EIO; this is
        // not a naturally induced kernel failure. Cancellation must retain it.
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
