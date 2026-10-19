//! 请求边界使用真实Font、Source和worker；不以复制构造器条件的纯函数代替生产入口。
use super::*;

fn create(
    fixture: &Fixture,
    font: &NativeMarkdownPagedFont,
    request: NativeMarkdownFontFallbackRequest,
    mtm: MainThreadMarker,
) -> Result<NativeMarkdownFontFallback, NativeMarkdownFontFallbackError> {
    NativeMarkdownFontFallback::new(
        fixture.source.clone(),
        &fixture.authority,
        font,
        &font.core_font().authority(),
        request,
        mtm,
    )
}

fn run_trial(
    fixture: &mut Fixture,
    font: &NativeMarkdownPagedFont,
    fallback: &mut NativeMarkdownFontFallback,
) -> omni_md_plugin::native_surface::paged_font_fallback::NativeMarkdownFontTrialResult {
    let NativeMarkdownFontFallbackStep::Trial(trial) = fallback
        .next_trial(
            font,
            &font.core_font().authority(),
            &mut fixture.graphemes,
            &mut fixture.scripts,
            &|| true,
        )
        .unwrap()
    else {
        panic!("原索引完成后必须交出真实Base试次")
    };
    std::thread::spawn(move || trial.run()).join().unwrap()
}

fn release_fixture(fixture: Fixture) {
    let source = fixture.source.resource_observer();
    let mapping = fixture.document.original_backing_observer().unwrap();
    drop(fixture);
    assert!(source.payload_released());
    assert!(mapping.unmap_succeeded() && !mapping.failed());
    assert_eq!(mapping.mapped_bytes(), 0);
}

fn release_font(font: NativeMarkdownPagedFont) {
    let bytes = font.bytes_observer();
    drop(font);
    assert!(bytes.mapping_drop_completed() && bytes.unmap_succeeded());
    assert_eq!(bytes.mapped_bytes(), 0);
    assert!(
        bytes.source_descriptor_closed() && bytes.backing_descriptor_closed() && !bytes.failed()
    );
}

fn fractional_sizes_keep_the_original_payload(mtm: MainThreadMarker) {
    for size in [14.3_f64, 32.1_f64] {
        let name = CFString::from_str("Helvetica");
        // SAFETY: 固定字体名、正有限原始字号和空矩阵；没有正文输入或外来指针。
        let native = unsafe { CTFont::with_name(&name, size, std::ptr::null()) };
        let font =
            NativeMarkdownPagedFont::capture(&native, MAX_FONT_BYTES, mtm, &|| true).unwrap();
        let mut fixture = Fixture::new("a");
        fixture.prepare();
        let mut actual = request(0..1, *b"Latn", MarkdownShapingDirection::LeftToRight, 1);
        actual.options.points = size as f32;
        assert_ne!(f64::from(actual.options.points), size);
        assert!(font.accepts_shaping_size(actual.options.points));

        // 相邻f32仍不是原Font编码；修复合法小数不能变成容差比较或放过foreign payload。
        let mut adjacent = actual.clone();
        adjacent.options.points = f32::from_bits(actual.options.points.to_bits() + 1);
        assert!(matches!(
            create(&fixture, &font, adjacent, mtm),
            Err(NativeMarkdownFontFallbackError::InvalidCluster)
        ));
        let foreign = base(mtm);
        assert!(matches!(
            NativeMarkdownFontFallback::new(
                fixture.source.clone(),
                &fixture.authority,
                &font,
                &foreign.core_font().authority(),
                actual.clone(),
                mtm,
            ),
            Err(NativeMarkdownFontFallbackError::FontMismatch)
        ));
        release_font(foreign);

        let mut fallback = create(&fixture, &font, actual, mtm).unwrap();
        let result = run_trial(&mut fixture, &font, &mut fallback);
        stores_released(&result);
        let NativeMarkdownFontFallbackCompletion::Selected(choice) = fallback
            .complete_trial(
                &font,
                &font.core_font().authority(),
                &mut fixture.graphemes,
                &mut fixture.scripts,
                result,
                &|| true,
            )
            .unwrap()
        else {
            panic!("Helvetica原字号的真实a试次应选择Base")
        };
        assert_eq!(choice.origin(), NativeMarkdownFontOrigin::Base);
        assert_eq!(choice.facts().missing_glyphs, 0);
        assert!(std::ptr::eq(
            choice.font(&font, &fixture.source).unwrap(),
            &font
        ));
        drop(choice);
        drop(fallback);
        release_fixture(fixture);
        release_font(font);
        println!(
            "paged_font_fallback fractional_size={size} actual_worker_selected=true adjacent_size_rejected=true foreign_payload_rejected=true resources_released=true"
        );
    }
}

fn language_allocation_boundary_precedes_packet_clone(mtm: MainThreadMarker) {
    let font = base(mtm);
    let mut fixture = Fixture::new("a");
    fixture.prepare();
    for (language, oversized, valid) in [
        ("a".repeat(64), false, true),
        ("a".repeat(65), true, false),
        ("é".repeat(32), false, false),
        ("é".repeat(33), true, false),
        (String::new(), false, false),
        ("en!".to_owned(), false, false),
    ] {
        let bytes = language.len();
        let mut requested = request(0..1, *b"Latn", MarkdownShapingDirection::LeftToRight, 1);
        requested.options.language = Some(language);
        let fallback = create(&fixture, &font, requested, mtm);
        if oversized {
            // 构造本身失败，因而不存在后继issue/options.clone；按UTF8字节而不是标量数量计费。
            assert!(matches!(
                fallback,
                Err(NativeMarkdownFontFallbackError::Shaping(
                    MarkdownShapingError::InvalidOptions
                ))
            ));
            println!(
                "paged_font_fallback language_bytes={bytes} constructor_rejected=true trial_issued=false"
            );
            continue;
        }
        // 有限标签仍走真正Core校验；Native不截断、不改默认值，也不复制整套语言语法。
        let mut fallback = fallback.unwrap();
        let result = run_trial(&mut fixture, &font, &mut fallback);
        if valid {
            stores_released(&result);
        } else {
            assert!(matches!(
                result.outcome(),
                Err(NativeMarkdownFontFallbackError::Shaping(
                    MarkdownShapingError::InvalidOptions
                ))
            ));
            assert!(result.resources().is_none()); // 原Core构造失败，没有假造“已释放”零收据。
        }
        let completion = fallback.complete_trial(
            &font,
            &font.core_font().authority(),
            &mut fixture.graphemes,
            &mut fixture.scripts,
            result,
            &|| true,
        );
        if valid {
            assert!(matches!(
                completion,
                Ok(NativeMarkdownFontFallbackCompletion::Selected(_))
            ));
        } else {
            assert!(matches!(
                completion,
                Err(NativeMarkdownFontFallbackError::Shaping(
                    MarkdownShapingError::InvalidOptions
                ))
            ));
        }
        drop(fallback);
        println!(
            "paged_font_fallback language_bytes={bytes} constructor_rejected=false core_valid={valid} actual_worker_checked=true"
        );
    }
    release_fixture(fixture);
    release_font(font);
}

fn original_nonzero_scalar_endpoints_remain_required(mtm: MainThreadMarker) {
    let font = base(mtm);
    let mut fixture = Fixture::new("a😀b");
    fixture.prepare();
    for range in [2..3, 1..2] {
        // 分别切入低代理项起点和终点；健康检查变0不能替构造器跳过实际端点解码。
        assert!(matches!(
            create(
                &fixture,
                &font,
                request(range, *b"Latn", MarkdownShapingDirection::LeftToRight, 4),
                mtm
            ),
            Err(NativeMarkdownFontFallbackError::Source(
                omni_markdown_core::MarkdownDocumentBufferError::NotUnicodeBoundary
            ))
        ));
    }
    let mut fallback = create(
        &fixture,
        &font,
        request(3..4, *b"Latn", MarkdownShapingDirection::LeftToRight, 4),
        mtm,
    )
    .unwrap();
    let result = run_trial(&mut fixture, &font, &mut fallback);
    stores_released(&result);
    let NativeMarkdownFontFallbackCompletion::Selected(choice) = fallback
        .complete_trial(
            &font,
            &font.core_font().authority(),
            &mut fixture.graphemes,
            &mut fixture.scripts,
            result,
            &|| true,
        )
        .unwrap()
    else {
        panic!("代理项之后的真实b必须由原Base试次选出")
    };
    assert_eq!(choice.source_range(), 3..4);
    assert_eq!(choice.facts().input_scalars, 1);
    assert_eq!(choice.facts().missing_glyphs, 0);
    drop(choice);
    fallback.close();
    release_fixture(fixture);
    release_font(font);
}

fn pending_remote_fixture() -> Fixture {
    use std::io::Write;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("远端待决字素.md");
    let mut file = std::fs::File::create(&path).unwrap();
    let block = [b'a'; 4096];
    for _ in 0..64 {
        file.write_all(&block).unwrap(); // 固定4KiB流式生成256KiB，不用整篇String构造原文。
    }
    // 真实LF让GCB保持原流式待决；不能让Printable Dense直接Ready绕开被测等待分支。
    file.write_all(b"\n").unwrap();
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let graphemes = MarkdownGraphemeJob::new(source.clone()).unwrap();
    let scripts = MarkdownScriptJob::new(
        source.clone(),
        &authority,
        MarkdownScriptCancellation::new(),
    )
    .unwrap();
    assert!(!graphemes.is_ready());
    Fixture {
        _directory: directory,
        document,
        source,
        authority,
        graphemes,
        scripts,
    }
}

fn pending_guard_does_not_reborrow_the_validated_remote_endpoint(mtm: MainThreadMarker) {
    let font = base(mtm);
    let mut fixture = pending_remote_fixture();
    let mapping = fixture.document.original_backing_observer().unwrap();
    let mut fallback = create(
        &fixture,
        &font,
        request(
            192 * 1024..192 * 1024 + 1,
            *b"Latn",
            MarkdownShapingDirection::LeftToRight,
            fixture.source.utf16_len(),
        ),
        mtm,
    )
    .unwrap();
    // 构造已经读取真实远端叶；随后填满首64KiB真实页目录，使旧guard重借远端必然驱逐页。
    for offset in (1..64 * 1024).step_by(4096) {
        assert_eq!(fixture.document.byte_offset_for_utf16(offset), Ok(offset));
    }
    assert_eq!(mapping.managed_resident_bytes(), 64 * 1024);
    let before = (mapping.reclaim_attempts(), mapping.reclaim_successes());
    let entered = std::cell::Cell::new(false);
    let current = || {
        entered.set(true);
        true
    };
    assert!(matches!(
        fallback.next_trial(
            &font,
            &font.core_font().authority(),
            &mut fixture.graphemes,
            &mut fixture.scripts,
            &current
        ),
        Ok(NativeMarkdownFontFallbackStep::PendingDependencies)
    ));
    assert!(entered.get());
    assert_eq!(
        (mapping.reclaim_attempts(), mapping.reclaim_successes()),
        before
    );
    assert_eq!(fixture.document.byte_offset_for_utf16(1), Ok(1));
    assert_eq!(
        (mapping.reclaim_attempts(), mapping.reclaim_successes()),
        before
    ); // 原最旧页仍实际命中；不只以返回Pending推断没有源页访问。
    fallback.close();
    release_fixture(fixture);
    release_font(font);
    println!(
        "paged_font_fallback pending_remote_endpoint_reborrow=false original_first_page_preserved=true actual_source_unmapped=true"
    );
}

#[cfg(feature = "native-source-reclaim-probe")]
fn original_reclaim_failure_precedes_current_cancellation(mtm: MainThreadMarker) {
    use omni_markdown_core::MarkdownDocumentBufferError;

    let font = base(mtm);
    for before_current in [true, false] {
        let mut fixture = pending_remote_fixture();
        let source = fixture.source.resource_observer();
        let graphemes = fixture.graphemes.resource_observer();
        let scripts = fixture.scripts.resource_observer();
        let mapping = fixture.document.original_backing_observer().unwrap();
        let mut fallback = create(
            &fixture,
            &font,
            request(
                192 * 1024..192 * 1024 + 1,
                *b"Latn",
                MarkdownShapingDirection::LeftToRight,
                fixture.source.utf16_len(),
            ),
            mtm,
        )
        .unwrap();
        for offset in (1..64 * 1024).step_by(4096) {
            assert_eq!(fixture.document.byte_offset_for_utf16(offset), Ok(offset));
        }
        let before = (mapping.reclaim_attempts(), mapping.reclaim_successes());
        let document = &fixture.document;
        let fail_original_reclaim = || {
            mapping.fail_next_reclaim_for_test();
            // 不在guard里伪造错误；真实请求未驻留页，经原msync成功后的一次合成EIO撤权。
            assert_eq!(
                document.byte_offset_for_utf16(128 * 1024 + 1),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
        };
        if before_current {
            fail_original_reclaim();
        } // 原失败在入口已发生时，current回调不能再运行。
        let entered = std::cell::Cell::new(false);
        let current = || {
            entered.set(true);
            if !before_current {
                fail_original_reclaim();
            } // 回调内原源真实失权，返回false也不得把EIO降级为普通取消。
            false
        };
        assert!(matches!(
            fallback.next_trial(
                &font,
                &font.core_font().authority(),
                &mut fixture.graphemes,
                &mut fixture.scripts,
                &current
            ),
            Err(NativeMarkdownFontFallbackError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert_eq!(entered.get(), !before_current);
        assert_eq!(mapping.first_errno(), Some(libc::EIO));
        assert!(mapping.reclaim_attempts() > before.0 && mapping.reclaim_successes() > before.1);
        fallback.close();
        drop(fixture);
        assert!(source.payload_released());
        let graphemes = graphemes.snapshot();
        assert!(graphemes.metadata_storage_allocated && graphemes.metadata_owner_released);
        assert!(!graphemes.metadata_file_open && !graphemes.cleanup_failed);
        assert_eq!(graphemes.metadata_map_bytes, 0);
        assert!(graphemes.source_payload_released);
        let scripts = scripts.snapshot();
        assert!(!scripts.run_file_open && !scripts.stack_file_open && !scripts.cleanup_failed);
        assert_eq!(scripts.mapped_bytes, 0);
        assert!(scripts.source_payload_released);
        assert!(mapping.unmap_succeeded() && mapping.failed());
        assert_eq!(mapping.mapped_bytes(), 0);
        assert_eq!(mapping.managed_resident_bytes(), 0);
        println!(
            "paged_font_fallback source_eio_before_current={before_current} original_errno_preserved=true actual_reclaim_succeeded=true actual_source_unmapped=true metadata_files_closed=true kernel_eio_claim=false"
        );
    }
    release_font(font);
}

pub(super) fn verify(mtm: MainThreadMarker) {
    fractional_sizes_keep_the_original_payload(mtm);
    language_allocation_boundary_precedes_packet_clone(mtm);
    original_nonzero_scalar_endpoints_remain_required(mtm);
    pending_guard_does_not_reborrow_the_validated_remote_endpoint(mtm);
    #[cfg(feature = "native-source-reclaim-probe")]
    original_reclaim_failure_precedes_current_cancellation(mtm);
}
