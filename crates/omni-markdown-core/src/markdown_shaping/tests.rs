//! 真实冻结piece与真实字体之间的契约回归；不把同一算法对照当成Native功能或总峰值证明。
use super::*;
use harfrust::ShaperFont;
use std::sync::Arc;

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));

fn font() -> MarkdownLayoutFont {
    MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap()
}
fn document(text: &str) -> (tempfile::TempDir, crate::MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded-shaping.md");
    std::fs::write(&path, text).unwrap();
    let document =
        crate::MarkdownDocumentBuffer::open(&path, u64::try_from(text.len() + 65_536).unwrap())
            .unwrap();
    (directory, document)
}
fn options(script: [u8; 4], direction: MarkdownShapingDirection) -> MarkdownShapingOptions {
    MarkdownShapingOptions {
        direction,
        script,
        language: Some("zh-CN".into()),
        points: 16.0,
        beginning_of_text: false,
        end_of_text: false,
        features: Vec::new(),
    }
}
fn fill(
    job: &mut MarkdownShapingJob,
    source: &MarkdownLayoutTextAuthority,
    font: &MarkdownLayoutFontAuthority,
    bytes: usize,
    scalars: usize,
) {
    while job.status() == MarkdownShapingStatus::Ingesting {
        let step = job
            .step(source, font, MarkdownShapingBudget { bytes, scalars })
            .unwrap();
        assert!(step.bytes > 0 && step.bytes <= bytes.min(MARKDOWN_LAYOUT_SOURCE_BYTES));
        assert!(step.scalars <= scalars);
    }
    assert_eq!(job.status(), MarkdownShapingStatus::AwaitingShape);
}

#[test]
fn real_piece_byte_splits_preserve_global_utf16_clusters_and_exact_shaper_fields() {
    let prefix = "外🙂甲乙🙂اب:";
    let run = "office a\u{301}🙂";
    let suffix = "عربي🙂尾部";
    let text = format!("{prefix}{run}{suffix}");
    let (_directory, document) = document(&text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_authority = source.authority();
    let font = font();
    let font_authority = font.authority();
    let origin = prefix.encode_utf16().count();
    let end = origin + run.encode_utf16().count();
    // 同一真实run用不同bytes/scalars配额切入组合字符与emoji内部；正文输入没有中途BOT/EOT。
    for (bytes, scalars) in [(1, 1), (2, 1), (3, 2), (4, 3), (7, 2), (4096, 1024)] {
        let mut job = MarkdownShapingJob::new(
            source.clone(),
            &source_authority,
            font.clone(),
            &font_authority,
            origin..end,
            options(*b"Latn", MarkdownShapingDirection::LeftToRight),
        )
        .unwrap();
        assert!(matches!(
            job.shape(&source_authority, &font_authority),
            Err(MarkdownShapingError::Pending)
        ));
        fill(&mut job, &source_authority, &font_authority, bytes, scalars);
        let payload = job.payload.as_ref().unwrap();
        let before: Vec<u32> = prefix.chars().rev().take(5).map(u32::from).collect();
        let after: Vec<u32> = suffix.chars().take(5).map(u32::from).collect();
        assert_eq!(payload.buffer.pre_context_codepoints(), before);
        assert_eq!(payload.buffer.post_context_codepoints(), after);
        let mut reference = Buffer::new();
        reference.set_direction(job.options.direction);
        reference.set_script(Some(job.options.script));
        reference.set_language(job.options.language.clone());
        reference.set_flags(payload.buffer.flags());
        reference.set_pre_context(prefix);
        reference.set_post_context(suffix);
        let mut at = origin;
        for scalar in run.chars() {
            reference.push(u32::from(scalar), u32::try_from(at).unwrap());
            at += scalar.len_utf16();
        }
        harfrust::shape(
            &ShaperFont::new(font.font()).with_scale(1024),
            &mut reference,
            ShapeOptions::new().point_size(Some(16.0)),
        )
        .unwrap();
        job.shape(&source_authority, &font_authority).unwrap();
        assert_eq!(job.profile().unwrap().glyph_count, reference.len());
        for index in 0..reference.len() {
            let glyph = job
                .glyph(&source_authority, &font_authority, index)
                .unwrap()
                .unwrap();
            let (info, position) = reference.paged_glyph_record(index).unwrap();
            assert_eq!(glyph.glyph_id, info.glyph_id);
            assert_eq!(glyph.source_utf16_cluster, info.cluster as usize);
            assert_eq!(
                glyph.flags,
                info.flags().to_bits() & harfrust::GlyphFlags::ALL.to_bits()
            );
            assert_eq!(glyph.x_advance, position.x_advance);
            assert_eq!(glyph.y_advance, position.y_advance);
            assert_eq!(glyph.x_offset, position.x_offset);
            assert_eq!(glyph.y_offset, position.y_offset);
        }
        assert!(
            job.glyph(&source_authority, &font_authority, reference.len())
                .unwrap()
                .is_none()
        );
        assert!(job.profile().unwrap().source_borrow_max_bytes <= 4096);
        assert_eq!(job.profile().unwrap().input_scalars, run.chars().count());
    }
}

#[test]
fn real_short_and_growing_shapes_keep_complete_glyphs_and_release_actual_storage() {
    // 同一公开Job逐小批输入：极短项、小工作区增长以及真实64KiB溢出都必须保留完整字形。
    // 只核原文本的实际结果与FD/映射归还，不读取分配器私态或断言内部增长次数。
    for units in [7, 600, 2400, 4000] {
        let text = "x".repeat(units);
        let (directory, document) = document(&text);
        let mapping = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let source_authority = source.authority();
        let font = font();
        let font_authority = font.authority();
        let mut job = MarkdownShapingJob::new(
            source.clone(),
            &source_authority,
            font.clone(),
            &font_authority,
            0..source.utf16_len(),
            options(*b"Latn", MarkdownShapingDirection::LeftToRight),
        )
        .unwrap();
        let observer = job.resource_observer();
        fill(&mut job, &source_authority, &font_authority, 4096, 256);
        job.shape(&source_authority, &font_authority).unwrap();
        assert_eq!(job.profile().unwrap().glyph_count, units);
        assert!(job.profile().unwrap().source_borrow_max_bytes <= 4096);
        let first = job
            .glyph(&source_authority, &font_authority, 0)
            .unwrap()
            .unwrap();
        assert!(first.glyph_id != 0 && first.x_advance > 0);
        for index in 0..units {
            let actual = job
                .glyph(&source_authority, &font_authority, index)
                .unwrap()
                .unwrap();
            assert_eq!(actual.source_utf16_cluster, index);
            assert_eq!(actual.glyph_id, first.glyph_id);
            assert_eq!(actual.x_advance, first.x_advance);
        }
        assert!(
            job.glyph(&source_authority, &font_authority, units)
                .unwrap()
                .is_none()
        );
        if units <= 2400 {
            let active = observer.paging.snapshot();
            assert!(active.work_bytes > 0);
            assert_eq!((active.files_created, active.mappings_created), (0, 0));
        } else {
            assert!(observer.paging.snapshot().files_created > 0);
        } // 小项不能因首次块变小而提前落盘；超过原64KiB仍必须走原文件分页。
        assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("bounded-shaping.md")).unwrap(),
            text.as_bytes()
        );
        drop(job);
        let closed = observer.paging.snapshot();
        assert_eq!(
            (
                closed.work_bytes,
                closed.files_open,
                closed.mappings_open,
                closed.mapped_bytes
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(closed.files_created, closed.files_closed);
        assert_eq!(closed.mappings_created, closed.mappings_released);
        assert!(!closed.cleanup_failed && closed.first_error.is_none());
        drop((source, font, document));
        assert!(observer.resources_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn invalid_foreign_authority_poisoning_closes_real_arrays_and_never_recovers() {
    let (_directory, document) = document("office a\u{301}🙂");
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_authority = source.authority();
    let other = MarkdownLayoutTextSource::capture(&document);
    let font = font();
    let font_authority = font.authority();
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &source_authority,
        font.clone(),
        &font_authority,
        0..source.utf16_len(),
        options(*b"Latn", MarkdownShapingDirection::LeftToRight),
    )
    .unwrap();
    let observer = job.resource_observer();
    fill(&mut job, &source_authority, &font_authority, 3, 2);
    let active = observer.paging.snapshot();
    assert!(active.work_bytes > 0);
    assert_eq!((active.files_open, active.mappings_open), (0, 0)); // 短项真实只拥有固定工作块。
    assert!(matches!(
        job.shape(&other.authority(), &font_authority),
        Err(MarkdownShapingError::SourceMismatch)
    ));
    assert_eq!(job.status(), MarkdownShapingStatus::Poisoned);
    let snapshot = observer.paging.snapshot();
    assert_eq!(snapshot.work_bytes, 0);
    assert_eq!(snapshot.files_open, 0);
    assert_eq!(snapshot.mappings_open, 0);
    assert_eq!(snapshot.mapped_bytes, 0);
    assert!(snapshot.first_error.is_none());
    assert!(!snapshot.cleanup_failed);
    assert!(matches!(
        job.shape(&source_authority, &font_authority),
        Err(MarkdownShapingError::Poisoned)
    ));
    job.cancel();
    assert_eq!(job.status(), MarkdownShapingStatus::Poisoned);
    drop(source);
    drop(font);
    assert!(observer.resources_released()); // other是另一payload，不保活原source。
}

#[test]
fn cancelled_and_empty_runs_have_distinct_terminal_states_and_actual_resource_receipts() {
    for text in ["", "a\u{301}🙂text"] {
        let (_directory, document) = document(text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let source_authority = source.authority();
        let font = font();
        let font_authority = font.authority();
        let mut job = MarkdownShapingJob::new(
            source.clone(),
            &source_authority,
            font.clone(),
            &font_authority,
            0..source.utf16_len(),
            options(*b"Latn", MarkdownShapingDirection::LeftToRight),
        )
        .unwrap();
        let observer = job.resource_observer();
        if text.is_empty() {
            assert_eq!(
                job.shape(&source_authority, &font_authority).unwrap(),
                MarkdownShapingStatus::Ready
            );
            assert_eq!(job.profile().unwrap().glyph_count, 0);
            assert_eq!(job.profile().unwrap().source_bytes_read, 0);
            assert!(
                job.glyph(&source_authority, &font_authority, 0)
                    .unwrap()
                    .is_none()
            );
        } else {
            job.step(
                &source_authority,
                &font_authority,
                MarkdownShapingBudget {
                    bytes: 2,
                    scalars: 1,
                },
            )
            .unwrap();
        }
        drop(source);
        drop(font);
        job.cancel();
        assert_eq!(job.status(), MarkdownShapingStatus::Cancelled);
        assert!(matches!(
            job.profile(),
            Err(MarkdownShapingError::Cancelled)
        ));
        assert!(matches!(
            job.shape(&source_authority, &font_authority),
            Err(MarkdownShapingError::Cancelled)
        ));
        assert!(observer.resources_released());
    }
}

#[test]
fn edits_after_capture_cannot_replace_the_run_or_refresh_its_original_authority() {
    let (_directory, mut document) = document("office🙂");
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_authority = source.authority();
    let font = font();
    let font_authority = font.authority();
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &source_authority,
        font.clone(),
        &font_authority,
        0..source.utf16_len(),
        options(*b"Latn", MarkdownShapingDirection::LeftToRight),
    )
    .unwrap();
    document
        .replace_utf16_range(
            0..document.utf16_len(),
            "different",
            crate::MarkdownTextSelection::caret(9),
        )
        .unwrap();
    assert_ne!(document.generation(), source.generation());
    fill(&mut job, &source_authority, &font_authority, 1, 1);
    job.shape(&source_authority, &font_authority).unwrap();
    let profile = job.profile().unwrap();
    assert_eq!(profile.source_generation, source.generation());
    assert_eq!(profile.input_scalars, "office🙂".chars().count());
    assert_eq!(job.source_range(), 0.."office🙂".encode_utf16().count());
    let latest = MarkdownLayoutTextSource::capture(&document);
    assert!(matches!(
        job.glyph(&latest.authority(), &font_authority, 0),
        Err(MarkdownShapingError::SourceMismatch)
    ));
}

#[test]
fn foreign_font_and_invalid_budget_never_fall_back_or_publish_an_input_prefix() {
    let (_directory, document) = document("🙂ffi");
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_authority = source.authority();
    let original_font = font();
    let authority = original_font.authority();
    let other_font = font();
    assert!(matches!(
        MarkdownShapingJob::new(
            source.clone(),
            &source_authority,
            original_font.clone(),
            &other_font.authority(),
            0..source.utf16_len(),
            options(*b"Latn", MarkdownShapingDirection::LeftToRight),
        ),
        Err(MarkdownShapingError::FontMismatch)
    ));
    // 一个低代理项范围不能因起点“很接近”而被修正为整标量。
    assert!(
        MarkdownShapingJob::new(
            source.clone(),
            &source_authority,
            original_font.clone(),
            &authority,
            1..2,
            options(*b"Latn", MarkdownShapingDirection::LeftToRight),
        )
        .is_err()
    );
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &source_authority,
        original_font.clone(),
        &authority,
        0..source.utf16_len(),
        options(*b"Latn", MarkdownShapingDirection::LeftToRight),
    )
    .unwrap();
    assert!(matches!(
        job.step(
            &source_authority,
            &authority,
            MarkdownShapingBudget {
                bytes: 0,
                scalars: 1
            }
        ),
        Err(MarkdownShapingError::InvalidOptions)
    ));
    assert_eq!(job.status(), MarkdownShapingStatus::Poisoned);
    assert!(matches!(
        job.shape(&source_authority, &authority),
        Err(MarkdownShapingError::Poisoned)
    ));
}

#[test]
fn deep_shape_cancellation_joins_worker_and_releases_original_source_and_paging() {
    use harfrust::{CooperativeCheckpointKind as Kind, CooperativePhase as Phase};
    use std::sync::{
        Barrier,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let (owner_tx, owner_rx) = mpsc::sync_channel(1);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker = std::thread::spawn(move || {
        // 所有不可Send的job/Font分页工作owner均在worker内构造，不做unsafe线程声明。
        let text = "office a\u{301}".repeat(2048);
        let (_directory, document) = document(&text);
        let backing = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let source_authority = source.authority();
        let font = font();
        let font_authority = font.authority();
        let mut job = MarkdownShapingJob::new(
            source.clone(),
            &source_authority,
            font.clone(),
            &font_authority,
            0..source.utf16_len(),
            options(*b"Latn", MarkdownShapingDirection::LeftToRight),
        )
        .unwrap();
        fill(&mut job, &source_authority, &font_authority, 4096, 1024);
        let observer = job.resource_observer();
        owner_tx
            .send((job.cancellation(), observer.clone(), backing))
            .unwrap();
        let entered = AtomicBool::new(false);
        job.cancellation_hook = Some(Arc::new(move |point| {
            if point.phase == Phase::Normalize
                && point.kind == Kind::Loop
                && !entered.swap(true, Ordering::AcqRel)
            {
                entered_tx.send(point).unwrap();
                worker_barrier.wait(); // 真实Normalize循环已进入后，另一线程才请求同原token。
            }
        }));
        assert!(matches!(
            job.shape(&source_authority, &font_authority),
            Err(MarkdownShapingError::Cancelled)
        ));
        assert_eq!(job.status(), MarkdownShapingStatus::Cancelled);
        assert!(job.payload.is_none());
        assert!(matches!(
            job.profile(),
            Err(MarkdownShapingError::Cancelled)
        ));
        drop(job);
        drop(source);
        drop(font);
        drop(document);
    });
    let (cancel, observer, backing) = owner_rx.recv().unwrap();
    assert_eq!(entered_rx.recv().unwrap().phase, Phase::Normalize);
    cancel.request();
    barrier.wait();
    worker.join().unwrap();
    assert!(observer.resources_released());
    let facts = observer.snapshot();
    assert_eq!(
        (facts.files_open, facts.mappings_open, facts.mapped_bytes),
        (0, 0, 0)
    );
    assert_eq!(facts.files_created, facts.files_closed);
    assert_eq!(facts.mappings_created, facts.mappings_released);
    assert!(facts.files_created > 0 && facts.mappings_created > 0);
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn actual_source_reclaim_failure_is_not_hidden_by_cancellation_request() {
    let (_directory, document) = document("office a\u{301}🙂");
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_authority = source.authority();
    let font = font();
    let font_authority = font.authority();
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &source_authority,
        font.clone(),
        &font_authority,
        0..source.utf16_len(),
        options(*b"Latn", MarkdownShapingDirection::LeftToRight),
    )
    .unwrap();
    fill(&mut job, &source_authority, &font_authority, 4096, 1024);
    // 先真实触达同原窗口，再于真实msync完成后注入永久回收错误，不能只手改状态位。
    source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    document.fail_next_original_reclaim_for_test();
    assert!(document.reclaim_original_read_windows_for_test().is_err());
    job.cancellation().request();
    assert!(matches!(
        job.shape(&source_authority, &font_authority),
        Err(MarkdownShapingError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        ))
    ));
    assert_eq!(job.status(), MarkdownShapingStatus::Poisoned);
    assert!(job.payload.is_none());
}

#[test]
fn cancellation_of_a_previously_ready_job_revokes_cached_glyphs_without_recreating_output() {
    let (_directory, document) = document("office");
    let source = MarkdownLayoutTextSource::capture(&document);
    let source_authority = source.authority();
    let font = font();
    let font_authority = font.authority();
    let mut job = MarkdownShapingJob::new(
        source.clone(),
        &source_authority,
        font.clone(),
        &font_authority,
        0..source.utf16_len(),
        options(*b"Latn", MarkdownShapingDirection::LeftToRight),
    )
    .unwrap();
    fill(&mut job, &source_authority, &font_authority, 4096, 1024);
    job.shape(&source_authority, &font_authority).unwrap();
    assert!(
        job.glyph(&source_authority, &font_authority, 0)
            .unwrap()
            .is_some()
    );
    let resources = job.resource_observer();
    let output = job.index_identity();
    job.cancellation().request();
    assert!(matches!(
        job.glyph(&source_authority, &font_authority, 0),
        Err(MarkdownShapingError::Cancelled)
    ));
    assert_eq!(job.status(), MarkdownShapingStatus::Cancelled);
    assert!(job.payload.is_none());
    assert!(std::sync::Weak::ptr_eq(&output, &job.index_identity())); // 失败不能靠换输出nonce续签。
    assert!(matches!(
        job.shape(&source_authority, &font_authority),
        Err(MarkdownShapingError::Cancelled)
    ));
    let facts = resources.snapshot();
    assert_eq!(
        (facts.files_open, facts.mappings_open, facts.mapped_bytes),
        (0, 0, 0)
    );
}
