//! Commit real LineEnds through measurement and safe slices, without fabricated ranges or directories.
use super::*;
use crate::{
    BidiBaseDirection, BidiMemoryBudget, MarkdownBidiParagraph, MarkdownLayoutBreakJob,
    MarkdownLineBreakBudget, MarkdownSafeGlyphSlices, MarkdownSoftLineMeasureAnswer,
    MarkdownSoftLineMeasureBudget, MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureJob,
    MarkdownSoftLineMeasureRequest,
};

pub(super) fn breaks(fixture: &Fixture) -> MarkdownLayoutBreakJob {
    // Prepare the real original break job to its explicit Ready state. A yielded
    // quantum cannot authorize grapheme export or substitute for an actual EOF.
    let mut breaks =
        MarkdownLayoutBreakJob::new(fixture.source.clone(), &fixture.source.authority()).unwrap();
    for _ in 0..10000 {
        if breaks
            .step(
                &fixture.source.authority(),
                MarkdownLineBreakBudget::default(),
            )
            .unwrap()
            .ready
        {
            return breaks;
        }
    }
    panic!("有限原断点表未实际Ready");
}

#[test]
fn original_safe_lines_commit_real_line_end_and_preserve_glyphs_gcb_clusters_after_job_drop() {
    // Keep independent original job, cluster and break results while committing
    // several naturally wrapped lines into the real store.
    let fixture = Fixture::new("office office office office office office office office");
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let cancellation = MarkdownLineComposeCancellation::new();
    let mut store = fixture.store(cancellation.clone());
    let mut breaks = breaks(&fixture);
    while !store.capture_graphemes(&mut breaks, 3).unwrap() {}
    let proof = capture(&mut store, &mut item);
    let mut index =
        MarkdownGlyphClusterIndex::new(&mut item.job, &item.authority, &item.font).unwrap();
    while index.step(&mut item.job, 16).unwrap().status != MarkdownGlyphClusterStatus::Ready {}
    // Resolve paragraph direction from the same captured source; selected line
    // geometry must not borrow a bidi result from an equal-text capture.
    let mut bidi = MarkdownBidiParagraph::analyze_layout_source(
        fixture.source.clone(),
        &fixture.source.authority(),
        0..fixture.source.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
    )
    .unwrap();
    let mut start = 0;
    let mut lines = 0;
    // Each next measurement starts at the prior committed endpoint. Its chosen
    // break and safe glyph slice remain tied to the original job and cancellation.
    while start < fixture.source.utf16_len() {
        let mut measure = MarkdownSoftLineMeasureJob::new(
            fixture.source.clone(),
            &item.authority,
            &item.font,
            &mut item.job,
            &mut breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: start,
                width_26_6: 80 * 64,
                cancellation: cancellation.measure(),
            },
        )
        .unwrap();
        let mut chosen = None;
        for _ in 0..10000 {
            if let MarkdownSoftLineMeasureAnswer::Selected(choice) = measure
                .step(
                    &mut item.job,
                    &mut index,
                    &mut breaks,
                    MarkdownSoftLineMeasureBudget::default(),
                )
                .unwrap()
                .answer
            {
                chosen = Some(choice);
                break;
            }
        }
        // This finite fixture has natural opportunities. Emergency reshaping is
        // not accepted as an interchangeable way to obtain the expected line.
        let selected = match chosen.unwrap() {
            MarkdownSoftLineMeasureChoice::MeasuredBreak(value) => value,
            other => panic!("有限自然软行必须真实MeasuredBreak: {other:?}"),
        };
        let (safe, _tail) =
            MarkdownSafeGlyphSlices::from_measure(&measure, &mut item.job, &mut breaks, &mut index)
                .unwrap()
                .into_parts();
        // Preserve the actual break kind in the committed line rather than
        // inferring it later from a coordinate or the remaining source length.
        let kind = if selected.boundary.kind == crate::MarkdownLineBreakKind::Mandatory {
            MarkdownComposedLineKind::Mandatory
        } else {
            MarkdownComposedLineKind::Allowed
        };
        let facts = store
            .append_safe_line(&proof, safe, &mut item.job, &mut bidi, kind)
            .unwrap();
        assert_eq!(facts.source_utf16, start..selected.source_utf16.end);
        assert_eq!(facts.x_advance_26_6, selected.x_advance_26_6);
        assert_eq!(facts.ordinal, lines);
        start = facts.source_utf16.end;
        lines += 1;
    }
    assert!(lines >= 3);
    // These bounded oracle vectors belong only to this short fixture. Drop all
    // original producers before exercising output reads from the unique store.
    let mut expected = Vec::new();
    while let Some(glyph) = item.glyph(expected.len()).unwrap() {
        expected.push(glyph);
    }
    let mut gcb = Vec::new();
    for offset in 0..=fixture.source.utf16_len() {
        gcb.push(
            breaks
                .grapheme_query(
                    &fixture.source.authority(),
                    offset,
                    crate::MarkdownGraphemeQuery::IsBoundary,
                )
                .unwrap(),
        );
    }
    drop(item);
    drop(index);
    drop(breaks);
    drop(bidi);
    drop(coordinator);
    let mut output = store.into_output().unwrap();
    let observer = output.resource_observer();
    assert_eq!(output.source_range(), 0..fixture.source.utf16_len());
    assert!(output.source_authority() == &fixture.source.authority());
    output.require(&fixture.source.authority()).unwrap();
    // Walk published lines backward to prove complete, nonoverlapping coverage.
    // Each part retains its actual source and weak font registration authority.
    let mut expected_end = fixture.source.utf16_len();
    let mut actual = 0;
    let mut line = output.last_line().unwrap();
    while let Some(current) = line {
        assert_eq!(current.facts().source_utf16.end, expected_end);
        assert_eq!(current.facts().parts, 1);
        let part = output.part(&current, 0).unwrap().unwrap();
        assert!(output.part(&current, 1).unwrap().is_none());
        assert!(part.source_authority() == &fixture.source.authority());
        assert!(
            part.font_registration()
                .lease()
                .unwrap()
                .font()
                .permits(&fixture.font.font().authority())
        );
        // Read one real part into a fixed-size buffer and compare the corresponding
        // original visual glyph range, then query clusters at every covered offset.
        let blank = MarkdownShapedGlyph {
            glyph_id: 0,
            source_utf16_cluster: 0,
            flags: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        };
        let mut batch = [blank; 256];
        let facts = part.facts();
        assert_eq!((facts.origin_x_26_6, facts.origin_y_26_6), (0, 0));
        output
            .read_batch(&part, 0..facts.glyphs, &mut batch[..facts.glyphs])
            .unwrap();
        assert_eq!(
            &batch[..facts.glyphs],
            &expected[facts.visual_glyphs.clone()]
        );
        for offset in facts.source_utf16.clone() {
            let cluster = match output.cluster(&part, offset).unwrap() {
                MarkdownGlyphClusterAnswer::Known(Some(value)) => value,
                _ => panic!("真实行part不能失去cluster"),
            };
            assert!(cluster.glyphs.end <= facts.glyphs);
            assert!(cluster.source_utf16.contains(&offset));
        }
        assert!(matches!(
            output.cluster(&part, facts.source_utf16.end).unwrap(),
            MarkdownGlyphClusterAnswer::Known(None)
        ));
        expected_end = current.facts().source_utf16.start;
        actual += 1;
        line = output.previous_line(&current).unwrap();
    }
    assert_eq!(expected_end, 0);
    assert_eq!(actual, lines);
    // A grapheme ordinal advances only at an original boundary; UTF-16 scalar
    // coordinates inside a cluster cannot be relabeled as consecutive ordinals.
    let mut ordinal = 0;
    for (offset, expected) in gcb.into_iter().enumerate() {
        assert_eq!(
            output
                .grapheme_query(offset, crate::MarkdownGraphemeQuery::IsBoundary)
                .unwrap(),
            expected
        );
        let actual = output.grapheme_ordinal(offset).unwrap();
        if matches!(expected, crate::MarkdownGraphemeAnswer::Known(Some(_))) {
            assert_eq!(actual, Some(ordinal));
            ordinal += 1;
        } else {
            assert_eq!(actual, None);
        }
    }
    // Explicit close provides fallible file/mapping cleanup evidence after all
    // original job/index/bidi owners have already been released.
    output.close().unwrap();
    let state = observer.snapshot();
    assert!(
        !state.file_open
            && state.mapped_bytes == 0
            && !state.cleanup_failed
            && !state.reader_failed
    );
}

#[test]
fn ordinal_export_is_unknown_before_ready_and_never_accepts_large_or_foreign_batches() {
    // Export before Ready must leave caller memory untouched and remain unknown,
    // rather than converting an unfinished boundary table into an empty table.
    let fixture = Fixture::new("a\u{301}😀 z");
    let auth = fixture.source.authority();
    let mut pending = MarkdownLayoutBreakJob::new(fixture.source.clone(), &auth).unwrap();
    assert_eq!(pending.ready_grapheme_count(&auth).unwrap(), None);
    let sentinel = crate::MarkdownGraphemeBoundary {
        byte: usize::MAX,
        utf16: usize::MAX,
    };
    let mut untouched = [sentinel; 1];
    assert!(
        !pending
            .read_ready_graphemes(&auth, 0..1, &mut untouched)
            .unwrap()
    );
    assert_eq!(untouched, [sentinel]);
    // Once the genuine break job is ready, its exported endpoints and individual
    // queries must agree on the same original byte and UTF-16 coordinates.
    let mut ready = breaks(&fixture);
    let count = ready.ready_grapheme_count(&auth).unwrap().unwrap();
    let mut all = [sentinel; 16];
    assert!(
        ready
            .read_ready_graphemes(&auth, 0..count, &mut all[..count])
            .unwrap()
    );
    assert_eq!(
        all[0],
        crate::MarkdownGraphemeBoundary { byte: 0, utf16: 0 }
    );
    assert_eq!(
        all[count - 1],
        crate::MarkdownGraphemeBoundary {
            byte: fixture.source.byte_len(),
            utf16: fixture.source.utf16_len()
        }
    );
    for value in &all[..count] {
        assert!(
            matches!(ready.grapheme_query(&auth,value.utf16,crate::MarkdownGraphemeQuery::IsBoundary).unwrap(),crate::MarkdownGraphemeAnswer::Known(Some(actual)) if actual==*value)
        );
    }
    // An excessive batch fails before mutating caller output and retires that job.
    // A separate healthy job then proves that equal text has no source authority.
    let mut huge = [sentinel; 257];
    assert!(
        ready
            .read_ready_graphemes(&auth, 0..257, &mut huge)
            .is_err()
    );
    assert!(huge.iter().all(|value| *value == sentinel));
    assert!(matches!(
        ready.ready_grapheme_count(&auth),
        Err(crate::MarkdownLayoutBreakError::Poisoned)
    ));
    let mut other = breaks(&fixture);
    let foreign = Fixture::new("a\u{301}😀 z");
    assert!(matches!(
        other.ready_grapheme_count(&foreign.source.authority()),
        Err(crate::MarkdownLayoutBreakError::SourceMismatch)
    ));
}
