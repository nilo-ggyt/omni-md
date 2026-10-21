//! Real itemization over finite synthetic Sources. ItemReady here does not establish
//! the subsequent complete line's committed LineEnd.

use super::*;
mod engine;
mod folding;
mod font_probe;
#[cfg(feature = "paged-shaping-frontier")]
mod frontier;
#[cfg(feature = "paged-shaping-frontier")]
mod publish;
mod resources;
use crate::{MarkdownDocumentBuffer, MarkdownLayoutFont, MarkdownLayoutTextSource};
use std::sync::Arc;

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));

fn font() -> MarkdownLayoutFont {
    MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap()
}
fn style() -> MarkdownLineComposeStyle {
    MarkdownLineComposeStyle {
        presentation: Default::default(),
        points: 16.0,
        language: None,
        features: Vec::new(),
    }
}
struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    fn new(text: &str) -> Self {
        // Own an isolated actual UTF-8 file and its original captured Source. The
        // fixture's directory remains alive until every worker/output is retired.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("真实分项.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        Self {
            _directory: directory,
            document,
            source,
        }
    }
    fn coordinator(
        &self,
        cancellation: MarkdownLineComposeCancellation,
    ) -> MarkdownLineItemCoordinator {
        MarkdownLineItemCoordinator::new(
            self.source.clone(),
            &self.source.authority(),
            cancellation,
        )
        .unwrap()
    }
}
fn budget() -> MarkdownLineComposeBudget {
    MarkdownLineComposeBudget {
        bytes: 7,
        scalars: 3,
        records: 4,
        metadata_units: 2,
    }
}
fn request(coordinator: &mut MarkdownLineItemCoordinator) -> MarkdownLineStyleRequest {
    // Advance only original public bounded work. Losing progress or issuing another
    // terminal answer cannot be hidden as a fabricated Style request.
    for _ in 0..100_000 {
        let step = coordinator.step(budget()).unwrap();
        assert!(
            step.bytes <= 7 && step.scalars <= 3 && step.metadata_units <= 2 && step.shapes <= 1
        );
        match step.answer {
            MarkdownLineItemAnswer::NeedStyle(request) => return request,
            MarkdownLineItemAnswer::Yielded => {}
            _ => panic!("有限夹具的样式请求尚未签发"),
        }
    }
    panic!("有限依赖未实际完成");
}

#[test]
fn mixed_script_item_factory_uses_actual_source_levels_fonts_and_complete_glyphs() {
    // Actual Roboto covers these three scripts. A .notdef glyph, language label,
    // or invented line break cannot stand in for genuine mixed-script itemization.
    let fixture = Fixture::new("Office Αλφα Привет office");
    let before = (
        fixture.document.generation(),
        fixture.document.byte_len(),
        fixture.document.utf16_len(),
    );
    let mut coordinator = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let actual_font = font();
    let mut next = 0;
    let mut scripts = std::collections::BTreeSet::new();
    let mut items = 0;
    for _ in 0..100_000 {
        let step = coordinator.step(budget()).unwrap();
        assert!(
            step.bytes <= 7 && step.scalars <= 3 && step.metadata_units <= 2 && step.shapes <= 1
        );
        match step.answer {
            MarkdownLineItemAnswer::Yielded => {}
            MarkdownLineItemAnswer::NeedStyle(request) => {
                assert!(request.source() == &fixture.source.authority());
                assert_eq!(request.source_range().start, next);
                assert_eq!(request.paragraph_range(), 0..fixture.source.utf16_len());
                let range = request.source_range();
                coordinator
                    .provide_style(
                        request
                            .respond(range, actual_font.clone(), style())
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineItemAnswer::ItemReady(facts) => {
                // Read every actual glyph before dropping this finite item. Its
                // complete Source coverage, original UBA direction, and paragraph
                // flags must agree with the preceding public request sequence.
                let mut item = coordinator.take_item().unwrap();
                assert_eq!(item.facts(), &facts);
                assert_eq!(facts.source_utf16.start, next);
                assert!(facts.source_utf16.end > next);
                assert_eq!(facts.level, 0);
                assert_eq!(
                    facts.direction,
                    crate::MarkdownShapingDirection::LeftToRight
                );
                assert_eq!(facts.beginning_of_text, next == 0);
                assert_eq!(
                    facts.end_of_text,
                    facts.source_utf16.end == fixture.source.utf16_len()
                );
                let mut glyphs = 0;
                while let Some(glyph) = item.glyph(glyphs).unwrap() {
                    assert_ne!(glyph.glyph_id, 0, "实际字体必须覆盖本有限脚本夹具");
                    assert!(facts.source_utf16.contains(&glyph.source_utf16_cluster));
                    glyphs += 1;
                    assert!(glyphs < 1024);
                }
                assert!(glyphs > 0);
                scripts.insert(facts.script);
                next = facts.source_utf16.end;
                items += 1;
            }
            #[cfg(feature = "paged-shaping-frontier")]
            MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                panic!("旧整item对照不应启前沿: {reason:?}")
            }
            MarkdownLineItemAnswer::NeedsControl(control) => {
                panic!("无控制夹具不应产生{control:?}")
            }
            MarkdownLineItemAnswer::EndOfSource => {
                // Completion must cover the actual original Source and all three
                // scripts, without changing document bytes/generation/history.
                assert_eq!(next, fixture.source.utf16_len());
                assert!(items >= 3);
                assert!(
                    scripts.contains(b"Latn")
                        && scripts.contains(b"Grek")
                        && scripts.contains(b"Cyrl")
                );
                assert_eq!(
                    before,
                    (
                        fixture.document.generation(),
                        fixture.document.byte_len(),
                        fixture.document.utf16_len()
                    )
                );
                return;
            }
        }
    }
    panic!("有限mixed输入没有真实完成；不能以Pending冒充成功");
}

#[test]
fn equal_bytes_foreign_request_cannot_authorize_original_coordinator() {
    let fixture = Fixture::new("same data");
    for same_source in [false, true] {
        // Distinct requests fail even on the same Source; Source identity cannot
        // substitute for the one current nonce at the response boundary.
        let other_source = if same_source {
            fixture.source.clone()
        } else {
            MarkdownLayoutTextSource::capture(&fixture.document)
        };
        let mut original = fixture.coordinator(MarkdownLineComposeCancellation::new());
        let mut foreign = MarkdownLineItemCoordinator::new(
            other_source.clone(),
            &other_source.authority(),
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let _original_request = request(&mut original);
        let foreign_request = request(&mut foreign);
        let range = foreign_request.source_range();
        let response = foreign_request.respond(range, font(), style()).unwrap();
        assert!(matches!(
            original.provide_style(response),
            Err(MarkdownLineComposeError::SourceMismatch)
        ));
        assert!(matches!(
            original.step(budget()),
            Err(MarkdownLineComposeError::Poisoned)
        ));
    }
}

#[test]
fn duplicate_response_and_grapheme_internal_style_end_are_rejected() {
    // A real combining sequence establishes the non-splittable right endpoint.
    // Separately replay a consumed public response, without mutating private state.
    let fixture = Fixture::new("a\u{301}b");
    let mut split = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let pending = request(&mut split);
    assert_eq!(pending.source_range(), 0..3);
    let response = pending.respond(0..1, font(), style()).unwrap();
    assert!(matches!(
        split.provide_style(response),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));

    let mut duplicate = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let pending = request(&mut duplicate);
    let range = pending.source_range();
    let replay = pending
        .clone()
        .respond(range.clone(), font(), style())
        .unwrap();
    duplicate
        .provide_style(pending.respond(range, font(), style()).unwrap())
        .unwrap();
    assert!(matches!(
        duplicate.provide_style(replay),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    assert!(matches!(
        duplicate.step(budget()),
        Err(MarkdownLineComposeError::Poisoned)
    ));
}

#[test]
fn cancel_after_style_request_preserves_source_and_never_constructs_an_item() {
    // Cancel the actual shared request before accepting its response. The typed
    // cancellation retires the worker while the original document remains unchanged.
    let fixture = Fixture::new("queued style");
    let cancel = MarkdownLineComposeCancellation::new();
    let mut coordinator = fixture.coordinator(cancel.clone());
    let pending = request(&mut coordinator);
    let range = pending.source_range();
    cancel.cancel();
    assert!(matches!(
        coordinator.provide_style(pending.respond(range, font(), style()).unwrap()),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    assert_eq!(fixture.document.generation(), 0);
    assert_eq!(fixture.source.byte_len(), b"queued style".len());
    assert!(matches!(
        coordinator.step(budget()),
        Err(MarkdownLineComposeError::Poisoned)
    ));
}

#[test]
fn empty_source_has_no_fake_item_and_zero_budget_is_terminal() {
    // Original empty input reaches actual EOF without an invented nonempty item.
    // Invalid work budget separately poisons its real candidate rather than resume.
    let empty = Fixture::new("");
    let mut coordinator = empty.coordinator(MarkdownLineComposeCancellation::new());
    for _ in 0..1000 {
        match coordinator.step(budget()).unwrap().answer {
            MarkdownLineItemAnswer::EndOfSource => break,
            MarkdownLineItemAnswer::Yielded => {}
            _ => panic!("空源不能签发非空item"),
        }
    }
    assert!(matches!(
        coordinator.step(budget()).unwrap().answer,
        MarkdownLineItemAnswer::EndOfSource
    ));
    let mut failed = empty.coordinator(MarkdownLineComposeCancellation::new());
    let mut zero = budget();
    zero.bytes = 0;
    assert!(matches!(
        failed.step(zero),
        Err(MarkdownLineComposeError::InvalidRequest)
    ));
    assert!(matches!(
        failed.step(budget()),
        Err(MarkdownLineComposeError::Poisoned)
    ));
}

#[test]
fn registered_font_response_requires_original_catalog_and_live_palette_value() {
    let fixture = Fixture::new("real font scope");
    let catalog = MarkdownLineFontCatalog::new();
    let registered = catalog.register(&font()).unwrap();
    let mut coordinator = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let pending = request(&mut coordinator);
    let range = pending.source_range();
    coordinator
        .provide_style(
            pending
                .respond_registered(range, &registered, style())
                .unwrap(),
        )
        .unwrap();
    let mut completed = false;
    for _ in 0..1000 {
        if matches!(
            coordinator.step(budget()).unwrap().answer,
            MarkdownLineItemAnswer::ItemReady(_)
        ) {
            let mut item = coordinator.take_item().unwrap();
            let token = item.font_registration().unwrap();
            let actual = catalog.require_registration(token).unwrap();
            assert!(actual.font().permits(&registered.font().authority()));
            assert!(item.glyph(0).unwrap().is_some());
            completed = true;
            break;
        }
    }
    assert!(completed, "有限注册字体输入必须真实达到ItemReady");
    // A response retaining its selected font is not the registration's owner. After
    // the actual palette value drops, the original weak registration must fail.
    let mut expired = fixture.coordinator(MarkdownLineComposeCancellation::new());
    let pending = request(&mut expired);
    let response = pending
        .clone()
        .respond_registered(pending.source_range(), &registered, style())
        .unwrap();
    drop(registered);
    assert!(matches!(
        expired.provide_style(response),
        Err(MarkdownLineComposeError::FontExpired)
    ));
}
