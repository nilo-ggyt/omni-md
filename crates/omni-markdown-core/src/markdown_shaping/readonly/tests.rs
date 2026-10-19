//! 真正worker构造与join移交；只比固定字段/流式摘要，不建立全文或glyph Vec镜像。
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownLayoutFontBytes, MarkdownShapingBudget, MarkdownShapingOptions,
};
use std::path::Path;

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));
fn make_job(source: &MarkdownLayoutTextSource, font: &MarkdownLayoutFont) -> MarkdownShapingJob {
    let sa = source.authority();
    let fa = font.authority();
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &sa,
        font.clone(),
        &fa,
        0..source.utf16_len(),
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
        job.step(&sa, &fa, MarkdownShapingBudget::default())
            .unwrap();
    }
    job.shape(&sa, &fa).unwrap();
    job
}
fn setup(
    text: &str,
) -> (
    tempfile::TempDir,
    MarkdownDocumentBuffer,
    MarkdownLayoutTextSource,
    MarkdownLayoutFont,
    MarkdownShapingJob,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("readonly.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 1_048_576).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
    let job = make_job(&source, &font);
    (directory, document, source, font, job)
}
fn build(
    job: &mut MarkdownShapingJob,
    source: &MarkdownLayoutTextSource,
    font: &MarkdownLayoutFont,
) -> MarkdownShapedSpool {
    let mut builder =
        MarkdownShapedSpoolBuilder::new(job, &source.authority(), &font.authority()).unwrap();
    while builder.status == MarkdownShapedSpoolStatus::Building {
        let step = builder.step(job, 31).unwrap();
        assert!(step.glyphs > 0 && step.glyphs <= 31);
    }
    builder.seal(job).unwrap()
}
fn hash(mut value: u64, glyph: MarkdownShapedGlyph) -> u64 {
    for byte in encode(glyph).unwrap() {
        value = value.wrapping_mul(257).wrapping_add(u64::from(byte));
    }
    value
}

#[test]
fn sealed_owner_auto_send_preserves_exact_glyphs_and_actual_file_font_source_release() {
    fn assert_send<T: Send>() {}
    assert_send::<MarkdownShapedSpool>();
    assert_send::<MarkdownLayoutTextSource>();
    assert_send::<MarkdownLayoutFont>();
    let worker = std::thread::spawn(|| {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thread-source.md");
        std::fs::write(&path, "office a\u{301}🙂 ".repeat(80)).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1_048_576).unwrap();
        let backing = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let sa = source.authority();
        let bytes = MarkdownLayoutFontBytes::capture_file(
            Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
            )),
            2_097_152,
        )
        .unwrap();
        let mapping = bytes.resource_observer();
        let font = MarkdownLayoutFont::resolve(bytes.blob(), "Roboto-Regular", &[]).unwrap();
        let fa = font.authority();
        let mut job = make_job(&source, &font);
        let job_resources = job.resource_observer();
        let count = job.profile().unwrap().glyph_count;
        let mut expected = 0;
        for index in 0..count {
            expected = hash(expected, job.glyph(&sa, &fa, index).unwrap().unwrap());
        }
        let spool = build(&mut job, &source, &font);
        assert!(Weak::ptr_eq(&spool.authority().0, &job.index_identity()));
        let resources = spool.resource_observer();
        drop(job);
        drop(source);
        drop(font);
        drop(bytes);
        drop(document);
        drop(directory);
        assert_eq!(
            (
                job_resources.snapshot().files_open,
                job_resources.snapshot().mappings_open
            ),
            (0, 0)
        );
        (spool, sa, fa, expected, backing, mapping, resources)
    });
    let (mut spool, sa, fa, expected, backing, mapping, resources) = worker.join().unwrap();
    assert!(resources.snapshot().file_open);
    assert!(
        !resources.snapshot().source_payload_released
            && !resources.snapshot().font_payload_released
    );
    assert!(backing.mapped_bytes() > 0 && mapping.mapped_bytes() > 0); // 强边合法交给主线程，join不是源已释放。
    let authority = spool.authority();
    let mut actual = 0;
    for index in 0..spool.glyph_count() {
        actual = hash(
            actual,
            spool.glyph(&authority, &sa, &fa, index).unwrap().unwrap(),
        );
    }
    assert_eq!(actual, expected);
    assert!(
        spool
            .glyph(&authority, &sa, &fa, spool.glyph_count())
            .unwrap()
            .is_none()
    );
    spool.close().unwrap();
    let final_resources = resources.snapshot();
    assert!(
        !final_resources.file_open
            && final_resources.close_succeeded
            && !final_resources.cleanup_failed
    );
    assert!(final_resources.source_payload_released && final_resources.font_payload_released);
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    assert!(
        mapping.mapping_drop_completed()
            && mapping.unmap_succeeded()
            && mapping.mapped_bytes() == 0
            && !mapping.failed()
    );
    assert!(mapping.source_descriptor_closed() && mapping.backing_descriptor_closed());
}

#[test]
fn identical_source_font_cannot_substitute_another_job_nonce() {
    let (_dir, _document, source, font, mut job) = setup("office");
    let mut other = make_job(&source, &font);
    let mut builder =
        MarkdownShapedSpoolBuilder::new(&mut job, &source.authority(), &font.authority()).unwrap();
    let resources = builder.resource_observer();
    assert!(matches!(
        builder.step(&mut other, 1),
        Err(MarkdownShapedSpoolError::OutputMismatch)
    ));
    assert_eq!(other.status(), MarkdownShapingStatus::Ready);
    assert!(!resources.snapshot().file_open && resources.snapshot().close_succeeded);
    let mut spool = build(&mut job, &source, &font);
    let other_spool = build(&mut other, &source, &font);
    assert!(matches!(
        spool.glyph(
            &other_spool.authority(),
            &source.authority(),
            &font.authority(),
            0
        ),
        Err(MarkdownShapedSpoolError::OutputMismatch)
    ));
}

#[test]
fn same_text_or_postscript_name_cannot_replace_original_source_or_font_authority() {
    for replace_source in [true, false] {
        let (_dir, document, source, font, mut job) = setup("office");
        let other_source = MarkdownLayoutTextSource::capture(&document);
        let other_font =
            MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
        let mut spool = build(&mut job, &source, &font);
        let authority = spool.authority();
        let result = spool.glyph(
            &authority,
            &if replace_source {
                other_source.authority()
            } else {
                source.authority()
            },
            &if replace_source {
                font.authority()
            } else {
                other_font.authority()
            },
            0,
        );
        if replace_source {
            assert!(matches!(
                result,
                Err(MarkdownShapedSpoolError::SourceMismatch)
            ));
        } else {
            assert!(matches!(
                result,
                Err(MarkdownShapedSpoolError::FontMismatch)
            ));
        }
        assert_eq!(spool.status(), MarkdownShapedSpoolStatus::Poisoned);
        assert!(!spool.resource_observer().snapshot().file_open);
    }
}

#[test]
fn early_seal_and_oversized_step_do_not_publish_a_partial_file() {
    let (_dir, _document, source, font, mut job) = setup("office office office");
    let mut partial =
        MarkdownShapedSpoolBuilder::new(&mut job, &source.authority(), &font.authority()).unwrap();
    partial.step(&mut job, 1).unwrap();
    let resources = partial.resource_observer();
    assert!(matches!(
        partial.seal(&mut job),
        Err(MarkdownShapedSpoolError::Pending)
    ));
    assert!(!resources.snapshot().file_open && resources.snapshot().close_succeeded);
    let mut oversized =
        MarkdownShapedSpoolBuilder::new(&mut job, &source.authority(), &font.authority()).unwrap();
    assert!(matches!(
        oversized.step(&mut job, MARKDOWN_SHAPED_SPOOL_STEP + 1),
        Err(MarkdownShapedSpoolError::InvalidRequest)
    ));
    assert_eq!(oversized.consumed, 0);
    assert!(!oversized.resources.snapshot().file_open);
}

#[test]
fn empty_run_has_a_verified_true_eof_file_without_invented_glyphs() {
    let (_dir, _document, source, font, mut job) = setup("");
    let mut spool = build(&mut job, &source, &font);
    let authority = spool.authority();
    assert_eq!(spool.glyph_count(), 0);
    assert_eq!(spool.source_range(), 0..0);
    assert!(
        spool
            .glyph(&authority, &source.authority(), &font.authority(), 0)
            .unwrap()
            .is_none()
    );
    spool.close().unwrap();
}

#[test]
fn actual_write_and_sync_failure_prefixes_never_become_readonly_and_cleanup_preserves_first_error()
{
    for sync in [true, false] {
        let (_dir, _document, source, font, mut job) = setup("office");
        let mut builder =
            MarkdownShapedSpoolBuilder::new(&mut job, &source.authority(), &font.authority())
                .unwrap();
        let resources = builder.resource_observer();
        if sync {
            while builder.status == MarkdownShapedSpoolStatus::Building {
                builder.step(&mut job, 256).unwrap();
            }
            let file = &mut builder.payload.as_mut().unwrap().file;
            file.fail_sync_after_real();
            file.fail_close_after_real();
            assert!(matches!(
                builder.seal(&mut job),
                Err(MarkdownShapedSpoolError::Storage(_))
            ));
        } else {
            let file = &mut builder.payload.as_mut().unwrap().file;
            file.fail_write_after_real();
            file.fail_close_after_real();
            assert!(matches!(
                builder.step(&mut job, 256),
                Err(MarkdownShapedSpoolError::Storage(_))
            ));
        }
        let facts = resources.snapshot();
        assert!(!facts.file_open && facts.close_succeeded && facts.cleanup_failed);
        assert_eq!(
            facts.first_error.unwrap().operation,
            if sync {
                MarkdownShapedSpoolIoOperation::Sync
            } else {
                MarkdownShapedSpoolIoOperation::Write
            }
        );
    }
}

#[test]
fn truncated_file_and_post_seal_cancellation_revoke_the_unique_reader() {
    for cancel in [true, false] {
        let (_dir, _document, source, font, mut job) = setup("office");
        let mut spool = build(&mut job, &source, &font);
        let authority = spool.authority();
        if cancel {
            job.cancellation().request();
        } else {
            spool.payload.as_ref().unwrap().file.truncate_after_seal();
        }
        let result = spool.glyph(&authority, &source.authority(), &font.authority(), 0);
        if cancel {
            assert!(matches!(result, Err(MarkdownShapedSpoolError::Cancelled)));
        } else {
            assert!(matches!(result, Err(MarkdownShapedSpoolError::Storage(_))));
        }
        assert!(spool.payload.is_none());
        assert!(
            !spool.resources.snapshot().file_open && spool.resources.snapshot().close_succeeded
        );
    }
}

#[test]
fn source_reclaim_error_remains_the_cause_even_after_seal_and_cancel() {
    let (_dir, document, source, font, mut job) = setup("office");
    let mut spool = build(&mut job, &source, &font);
    let authority = spool.authority();
    source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    document.fail_next_original_reclaim_for_test();
    assert!(document.reclaim_original_read_windows_for_test().is_err());
    job.cancellation().request();
    assert!(matches!(
        spool.glyph(&authority, &source.authority(), &font.authority(), 0),
        Err(MarkdownShapedSpoolError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        ))
    ));
    assert_eq!(spool.status(), MarkdownShapedSpoolStatus::Poisoned);
    assert!(!spool.resources.snapshot().file_open);
}

#[test]
fn bounded_batch_preserves_every_field_and_rejects_oversized_reads_before_output() {
    let (_dir, _document, source, font, mut job) = setup(&"office a\u{301} ".repeat(80));
    let mut spool = build(&mut job, &source, &font);
    let authority = spool.authority();
    let mut output = [empty_glyph(); MARKDOWN_SHAPED_SPOOL_STEP];
    let count = output.len().min(spool.glyph_count());
    assert_eq!(
        spool
            .read_batch(
                &authority,
                &source.authority(),
                &font.authority(),
                0..count,
                &mut output[..count]
            )
            .unwrap(),
        count
    );
    for (index, actual) in output[..count].iter().enumerate() {
        assert_eq!(
            *actual,
            job.glyph(&source.authority(), &font.authority(), index)
                .unwrap()
                .unwrap()
        );
    }
    let mut oversized = [empty_glyph(); MARKDOWN_SHAPED_SPOOL_STEP + 1];
    assert!(matches!(
        spool.read_batch(
            &authority,
            &source.authority(),
            &font.authority(),
            0..MARKDOWN_SHAPED_SPOOL_STEP + 1,
            &mut oversized
        ),
        Err(MarkdownShapedSpoolError::InvalidRequest)
    ));
    assert!(oversized.iter().all(|glyph| *glyph == empty_glyph()));
    assert!(spool.payload.is_none() && !spool.resources.snapshot().file_open);
}
