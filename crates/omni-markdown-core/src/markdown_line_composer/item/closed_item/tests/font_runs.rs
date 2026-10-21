//! Real SFNT missing-glyph and complete-grapheme fallback. The fixture removes only
//! x's cmap entry while retaining the other outlines, GSUB/GPOS tables, and mappings.

use super::*;

/// Rewrite one actual Roboto cmap subtable to omit x. The real shaper must produce
/// glyph zero itself; neither glyph responses nor the font port are substituted.
fn without_x() -> MarkdownLayoutFont {
    let mut bytes = ROBOTO.to_vec();
    let u16_at = |offset| {
        usize::from(u16::from_be_bytes(
            ROBOTO[offset..offset + 2].try_into().unwrap(),
        ))
    };
    let u32_at = |offset| u32::from_be_bytes(ROBOTO[offset..offset + 4].try_into().unwrap());
    // Locate the actual cmap table and its format-12 subtable in the source SFNT.
    // Every retained mapping below keeps its original glyph identity.
    let record = (0..u16_at(4))
        .map(|index| 12 + index * 16)
        .find(|&offset| &ROBOTO[offset..offset + 4] == b"cmap")
        .unwrap();
    let cmap = u32_at(record + 8) as usize;
    let subtable = (0..u16_at(cmap + 2))
        .map(|index| cmap + u32_at(cmap + 8 + index * 8) as usize)
        .find(|&offset| u16_at(offset) == 12)
        .unwrap();
    let mut groups = Vec::new();
    for index in 0..u32_at(subtable + 12) as usize {
        let offset = subtable + 16 + index * 12;
        let (start, end, glyph) = (u32_at(offset), u32_at(offset + 4), u32_at(offset + 8));
        if start <= u32::from('x') && end >= u32::from('x') {
            // Split only the group containing x. Keep every other character's
            // original glyph index and all capabilities unrelated to this omission.
            if start < u32::from('x') {
                groups.push((start, u32::from('x') - 1, glyph));
            }
            if end > u32::from('x') {
                groups.push((u32::from('x') + 1, end, glyph + u32::from('x') + 1 - start));
            }
        } else {
            groups.push((start, end, glyph));
        }
    }
    let length = 16 + groups.len() * 12;
    let mut replacement = Vec::new();
    replacement.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 4]); // One Unicode format-12 encoding record.
    replacement.extend_from_slice(&12_u32.to_be_bytes());
    replacement.extend_from_slice(&[0, 12, 0, 0]);
    replacement.extend_from_slice(&(length as u32).to_be_bytes());
    replacement.extend_from_slice(&0_u32.to_be_bytes());
    replacement.extend_from_slice(&(groups.len() as u32).to_be_bytes());
    for (start, end, glyph) in groups {
        for value in [start, end, glyph] {
            replacement.extend_from_slice(&value.to_be_bytes());
        }
    }
    // Keep the rewritten table within the original allocation, leaving other SFNT
    // tables in place. Resolve the resulting real font through its actual name data.
    assert!(replacement.len() <= u32_at(record + 12) as usize);
    bytes[cmap..cmap + replacement.len()].copy_from_slice(&replacement);
    let name = read_fonts::FontRef::from_index(&bytes, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    MarkdownLayoutFont::resolve_name_table_at_size(Arc::new(bytes), &name, &[], 16.0).unwrap()
}

/// Obtain the small fixture's real glyph records from an independent original
/// complete Job; expected records are never copied from the candidate.
fn original_glyphs(
    source: &MarkdownLayoutTextSource,
    actual: &MarkdownLayoutFont,
    range: Range<usize>,
    options: MarkdownShapingOptions,
) -> Vec<MarkdownShapedGlyph> {
    let authority = source.authority();
    let font = actual.authority();
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &authority,
        actual.clone(),
        &font,
        range,
        options,
    )
    .unwrap();
    // Prepare through actual one-byte/one-scalar Source work before shaping. Release
    // and inspect the oracle's real backing resources after reading its finite result.
    for _ in 0..100_000 {
        if job
            .step(
                &authority,
                &font,
                MarkdownShapingBudget {
                    bytes: 1,
                    scalars: 1,
                },
            )
            .unwrap()
            .status
            == MarkdownShapingStatus::AwaitingShape
        {
            break;
        }
    }
    job.shape(&authority, &font).unwrap();
    let count = job.profile().unwrap().glyph_count;
    let result = (0..count)
        .map(|ordinal| job.glyph(&authority, &font, ordinal).unwrap().unwrap())
        .collect();
    let receipt = job.resource_observer();
    drop(job);
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
    result
}

#[test]
fn missing_closed_gcb_reuses_actual_probe_coalesces_fonts_and_reshapes_original_context() {
    for (text, fallback_range) in [("**xx**", 2..4), ("**x\u{0338}x\u{0338}**", 2..6)] {
        let fixture = Fixture::new(text);
        let base = without_x();
        let fallback = font(ROBOTO);
        let catalog = super::super::super::super::MarkdownLineFontCatalog::new();
        let base_registration = catalog.register(&base).unwrap();
        let fallback_registration = catalog.register(&fallback).unwrap();
        let mut worker = fixture.worker(true, MarkdownLineComposeCancellation::new());
        let resources = worker.resources.observer();
        let mut ranges = Vec::new(); // Only this few-grapheme oracle retains results.
        let mut probes = Vec::new();
        let mut ended = false;
        for _ in 0..100_000 {
            let progress = worker.step(one()).unwrap();
            assert!(
                progress.bytes <= 1
                    && progress.scalars <= 1
                    && progress.records <= 1
                    && progress.metadata_units <= 1
                    && progress.shapes <= 1
            );
            match progress.answer {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    let selected = if request.continuation_font().is_some() {
                        let probe = worker.font_probe(&request).unwrap();
                        assert_eq!(probe.source_range(), range);
                        let glyphs = original_glyphs(
                            probe.source().unwrap(),
                            &base,
                            range.clone(),
                            probe.shaping_options(&style()).unwrap(),
                        );
                        let missing = glyphs.iter().any(|glyph| glyph.glyph_id == 0);
                        if range.start < fallback_range.end {
                            assert_eq!(range.len(), (fallback_range.len() / 2));
                        } // x plus its mark is one real grapheme, not a bare missing
                        // HarfBuzz cluster or independently selectable scalars.
                        probes.push((range.clone(), missing));
                        if missing {
                            &fallback_registration
                        } else {
                            &base_registration
                        }
                    } else {
                        &base_registration
                    };
                    worker
                        .provide_style(
                            request
                                .respond_registered(range, selected, style())
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineItemAnswer::ItemReady(facts) => {
                    // Compare the whole coalesced run with a fresh real shaping Job
                    // using its original paragraph/context facts and selected payload.
                    let mut item = worker.take_item().unwrap();
                    let actual = if facts.source_utf16 == fallback_range {
                        &fallback
                    } else {
                        &base
                    };
                    assert!(actual.permits(&item.font));
                    let expected = original_glyphs(
                        &fixture.source,
                        actual,
                        facts.source_utf16.clone(),
                        MarkdownShapingOptions {
                            direction: facts.direction,
                            script: facts.script,
                            language: None,
                            points: 16.0,
                            beginning_of_text: facts.beginning_of_text,
                            end_of_text: facts.end_of_text,
                            features: Vec::new(),
                        },
                    );
                    let count = item.job.profile().unwrap().glyph_count;
                    let actual_glyphs: Vec<_> = (0..count)
                        .map(|ordinal| item.glyph(ordinal).unwrap().unwrap())
                        .collect();
                    assert_eq!(actual_glyphs, expected);
                    assert!(actual_glyphs.iter().all(|glyph| glyph.glyph_id != 0));
                    assert_eq!(facts.paragraph_utf16, 0..fixture.source.utf16_len());
                    assert_eq!(facts.beginning_of_text, facts.source_utf16.start == 0);
                    assert_eq!(
                        facts.end_of_text,
                        facts.source_utf16.end == fixture.source.utf16_len()
                    );
                    assert!(matches!(
                        worker.step(one()).unwrap().answer,
                        MarkdownLineItemAnswer::NeedsFrontier(
                            MarkdownLineFrontierReason::PendingConsumer
                        )
                    ));
                    ranges.push(facts.source_utf16);
                    drop(item); // Only actual child release permits the next run.
                }
                MarkdownLineItemAnswer::EndOfSource => {
                    ended = true;
                    break;
                }
                _ => panic!("有限真实换Font夹具不应缺控制或进入未认证前沿"),
            }
        }
        assert!(ended);
        assert_eq!(
            ranges,
            [
                0..2,
                fallback_range.clone(),
                fallback_range.end..fixture.source.utf16_len()
            ]
        );
        assert_eq!(probes.iter().filter(|(_, missing)| *missing).count(), 2);
        assert!(
            probes
                .iter()
                .any(|(range, missing)| range.start == fallback_range.end && !missing)
        );
        // Both production run children and the independent oracle have been dropped;
        // check actual files, mappings, Source backing, and font payload release.
        drop(worker);
        let released = resources.snapshot().unwrap();
        assert_eq!(released.shapes.active, 0);
        assert_eq!(released.shapes.registered, released.shapes.settled);
        assert_eq!(released.shapes.files_created, released.shapes.files_closed);
        assert_eq!(
            released.shapes.mappings_created,
            released.shapes.mappings_released
        );
        let source = fixture.source.resource_observer();
        let mapping = fixture.source.original_backing_observer().unwrap();
        drop((
            fixture,
            base_registration,
            fallback_registration,
            catalog,
            base,
            fallback,
        ));
        assert!(source.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn pending_closed_missing_gcb_preserves_source_errno_before_cancel_and_drops_original_job() {
    for poison in [false, true] {
        let fixture = Fixture::new("**xx**");
        let base = without_x();
        let cancellation = MarkdownLineComposeCancellation::new();
        let mut worker = fixture.worker(true, cancellation.clone());
        let first = request(&mut worker, one());
        let range = first.source_range();
        worker
            .provide_style(first.respond(range, base, style()).unwrap())
            .unwrap();
        let missing = request(&mut worker, one());
        assert_eq!(missing.source_range(), 2..3);
        assert!(missing.continuation_font().is_some());
        let probe = worker.font_probe(&missing).unwrap();
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
        } // Existing injection reports EIO through real backing reclaim, not a
        // naturally induced kernel failure. The old font trial must retain it.
        cancellation.cancel();
        let failure = worker.step(one()).err().expect("取消不应发布缺字Job");
        if poison {
            assert!(matches!(
                failure,
                MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                )
            ));
            assert!(matches!(
                probe.validate(),
                Err(MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                ))
            ));
        } else {
            assert!(matches!(failure, MarkdownLineComposeError::Cancelled));
            assert!(matches!(
                probe.validate(),
                Err(MarkdownLineComposeError::Cancelled)
            ));
        }
        // Typed failure and actual Job resource release establish the public
        // cancellation outcome without mirroring the worker's private payload slot.
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
