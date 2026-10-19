//! 原Job已归还后，真实Store目录只经原Measure许可触发有限unsafe前缀，不重造旧Job身份。
use super::*;
use crate::markdown_soft_line_resolve::stored::MarkdownStoredPrefixResolveJob;
use crate::{
    MarkdownEmergencyGlyphSliceAnswer, MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureJob,
    MarkdownSoftLineMeasureRequest, MarkdownSoftLineResolveBudget,
};

#[test]
fn stored_prefix_after_actual_job_drop_matches_direct_font_fields_and_requires_real_child_return() {
    let text = "ffi".repeat(16);
    let fixture = Fixture::new(&text);
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let cancellation = MarkdownLineComposeCancellation::new();
    let mut store = fixture.store(cancellation.clone());
    let proof = capture(&mut store, &mut item);
    let job_observer = item.job.resource_observer();
    drop(item);
    drop(coordinator);
    assert_eq!(job_observer.snapshot().files_open, 0);
    let mut breaks = super::lines::breaks(&fixture);
    let font = fixture.font.font().authority();
    let mut measure = {
        let mut provider = store.borrow(&proof).unwrap();
        MarkdownSoftLineMeasureJob::new_with_provider(
            fixture.source.clone(),
            &fixture.source.authority(),
            &font,
            &mut provider,
            &mut breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: 0,
                width_26_6: 8 * 64,
                cancellation: cancellation.measure(),
            },
        )
        .unwrap()
    };
    let choice = super::providers::selected(&mut measure, &mut store, &proof, &mut breaks);
    assert!(matches!(
        choice,
        MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(_)
            | MarkdownSoftLineMeasureChoice::NeedsReshape { .. }
    ));
    let permit = {
        let mut provider = store.borrow(&proof).unwrap();
        measure.resolution_permit(&mut provider).unwrap()
    };
    let mut resolver = {
        let mut provider = store.borrow(&proof).unwrap();
        MarkdownStoredPrefixResolveJob::new(&permit, &mut provider).unwrap()
    };
    let resources = resolver.resource_observer();
    let mut total = 0;
    for _ in 0..10000 {
        let step = {
            let mut provider = store.borrow(&proof).unwrap();
            resolver
                .step(
                    &permit,
                    &mut provider,
                    &mut breaks,
                    MarkdownSoftLineResolveBudget::default(),
                )
                .unwrap()
        };
        match step.answer {
            MarkdownEmergencyGlyphSliceAnswer::Yielded
            | MarkdownEmergencyGlyphSliceAnswer::PendingDependencies => {}
            MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(boundary) => {
                let mut child = {
                    let mut provider = store.borrow(&proof).unwrap();
                    resolver.take_signed_prefix(&permit, &mut provider).unwrap()
                };
                let actual = {
                    let mut provider = store.borrow(&proof).unwrap();
                    child.facts(&permit, &mut provider).unwrap().boundary
                };
                assert_eq!(actual, boundary);
                let range = actual.source_utf16.clone();
                assert_eq!(range.start, total);
                assert!(range.end > range.start && range.len() < text.len());
                // 有限直接字体API仅作字段对照，不称为独立上游算法或整产品内存验收。
                let mut oracle = harfrust::Buffer::new();
                oracle.set_direction(harfrust::Direction::LeftToRight);
                oracle.set_script(Some(harfrust::Script::LATIN));
                oracle.set_language("en-GB".parse().ok());
                oracle.set_cluster_level(harfrust::ClusterLevel::MonotoneGraphemes);
                let mut flags = harfrust::BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
                if range.start == 0 {
                    flags |= harfrust::BufferFlags::BEGINNING_OF_TEXT;
                }
                if range.end == text.len() {
                    flags |= harfrust::BufferFlags::END_OF_TEXT;
                }
                oracle.set_flags(flags);
                oracle.set_pre_context(&text[..range.start]);
                oracle.set_post_context(&text[range.end..]);
                for (at, scalar) in text[range.clone()].chars().enumerate() {
                    oracle.push(u32::from(scalar), u32::try_from(range.start + at).unwrap());
                }
                let features = [harfrust::Feature::new(harfrust::Tag::new(b"liga"), 1, ..)];
                harfrust::shape(
                    &harfrust::ShaperFont::new(fixture.font.font().font()).with_scale(1024),
                    &mut oracle,
                    harfrust::ShapeOptions::new()
                        .point_size(Some(16.0))
                        .features(&features),
                )
                .unwrap();
                assert_eq!(oracle.len(), actual.glyphs);
                for at in 0..oracle.len() {
                    let expected = oracle.paged_glyph_record(at).unwrap();
                    let glyph = {
                        let mut provider = store.borrow(&proof).unwrap();
                        child.glyph(&permit, &mut provider, at).unwrap().unwrap()
                    };
                    assert_eq!(
                        (
                            glyph.glyph_id,
                            glyph.source_utf16_cluster,
                            glyph.flags,
                            glyph.x_advance,
                            glyph.y_advance,
                            glyph.x_offset,
                            glyph.y_offset
                        ),
                        (
                            expected.0.glyph_id,
                            expected.0.cluster as usize,
                            expected.0.flags().to_bits() & harfrust::GlyphFlags::ALL.to_bits(),
                            expected.1.x_advance,
                            expected.1.y_advance,
                            expected.1.x_offset,
                            expected.1.y_offset
                        )
                    );
                }
                assert!({
                    let mut provider = store.borrow(&proof).unwrap();
                    child
                        .glyph(&permit, &mut provider, oracle.len())
                        .unwrap()
                        .is_none()
                });
                let pending = {
                    let mut provider = store.borrow(&proof).unwrap();
                    resolver
                        .step(
                            &permit,
                            &mut provider,
                            &mut breaks,
                            MarkdownSoftLineResolveBudget::default(),
                        )
                        .unwrap()
                };
                assert_eq!(
                    pending.answer,
                    MarkdownEmergencyGlyphSliceAnswer::PendingPrefixConsumer
                );
                assert_eq!(pending.input_bytes, 0);
                child.close().unwrap();
                assert_eq!(resources.snapshot().active_jobs, 0);
                total = range.end;
                if total >= 6 {
                    break;
                }
            }
            other => panic!("真实Stored unsafe前缀不可被其他阶段冒充:{other:?}"),
        }
    }
    assert!(total >= 6);
    let closed = resolver.close().unwrap();
    assert_eq!(closed.active_jobs, 0);
    assert!(!closed.cleanup_failed);
    store.close().unwrap();
}
