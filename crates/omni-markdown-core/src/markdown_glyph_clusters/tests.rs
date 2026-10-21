//! Independently aggregate glyphs from the original Ready job for finite fixtures. Cluster-index
//! verification does not establish completed soft-line layout.
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownLayoutFont, MarkdownLayoutTextSource, MarkdownShapingBudget,
    MarkdownShapingOptions, MarkdownShapingStatus,
};
use std::sync::Arc;

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));

fn ready(
    text: &str,
    direction: MarkdownShapingDirection,
) -> (
    MarkdownShapingJob,
    MarkdownLayoutTextAuthority,
    MarkdownLayoutFontAuthority,
) {
    // Open a real isolated document; the returned shaping job owns its actual captured Source, not
    // a replacement test reader or a synthetic glyph provider.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("有限cluster.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
    ready_from(&document, 0..document.utf16_len(), direction)
}

fn ready_from(
    document: &MarkdownDocumentBuffer,
    range: Range<usize>,
    direction: MarkdownShapingDirection,
) -> (
    MarkdownShapingJob,
    MarkdownLayoutTextAuthority,
    MarkdownLayoutFontAuthority,
) {
    // Ingest the same captured source in bounded steps and shape with an actual Font. Original
    // Source/font authorities accompany the job through every later index query.
    let source = MarkdownLayoutTextSource::capture(document);
    let authority = source.authority();
    let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
    let font_authority = font.authority();
    let mut job = MarkdownShapingJob::new(
        source,
        &authority,
        font,
        &font_authority,
        range,
        MarkdownShapingOptions {
            direction,
            script: *b"Latn",
            language: None,
            points: 16.0,
            beginning_of_text: true,
            end_of_text: true,
            features: Vec::new(),
        },
    )
    .unwrap();
    while job.status() == MarkdownShapingStatus::Ingesting {
        job.step(
            &authority,
            &font_authority,
            MarkdownShapingBudget {
                bytes: 7,
                scalars: 3,
            },
        )
        .unwrap();
    }
    job.shape(&authority, &font_authority).unwrap();
    // Return only the original job and weak authority tickets, without retaining another live
    // document or source-path owner.
    (job, authority, font_authority)
}

fn finish(index: &mut MarkdownGlyphClusterIndex, job: &mut MarkdownShapingJob, budget: usize) {
    // Resume the actual index per quantum; even an unlimited caller request retains the production
    // maximum glyph step instead of completing a giant pending cluster in one operation.
    while index.status() != MarkdownGlyphClusterStatus::Ready {
        let step = index.step(job, budget).unwrap();
        assert!(step.glyphs <= budget.min(MARKDOWN_GLYPH_CLUSTER_STEP));
    }
}

#[test]
fn exact_ltr_and_rtl_clusters_keep_every_original_glyph_field() {
    for direction in [
        MarkdownShapingDirection::LeftToRight,
        MarkdownShapingDirection::RightToLeft,
    ] {
        let (mut job, source, font) = ready("office a\u{301} 🙂 fi", direction);
        let count = job.profile().unwrap().glyph_count;
        // Only the finite test oracle collects the small glyph sample. The production index reads
        // original glyphs individually and retains one metadata page, not a complete glyph mirror.
        let glyphs = (0..count)
            .map(|at| job.glyph(&source, &font, at).unwrap().unwrap())
            .collect::<Vec<_>>();
        let mut index = MarkdownGlyphClusterIndex::new(&mut job, &source, &font).unwrap();
        assert_eq!(
            index.containing(&mut job, 0).unwrap(),
            MarkdownGlyphClusterAnswer::Pending
        );
        finish(&mut index, &mut job, 1);
        let mut at = 0;
        let mut ordinal = 0;
        let mut prefix = (0, 0);
        let mut visited = vec![false; count];
        while at < job.source_range().end {
            let MarkdownGlyphClusterAnswer::Known(Some(cluster)) =
                index.containing(&mut job, at).unwrap()
            else {
                panic!("真实完整cluster缺失")
            };
            // Ordinal and position queries must agree on actual ligature, combining, non-BMP, and
            // RTL records, including their original signed prefixes and visual ranges.
            assert_eq!(
                index.at_ordinal(&mut job, ordinal).unwrap(),
                Some(cluster.clone())
            );
            assert_eq!(cluster.source_utf16.start, at);
            assert_eq!((cluster.prefix_x_26_6, cluster.prefix_y_26_6), prefix);
            let original = &glyphs[cluster.glyphs.clone()];
            assert!(
                original
                    .iter()
                    .all(|glyph| glyph.source_utf16_cluster == at)
            );
            assert_eq!(
                cluster.x_advance_26_6,
                original
                    .iter()
                    .map(|glyph| i64::from(glyph.x_advance))
                    .sum::<i64>()
            );
            assert_eq!(
                cluster.y_advance_26_6,
                original
                    .iter()
                    .map(|glyph| i64::from(glyph.y_advance))
                    .sum::<i64>()
            );
            assert_eq!(
                cluster.flags,
                original.iter().fold(0, |flags, glyph| flags | glyph.flags)
            );
            assert_eq!(
                cluster.missing_glyphs,
                original.iter().filter(|glyph| glyph.glyph_id == 0).count()
            );
            for unit in cluster.source_utf16.clone() {
                // Every source unit inside a shaping cluster returns that same complete record;
                // this does not authorize splitting glyph reuse at interior grapheme positions.
                assert_eq!(
                    index.containing(&mut job, unit).unwrap(),
                    MarkdownGlyphClusterAnswer::Known(Some(cluster.clone()))
                );
            }
            for glyph in cluster.glyphs.clone() {
                assert!(!visited[glyph]);
                visited[glyph] = true;
            }
            prefix.0 += cluster.x_advance_26_6;
            prefix.1 += cluster.y_advance_26_6;
            at = cluster.source_utf16.end;
            ordinal += 1;
        }
        assert!(visited.into_iter().all(|value| value));
        assert_eq!(
            index.containing(&mut job, at).unwrap(),
            MarkdownGlyphClusterAnswer::Known(None)
        );
        assert_eq!(index.at_ordinal(&mut job, ordinal).unwrap(), None);
    }
}

#[test]
fn ordinal_reads_reject_unsealed_or_past_eof_metadata_and_release_the_index() {
    for complete in [false, true] {
        let (mut job, source, font) =
            ready("office a\u{301} 🙂", MarkdownShapingDirection::RightToLeft);
        let mut index = MarkdownGlyphClusterIndex::new(&mut job, &source, &font).unwrap();
        let resources = index.resource_observer();
        let ordinal = if complete {
            finish(&mut index, &mut job, 1);
            index.count() + 1 // The position after exact record EOF is not another valid empty result.
        } else {
            index.step(&mut job, 1).unwrap();
            assert_eq!(index.status(), MarkdownGlyphClusterStatus::Building);
            0 // A genuinely advanced prefix still grants no sealed-index ordinal access.
        };
        assert!(matches!(
            index.at_ordinal(&mut job, ordinal),
            Err(MarkdownGlyphClusterError::InvalidRequest)
        ));
        assert_eq!(index.status(), MarkdownGlyphClusterStatus::Poisoned);
        let facts = resources.snapshot();
        assert!(
            !facts.file_open
                && facts.mapped_bytes == 0
                && facts.buffer_bytes == 0
                && !facts.cleanup_failed
        );
        // Failure revokes only this derived index; its original owner still uses the Ready job and
        // its actual Source/font resources.
        assert!(job.glyph(&source, &font, 0).unwrap().is_some());
    }
}

#[test]
fn another_identical_job_cannot_publish_original_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("同一冻结源.md");
    std::fs::write(&path, "office").unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
    let source_value = MarkdownLayoutTextSource::capture(&document);
    let source = source_value.authority();
    let font_value = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
    let font = font_value.authority();
    let make = || {
        let mut job = MarkdownShapingJob::new(
            source_value.clone(),
            &source,
            font_value.clone(),
            &font,
            0..document.utf16_len(),
            MarkdownShapingOptions {
                direction: MarkdownShapingDirection::LeftToRight,
                script: *b"Latn",
                language: None,
                points: 16.0,
                beginning_of_text: true,
                end_of_text: true,
                features: Vec::new(),
            },
        )
        .unwrap();
        while job.status() == MarkdownShapingStatus::Ingesting {
            job.step(&source, &font, MarkdownShapingBudget::default())
                .unwrap();
        }
        job.shape(&source, &font).unwrap();
        job
    };
    let mut original = make();
    let mut foreign = make();
    // Both jobs deliberately share Source/font/range/options. Rejection must come from original-job
    // identity rather than a different input source.
    assert!(foreign.glyph(&source, &font, 0).unwrap().is_some());
    let mut index = MarkdownGlyphClusterIndex::new(&mut original, &source, &font).unwrap();
    let resources = index.resource_observer();
    assert!(matches!(
        index.step(&mut foreign, 1),
        Err(MarkdownGlyphClusterError::JobMismatch)
    ));
    assert_eq!(index.status(), MarkdownGlyphClusterStatus::Poisoned);
    assert!(!resources.snapshot().file_open);
    assert_eq!(resources.snapshot().mapped_bytes, 0);
    assert_eq!(resources.snapshot().buffer_bytes, 0);
    assert!(!resources.snapshot().cleanup_failed);

    // The original job can still build its own sealed index; another job with identical Source/font
    // cannot borrow its records through the ordinal path either.
    let mut ordinal_index = MarkdownGlyphClusterIndex::new(&mut original, &source, &font).unwrap();
    finish(&mut ordinal_index, &mut original, 1);
    let ordinal_resources = ordinal_index.resource_observer();
    assert!(matches!(
        ordinal_index.at_ordinal(&mut foreign, 0),
        Err(MarkdownGlyphClusterError::JobMismatch)
    ));
    let facts = ordinal_resources.snapshot();
    assert!(
        !facts.file_open
            && facts.mapped_bytes == 0
            && facts.buffer_bytes == 0
            && !facts.cleanup_failed
    );
}

#[test]
fn index_weak_keys_do_not_retain_source_font_or_original_job() {
    let (mut job, source, font) = ready("office a\u{301}", MarkdownShapingDirection::LeftToRight);
    let original_resources = job.resource_observer();
    let mut index = MarkdownGlyphClusterIndex::new(&mut job, &source, &font).unwrap();
    finish(&mut index, &mut job, 3);
    let index_resources = index.resource_observer();
    drop(job);
    // The derived index remains alive while original Source/font payloads and shaper descriptors/
    // mappings finish their actual lifetime; index keys must not retain that original owner graph.
    assert!(original_resources.resources_released());
    // This short index still owns its unique metadata buffer. Private seal need not create a file
    // merely to retain derived data; only dropping the index returns that remaining buffer.
    assert!(!index_resources.snapshot().file_open);
    assert_eq!(index_resources.snapshot().buffer_bytes, 16 * 1024);
    drop(index);
    assert!(!index_resources.snapshot().file_open);
    assert_eq!(index_resources.snapshot().mapped_bytes, 0);
    assert_eq!(index_resources.snapshot().buffer_bytes, 0);
    assert!(!index_resources.snapshot().cleanup_failed);
}

#[test]
fn one_long_cluster_yields_without_sealing_a_fake_chunk_boundary() {
    // Combining glyphs share one actual cluster. Several small steps must retain Pending for its
    // entire source interval instead of committing a boundary at each glyph-budget limit.
    let text = format!("a{}b", "\u{301}".repeat(2048));
    let (mut job, source, font) = ready(&text, MarkdownShapingDirection::LeftToRight);
    let mut index = MarkdownGlyphClusterIndex::new(&mut job, &source, &font).unwrap();
    for _ in 0..3 {
        let step = index.step(&mut job, 7).unwrap();
        assert_eq!(step.glyphs, 7);
        assert_eq!(step.clusters, 0);
        assert_eq!(
            index.containing(&mut job, 0).unwrap(),
            MarkdownGlyphClusterAnswer::Pending
        );
    }
    finish(&mut index, &mut job, usize::MAX);
    let MarkdownGlyphClusterAnswer::Known(Some(value)) = index.containing(&mut job, 1024).unwrap()
    else {
        panic!("长cluster缺失")
    };
    assert_eq!(value.source_utf16, 0..2049);
    let snapshot = index.resource_observer().snapshot();
    assert_eq!(snapshot.peak_mapped_bytes, 0);
    assert_eq!(
        snapshot.peak_buffer_bytes,
        crate::MARKDOWN_LAYOUT_INDEX_BYTES
    );
    assert!(!snapshot.cleanup_failed);
}

#[test]
fn multi_page_index_is_bounded_and_cancel_drops_only_its_own_resources() {
    // Force actual multipage index storage and read records across pages before cancellation;
    // observed file/buffer release is separate from kernel RSS or the source job's ownership.
    let (mut job, source, font) = ready(
        &"abcdef ".repeat(400),
        MarkdownShapingDirection::LeftToRight,
    );
    let mut index = MarkdownGlyphClusterIndex::new(&mut job, &source, &font).unwrap();
    let observer = index.resource_observer();
    finish(&mut index, &mut job, 53);
    for at in [0, 256, 1000, 2799] {
        assert!(matches!(
            index.containing(&mut job, at).unwrap(),
            MarkdownGlyphClusterAnswer::Known(Some(_))
        ));
    }
    assert_eq!(observer.snapshot().peak_mapped_bytes, 0);
    assert_eq!(
        observer.snapshot().peak_buffer_bytes,
        crate::MARKDOWN_LAYOUT_INDEX_BYTES
    );
    index.cancel();
    assert!(!observer.snapshot().file_open);
    assert_eq!(observer.snapshot().mapped_bytes, 0);
    assert_eq!(observer.snapshot().buffer_bytes, 0);
    assert!(!observer.snapshot().cleanup_failed);
    // Cancelling the derived index does not cancel the original job; its owner still reads actual glyphs.
    assert!(job.glyph(&source, &font, 0).unwrap().is_some());
}

#[test]
fn cancelled_job_revokes_an_already_ready_cluster_index() {
    // Sealed metadata remains subordinate to the actual source job's cancellation, including
    // queries that would otherwise be satisfied entirely from derived records.
    let (mut job, source, font) = ready("office", MarkdownShapingDirection::LeftToRight);
    let mut index = MarkdownGlyphClusterIndex::new(&mut job, &source, &font).unwrap();
    let resources = index.resource_observer();
    finish(&mut index, &mut job, 2);
    job.cancel();
    assert!(matches!(
        index.containing(&mut job, 0),
        Err(MarkdownGlyphClusterError::Shaping(
            MarkdownShapingError::Cancelled
        ))
    ));
    assert!(!resources.snapshot().file_open);
    assert_eq!(resources.snapshot().mapped_bytes, 0);
    assert_eq!(resources.snapshot().buffer_bytes, 0);
}

#[test]
fn empty_run_and_invalid_budget_are_explicit_without_phantom_clusters() {
    // An empty run transitions from Pending to a real sealed EOF with zero records. A later invalid
    // step budget must still fail explicitly rather than manufacturing a cluster or silent success.
    let (mut empty, source, font) = ready("", MarkdownShapingDirection::LeftToRight);
    let mut index = MarkdownGlyphClusterIndex::new(&mut empty, &source, &font).unwrap();
    assert_eq!(
        index.containing(&mut empty, 0).unwrap(),
        MarkdownGlyphClusterAnswer::Pending
    );
    assert_eq!(index.step(&mut empty, 1).unwrap().clusters, 0);
    assert_eq!(
        index.containing(&mut empty, 0).unwrap(),
        MarkdownGlyphClusterAnswer::Known(None)
    );
    assert_eq!(index.at_ordinal(&mut empty, 0).unwrap(), None);
    let observer = index.resource_observer();
    assert!(matches!(
        index.step(&mut empty, 0),
        Err(MarkdownGlyphClusterError::InvalidRequest)
    ));
    assert!(!observer.snapshot().file_open);
}

#[test]
fn record_codec_preserves_signed_advances_and_rejects_corruption() {
    // Keep the existing fixed-record regression: signed extremes survive encoding, while changed
    // reserved bytes must reject the record instead of becoming an accepted derived cluster.
    let value = MarkdownGlyphCluster {
        source_utf16: 100..105,
        glyphs: 8..11,
        prefix_x_26_6: -120,
        prefix_y_26_6: i64::MIN,
        x_advance_26_6: 71,
        y_advance_26_6: -200,
        flags: 7,
        missing_glyphs: 2,
    };
    assert_eq!(decode(encode(&value).unwrap()).unwrap(), value);
    let mut raw = encode(&value).unwrap();
    raw[63] = 1;
    assert!(matches!(
        decode(raw),
        Err(MarkdownGlyphClusterError::InconsistentGlyphs)
    ));
}

#[test]
fn vertical_directions_preserve_nonzero_global_source_coordinates_and_advances() {
    // Shape an actual nonzero global range in both vertical directions. Compare complete original
    // glyph records rather than renumbering source positions or reducing results to horizontal width.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("垂直有界范围.md");
    let prefix = "前🙂";
    let run = "office a\u{301}";
    std::fs::write(&path, format!("{prefix}{run}后🙂")).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
    let range =
        prefix.encode_utf16().count()..prefix.encode_utf16().count() + run.encode_utf16().count();
    for direction in [
        MarkdownShapingDirection::TopToBottom,
        MarkdownShapingDirection::BottomToTop,
    ] {
        let (mut job, source, font) = ready_from(&document, range.clone(), direction);
        let count = job.profile().unwrap().glyph_count;
        let original = (0..count)
            .map(|at| job.glyph(&source, &font, at).unwrap().unwrap())
            .collect::<Vec<_>>();
        let mut index = MarkdownGlyphClusterIndex::new(&mut job, &source, &font).unwrap();
        finish(&mut index, &mut job, 1);
        let mut at = range.start;
        let mut ordinal = 0;
        let mut prefix = (0, 0);
        let mut visited = vec![false; count];
        while at < range.end {
            let MarkdownGlyphClusterAnswer::Known(Some(value)) =
                index.containing(&mut job, at).unwrap()
            else {
                panic!("垂直cluster缺失")
            };
            // Ordinal locates the original record without renumbering global source coordinates or
            // bottom-to-top visual glyph ranges.
            assert_eq!(
                index.at_ordinal(&mut job, ordinal).unwrap(),
                Some(value.clone())
            );
            assert_eq!(value.source_utf16.start, at);
            assert!(value.source_utf16.end <= range.end);
            assert_eq!((value.prefix_x_26_6, value.prefix_y_26_6), prefix);
            let actual = &original[value.glyphs.clone()];
            assert!(actual.iter().all(|glyph| glyph.source_utf16_cluster == at));
            assert_eq!(
                value.x_advance_26_6,
                actual
                    .iter()
                    .map(|glyph| i64::from(glyph.x_advance))
                    .sum::<i64>()
            );
            assert_eq!(
                value.y_advance_26_6,
                actual
                    .iter()
                    .map(|glyph| i64::from(glyph.y_advance))
                    .sum::<i64>()
            );
            for glyph in value.glyphs.clone() {
                assert!(!visited[glyph]);
                visited[glyph] = true;
            }
            prefix.0 += value.x_advance_26_6;
            prefix.1 += value.y_advance_26_6;
            at = value.source_utf16.end;
            ordinal += 1;
        }
        assert!(visited.into_iter().all(|value| value));
        assert_eq!(
            index.containing(&mut job, range.end).unwrap(),
            MarkdownGlyphClusterAnswer::Known(None)
        );
        assert_eq!(index.at_ordinal(&mut job, ordinal).unwrap(), None);
    }
}

#[test]
fn real_source_reclaim_error_revokes_ready_metadata_even_at_eof() {
    // Complete the index first, then fail actual original-window reclaim. Both EOF access paths
    // must consult original authority before returning an apparently clean empty result.
    for ordinal_read in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("已封存源错误.md");
        std::fs::write(&path, "office a\u{301}").unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
        let end = document.utf16_len();
        let (mut job, source, font) =
            ready_from(&document, 0..end, MarkdownShapingDirection::LeftToRight);
        let mut index = MarkdownGlyphClusterIndex::new(&mut job, &source, &font).unwrap();
        finish(&mut index, &mut job, 2);
        let ordinal = index.count();
        let resources = index.resource_observer();
        document.fail_next_original_reclaim_for_test();
        assert_eq!(
            document.reclaim_original_read_windows_for_test(),
            Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
        );
        // Position and ordinal EOF queries expose the same actual Source EIO before any clean None.
        let error = if ordinal_read {
            index.at_ordinal(&mut job, ordinal).unwrap_err()
        } else {
            index.containing(&mut job, end).unwrap_err()
        };
        assert!(matches!(
            error,
            MarkdownGlyphClusterError::Shaping(MarkdownShapingError::Source(
                crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert!(!resources.snapshot().file_open);
        assert_eq!(resources.snapshot().mapped_bytes, 0);
        assert_eq!(resources.snapshot().buffer_bytes, 0);
    }
}
