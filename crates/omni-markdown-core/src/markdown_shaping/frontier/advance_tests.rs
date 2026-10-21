//! Actual font lower bounds and original-authority revocation; complete finite glyph traversal
//! belongs only to the independent test oracle.

use super::super::{
    MarkdownShapingCancellation, MarkdownShapingError, ValidatedOptions,
    advance::{AdvanceScanAnswer, AdvanceScanError, AdvanceScanOrigin, DefaultAdvanceScan},
};
use super::{Fixture, independent_font};
use crate::markdown_layout_font::frontier::FontFrontierPlan;
use crate::{MarkdownDocumentBufferError, MarkdownLayoutTextSource};
use harfrust::{GlyphId, ShaperFont};

fn origin(fixture: &Fixture) -> AdvanceScanOrigin<'_> {
    AdvanceScanOrigin {
        source: &fixture.source,
        source_authority: &fixture.authority,
        font: &fixture.font,
        font_authority: &fixture.font_authority,
    }
}

fn options_and_plan(fixture: &Fixture) -> (ValidatedOptions, FontFrontierPlan) {
    let options = ValidatedOptions::new(fixture.options(), &fixture.font).unwrap();
    let plan = FontFrontierPlan::new(
        &fixture.font,
        &fixture.font_authority,
        options.direction,
        options.script,
        options.language.as_ref(),
        &options.features,
    )
    .unwrap();
    (options, plan)
}

#[test]
fn all_glyphs_are_queried_in_fixed_steps_and_proof_binds_the_original_plan() {
    let fixture = Fixture::new("a word with spaces ", independent_font());
    let (options, plan) = options_and_plan(&fixture);
    let cancellation = MarkdownShapingCancellation::default();
    let mut scan =
        DefaultAdvanceScan::new(origin(&fixture), &options, &plan, cancellation.clone()).unwrap();
    let mut total = 0;
    loop {
        let step = scan.step(&fixture.source, &fixture.authority, 7).unwrap();
        assert!((1..=7).contains(&step.queried));
        total += step.queried;
        assert_eq!(step.total_queried as usize, total);
        if step.answer == AdvanceScanAnswer::Complete {
            break;
        }
        assert_eq!(step.answer, AdvanceScanAnswer::Pending);
    }
    assert_eq!(total, fixture.font.metadata().glyph_count as usize);
    let proof = scan.finish(&fixture.source, &fixture.authority).unwrap();
    // Query every actual glyph with an independent default-metric oracle, without sampling or
    // substituting the ASCII cmap list for a complete glyph-domain bound.
    let shaper = ShaperFont::new(fixture.font.font()).with_scale(options.scale);
    let minimum = (0..fixture.font.metadata().glyph_count)
        .map(|glyph| options.quantize(shaper.default_glyph_h_advance(GlyphId::new(glyph))))
        .min()
        .unwrap();
    assert!(minimum >= 0);
    assert_eq!(proof.minimum_26_6(), minimum);
    assert!(proof.matches(origin(&fixture), &options, &plan).unwrap());
    let (_, equivalent_new_plan) = options_and_plan(&fixture);
    assert!(
        !proof
            .matches(origin(&fixture), &options, &equivalent_new_plan)
            .unwrap()
    );
    let mut foreign_options = options.clone();
    foreign_options.scale += 1;
    assert!(
        !proof
            .matches(origin(&fixture), &foreign_options, &plan)
            .unwrap()
    );
    let mut foreign_options = options.clone();
    foreign_options.end_of_text = !foreign_options.end_of_text;
    assert!(
        !proof
            .matches(origin(&fixture), &foreign_options, &plan)
            .unwrap()
    );
    cancellation.request();
    assert!(matches!(
        proof.matches(origin(&fixture), &options, &plan),
        Err(AdvanceScanError::Shaping(MarkdownShapingError::Cancelled))
    )); // Live original plan/font owners cannot revive a cancelled proof.
}

#[test]
fn incomplete_foreign_and_zero_budget_scans_cannot_publish_or_resume() {
    let fixture = Fixture::new("actual source ", independent_font());
    let (options, plan) = options_and_plan(&fixture);
    let mut scan = DefaultAdvanceScan::new(
        origin(&fixture),
        &options,
        &plan,
        MarkdownShapingCancellation::default(),
    )
    .unwrap();
    let step = scan
        .step(&fixture.source, &fixture.authority, usize::MAX)
        .unwrap();
    assert_eq!(step.queried, 128); // Even an enormous requested budget retains the production per-step cap.
    assert_eq!(step.answer, AdvanceScanAnswer::Pending);
    assert!(matches!(
        scan.finish(&fixture.source, &fixture.authority),
        Err(AdvanceScanError::Incomplete)
    ));
    let mut scan = DefaultAdvanceScan::new(
        origin(&fixture),
        &options,
        &plan,
        MarkdownShapingCancellation::default(),
    )
    .unwrap();
    assert!(matches!(
        scan.step(&fixture.source, &fixture.authority, 0),
        Err(AdvanceScanError::InvalidRequest)
    ));
    assert!(matches!(
        scan.step(&fixture.source, &fixture.authority, 1),
        Err(AdvanceScanError::Incomplete)
    ));
    let other_source = MarkdownLayoutTextSource::capture(&fixture.document);
    let mut scan = DefaultAdvanceScan::new(
        origin(&fixture),
        &options,
        &plan,
        MarkdownShapingCancellation::default(),
    )
    .unwrap();
    assert!(matches!(
        scan.step(&other_source, &other_source.authority(), 1),
        Err(AdvanceScanError::Shaping(
            MarkdownShapingError::SourceMismatch
        ))
    ));
    assert!(matches!(
        scan.step(&fixture.source, &fixture.authority, 1),
        Err(AdvanceScanError::Incomplete)
    ));
    let foreign_font = independent_font();
    assert!(matches!(
        DefaultAdvanceScan::new(
            AdvanceScanOrigin {
                font: &foreign_font,
                font_authority: &foreign_font.authority(),
                ..origin(&fixture)
            },
            &options,
            &plan,
            MarkdownShapingCancellation::default(),
        ),
        Err(AdvanceScanError::PlanMismatch)
    ));
}

#[test]
fn original_source_io_failure_precedes_cancel_even_with_a_healthy_foreign_source() {
    // Prime a real source lease, fail its explicit reclaim, then supply healthy foreign authority.
    // The recorded original EIO must not become SourceMismatch or clean cancellation.
    let fixture = Fixture::new("source health ", independent_font());
    let (options, plan) = options_and_plan(&fixture);
    let backing = fixture.document.original_backing_observer().unwrap();
    let source_observer = fixture.source.resource_observer();
    let cancellation = MarkdownShapingCancellation::default();
    let mut scan =
        DefaultAdvanceScan::new(origin(&fixture), &options, &plan, cancellation.clone()).unwrap();
    let other = Fixture::new("unrelated healthy source ", independent_font());
    fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(
        fixture
            .document
            .reclaim_original_read_windows_for_test()
            .is_err()
    );
    cancellation.request();
    assert!(matches!(
        scan.step(&other.source, &other.authority, 1),
        Err(AdvanceScanError::Shaping(MarkdownShapingError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        )))
    ));
    drop(scan);
    drop(fixture);
    assert!(source_observer.payload_released());
    assert!(backing.unmap_succeeded());
    assert_eq!(backing.mapped_bytes(), 0);
}
