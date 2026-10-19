//! 使用真实字体与独立有限HarfRust整形核对；测试Vec只容纳合成oracle，产品不复制正文。
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownLayoutFont, MarkdownLayoutFontAuthority,
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource, MarkdownLineBreakBudget,
    MarkdownShapedGlyph, MarkdownShapedSpool, MarkdownShapedSpoolBuilder, MarkdownShapedSpoolError,
    MarkdownShapedSpoolStatus, MarkdownShapingBudget, MarkdownShapingDirection,
    MarkdownShapingFeature, MarkdownShapingOptions, MarkdownSoftLineMeasureAnswer,
    MarkdownSoftLineMeasureBudget, MarkdownSoftLineMeasureCancellation,
    MarkdownSoftLineMeasureRequest,
};
const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));
struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    options: MarkdownShapingOptions,
    shape: MarkdownShapingJob,
    clusters: MarkdownGlyphClusterIndex,
    breaks: MarkdownLayoutBreakJob,
}
impl Fixture {
    fn new(text: &str, item: Range<usize>, direction: MarkdownShapingDirection) -> Self {
        let font = MarkdownLayoutFont::resolve(Arc::new(ROBOTO), "Roboto-Regular", &[]).unwrap();
        Self::with_font(text, item, direction, font, *b"Latn")
    }
    fn with_font(
        text: &str,
        item: Range<usize>,
        direction: MarkdownShapingDirection,
        font: MarkdownLayoutFont,
        script: [u8; 4],
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("实际软行子范围.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, text.len() as u64 + 65536).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let font_authority = font.authority();
        let options = MarkdownShapingOptions {
            direction,
            script,
            language: Some("en".into()),
            points: 16.0,
            beginning_of_text: item.start == 0
                || text.encode_utf16().nth(item.start - 1) == Some(10),
            end_of_text: item.end == source.utf16_len(),
            features: vec![MarkdownShapingFeature {
                tag: *b"liga",
                value: 1,
                source_utf16_start: item.start as _,
                source_utf16_end: item.end as _,
            }],
        };
        let mut shape = MarkdownShapingJob::new(
            source.clone(),
            &authority,
            font.clone(),
            &font_authority,
            item,
            options.clone(),
        )
        .unwrap();
        fill(&mut shape, &authority, &font_authority);
        let mut clusters =
            MarkdownGlyphClusterIndex::new(&mut shape, &authority, &font_authority).unwrap();
        while clusters.status() != MarkdownGlyphClusterStatus::Ready {
            clusters.step(&mut shape, 11).unwrap();
        }
        let mut breaks = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
        while !breaks
            .step(&authority, MarkdownLineBreakBudget::default())
            .unwrap()
            .ready
        {}
        Self {
            _directory: directory,
            document,
            source,
            authority,
            font,
            font_authority,
            options,
            shape,
            clusters,
            breaks,
        }
    }
    fn measure(&mut self, start: usize, width: i64) -> MarkdownSoftLineMeasureJob {
        let mut job = MarkdownSoftLineMeasureJob::new(
            self.source.clone(),
            &self.authority,
            &self.font_authority,
            &mut self.shape,
            &mut self.breaks,
            MarkdownSoftLineMeasureRequest {
                start_utf16: start,
                width_26_6: width,
                cancellation: MarkdownSoftLineMeasureCancellation::new(),
            },
        )
        .unwrap();
        loop {
            match job
                .step(
                    &mut self.shape,
                    &mut self.clusters,
                    &mut self.breaks,
                    MarkdownSoftLineMeasureBudget::default(),
                )
                .unwrap()
                .answer
            {
                MarkdownSoftLineMeasureAnswer::Selected(_) => break,
                MarkdownSoftLineMeasureAnswer::Yielded => {}
                MarkdownSoftLineMeasureAnswer::PendingDependencies => panic!("真实依赖已完成"),
            }
        }
        job
    }
}
fn fill(
    shape: &mut MarkdownShapingJob,
    source: &MarkdownLayoutTextAuthority,
    font: &MarkdownLayoutFontAuthority,
) {
    while shape.status() == MarkdownShapingStatus::Ingesting {
        shape
            .step(
                source,
                font,
                MarkdownShapingBudget {
                    bytes: 31,
                    scalars: 7,
                },
            )
            .unwrap();
    }
    shape.shape(source, font).unwrap();
}
fn byte_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, scalar) in text.char_indices() {
        if units == utf16 {
            return byte;
        }
        units += scalar.len_utf16();
    }
    assert_eq!(units, utf16);
    text.len()
}

/// 独立完整有限子串oracle：直接公开HarfRust输入与原全局context/feature，不调用Core派生helper。
fn oracle(fixture: &Fixture, text: &str, range: Range<usize>) -> Vec<MarkdownShapedGlyph> {
    let mut buffer = harfrust::Buffer::new();
    let direction = match fixture.options.direction {
        MarkdownShapingDirection::LeftToRight => harfrust::Direction::LeftToRight,
        MarkdownShapingDirection::RightToLeft => harfrust::Direction::RightToLeft,
        _ => panic!("本控制只验证水平run"),
    };
    buffer.set_direction(direction);
    buffer.set_script(harfrust::Script::from_iso15924_tag(
        harfrust::Tag::from_be_bytes(fixture.options.script),
    ));
    buffer.set_language(harfrust::Language::new(
        fixture.options.language.as_ref().unwrap().as_bytes(),
    ));
    buffer.set_cluster_level(harfrust::ClusterLevel::MonotoneGraphemes);
    let mut flags = harfrust::BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
    let item = fixture.shape.source_range();
    if fixture.options.beginning_of_text && range.start == item.start {
        flags |= harfrust::BufferFlags::BEGINNING_OF_TEXT;
    }
    if fixture.options.end_of_text && range.end == item.end {
        flags |= harfrust::BufferFlags::END_OF_TEXT;
    }
    buffer.set_flags(flags);
    let bytes = byte_offset(text, range.start)..byte_offset(text, range.end);
    buffer.set_pre_context(&text[..bytes.start]);
    buffer.set_post_context(&text[bytes.end..]);
    let mut units = range.start;
    for scalar in text[bytes].chars() {
        buffer.push(u32::from(scalar), units as _);
        units += scalar.len_utf16();
    }
    let features: Vec<_> = fixture
        .options
        .features
        .iter()
        .map(|feature| harfrust::Feature {
            tag: harfrust::Tag::from_be_bytes(feature.tag),
            value: feature.value,
            start: feature.source_utf16_start,
            end: feature.source_utf16_end,
        })
        .collect();
    harfrust::shape(
        &harfrust::ShaperFont::new(fixture.font.font()).with_scale(1024),
        &mut buffer,
        harfrust::ShapeOptions::new()
            .point_size(Some(16.0))
            .features(&features),
    )
    .unwrap();
    (0..buffer.len())
        .map(|index| {
            let (info, position) = buffer.paged_glyph_record(index).unwrap();
            MarkdownShapedGlyph {
                glyph_id: info.glyph_id,
                source_utf16_cluster: info.cluster as _,
                flags: info.flags().to_bits(),
                x_advance: position.x_advance,
                y_advance: position.y_advance,
                x_offset: position.x_offset,
                y_offset: position.y_offset,
            }
        })
        .collect() // 仅有限测试完整glyph oracle；产品没有Vec输出或全文借用接口。
}

fn pair(fixture: &mut Fixture, measure: &MarkdownSoftLineMeasureJob) -> MarkdownSafeGlyphSlices {
    MarkdownSafeGlyphSlices::from_measure(
        measure,
        &mut fixture.shape,
        &mut fixture.breaks,
        &mut fixture.clusters,
    )
    .unwrap()
}
fn spool(fixture: &mut Fixture, slice: MarkdownSafeGlyphSlice) -> MarkdownShapedSpool {
    let mut builder =
        MarkdownShapedSpoolBuilder::from_safe_slice(&mut fixture.shape, slice).unwrap();
    while builder.step(&mut fixture.shape, 7).unwrap().status
        != MarkdownShapedSpoolStatus::AwaitingSeal
    {}
    builder.seal(&mut fixture.shape).unwrap()
}
fn compare(fixture: &Fixture, mut spool: MarkdownShapedSpool, expected: &[MarkdownShapedGlyph]) {
    let observer = spool.resource_observer();
    let authority = spool.authority();
    assert_eq!(spool.glyph_count(), expected.len());
    for (index, glyph) in expected.iter().enumerate() {
        assert_eq!(
            spool
                .glyph(
                    &authority,
                    &fixture.authority,
                    &fixture.font_authority,
                    index
                )
                .unwrap(),
            Some(*glyph)
        );
    }
    assert_eq!(
        spool
            .glyph(
                &authority,
                &fixture.authority,
                &fixture.font_authority,
                expected.len()
            )
            .unwrap(),
        None
    );
    spool.close().unwrap();
    let actual = observer.snapshot();
    assert!(!actual.file_open && actual.close_succeeded && !actual.cleanup_failed);
}

#[test]
fn actual_ltr_and_rtl_nonzero_line_and_remainder_match_full_oracle() {
    let text = "前\n01234 56789 01234 56789 01234 56789";
    let end = text.encode_utf16().count();
    for direction in [
        MarkdownShapingDirection::LeftToRight,
        MarkdownShapingDirection::RightToLeft,
    ] {
        let mut fixture = Fixture::new(text, 2..end, direction);
        let all = oracle(&fixture, text, 2..end);
        let measure = fixture.measure(2, 5000);
        let (line, tail) = pair(&mut fixture, &measure).into_parts();
        let facts = line.facts();
        assert!(facts.source_utf16.end < end);
        let expected: Vec<_> = all
            .iter()
            .copied()
            .filter(|g| facts.source_utf16.contains(&g.source_utf16_cluster))
            .collect();
        assert_eq!(expected, all[facts.visual_glyphs.clone()]);
        assert_eq!(
            facts.x_advance_26_6,
            expected.iter().map(|g| i64::from(g.x_advance)).sum::<i64>()
        );
        assert_eq!(expected, oracle(&fixture, text, facts.source_utf16.clone()));
        let sealed = spool(&mut fixture, line);
        compare(&fixture, sealed, &expected);
        let tail = tail.unwrap();
        let facts = tail.facts();
        let expected: Vec<_> = all
            .iter()
            .copied()
            .filter(|g| facts.source_utf16.contains(&g.source_utf16_cluster))
            .collect();
        let sealed = spool(&mut fixture, tail);
        compare(&fixture, sealed, &expected);
    }
}

fn same_independent_line_geometry(
    original: &[MarkdownShapedGlyph],
    reshaped: &[MarkdownShapedGlyph],
) {
    assert_eq!(original.len(), reshaped.len());
    let concat = harfrust::GlyphFlags::UNSAFE_TO_CONCAT.to_bits();
    for (old, new) in original.iter().zip(reshaped) {
        let mut old = *old;
        let mut new = *new;
        // 安全断点保证字形/位置可复用；UNSAFE_TO_CONCAT描述原片段外上下文，独立shape的
        // 首cluster可改变该位。交付spool仍逐字段保留完整原oracle（包括此位），绝不归零。
        old.flags &= !concat;
        new.flags &= !concat;
        assert_eq!(old, new);
    }
}

fn continuous(fixture: &mut Fixture, text: &str, width: i64) {
    let range = fixture.shape.source_range();
    let all = oracle(fixture, text, range.clone());
    let before = fixture.shape.profile().unwrap();
    let mut start = range.start;
    let mut copied = 0;
    let mut lines = 0;
    while start < range.end {
        let measure = fixture.measure(start, width);
        let (line, tail) = pair(fixture, &measure).into_parts();
        let facts = line.facts();
        assert_eq!(facts.source_utf16.start, start);
        assert!(facts.source_utf16.end > start);
        let expected = &all[facts.visual_glyphs.clone()];
        assert_eq!(
            facts.x_advance_26_6,
            expected.iter().map(|g| i64::from(g.x_advance)).sum::<i64>()
        );
        same_independent_line_geometry(
            expected,
            &oracle(fixture, text, facts.source_utf16.clone()),
        );
        let sealed = spool(fixture, line);
        compare(fixture, sealed, expected);
        copied += facts.visual_glyphs.len();
        start = facts.source_utf16.end;
        drop(tail); // 原余项只是一张固定元数据票；不把每行剩余glyph重写进新spool。
        lines += 1;
    }
    assert_eq!(copied, all.len());
    assert!(lines > 8);
    let after = fixture.shape.profile().unwrap();
    assert_eq!(before.input_scalars, after.input_scalars);
    assert_eq!(before.source_bytes_read, after.source_bytes_read);
    assert_eq!(before.glyph_count, after.glyph_count);
    assert!(after.source_borrow_max_bytes <= 4096);
    // 全过程仅原job；每条原glyph恰好写入一次。source_bytes_read是原shaper摄取统计，
    // 不冒称包含Source端点校验或索引IO，更不以此替代整产品峰值/CPU验收。
}
#[test]
fn many_ascii_lines_reuse_one_ready_job_and_copy_each_original_glyph_once() {
    let text = "01234 56789 ".repeat(128);
    let mut fixture = Fixture::new(&text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    continuous(&mut fixture, &text, 5000);
}
#[test]
#[cfg(target_os = "macos")]
fn real_cjk_font_many_lines_reuse_original_output_and_release_font_snapshot() {
    let bytes = crate::MarkdownLayoutFontBytes::capture_file(
        std::path::Path::new("/System/Library/Fonts/Hiragino Sans GB.ttc"),
        512 * 1024 * 1024,
    )
    .unwrap();
    let observer = bytes.resource_observer();
    let font = MarkdownLayoutFont::resolve(bytes.blob(), "HiraginoSansGB-W3", &[]).unwrap();
    let text = "中文原始字形安全换行验证。".repeat(32);
    let end = text.encode_utf16().count();
    let mut fixture = Fixture::with_font(
        &text,
        0..end,
        MarkdownShapingDirection::LeftToRight,
        font,
        *b"Hani",
    );
    continuous(&mut fixture, &text, 6000);
    drop(fixture);
    drop(bytes);
    assert!(
        observer.mapping_drop_completed()
            && observer.unmap_succeeded()
            && observer.mapped_bytes() == 0
            && observer.backing_descriptor_closed()
            && observer.source_descriptor_closed()
            && !observer.failed()
    );
}

#[test]
fn unsafe_and_inside_cluster_still_require_real_resolution() {
    for (text, width) in [("A\u{ad}V", 900), ("f\u{200b}fi", 650)] {
        let mut fixture = Fixture::new(
            text,
            0..text.encode_utf16().count(),
            MarkdownShapingDirection::LeftToRight,
        );
        let measure = fixture.measure(0, width);
        assert!(matches!(
            MarkdownSafeGlyphSlices::from_measure(
                &measure,
                &mut fixture.shape,
                &mut fixture.breaks,
                &mut fixture.clusters
            ),
            Err(MarkdownSafeGlyphSliceError::NeedsResolution)
        ));
    }
}
#[test]
fn unready_original_index_and_low_surrogate_cannot_sign_safe_range() {
    let text = "01234 56789 😀";
    let mut fixture = Fixture::new(
        text,
        0..text.encode_utf16().count(),
        MarkdownShapingDirection::LeftToRight,
    );
    let measure = fixture.measure(0, 5000);
    let mut pending = MarkdownGlyphClusterIndex::new(
        &mut fixture.shape,
        &fixture.authority,
        &fixture.font_authority,
    )
    .unwrap();
    assert!(matches!(
        MarkdownSafeGlyphSlices::from_measure(
            &measure,
            &mut fixture.shape,
            &mut fixture.breaks,
            &mut pending
        ),
        Err(MarkdownSafeGlyphSliceError::PendingDependencies)
    ));
    let seed = measure.resolve_seed(&mut fixture.shape).unwrap();
    let low = text.encode_utf16().count() - 1;
    assert!(matches!(
        facts(
            &seed,
            &mut fixture.shape,
            &mut fixture.breaks,
            &mut fixture.clusters,
            low..low + 1
        ),
        Err(MarkdownSafeGlyphSliceError::NeedsResolution)
    ));
}
#[test]
fn same_bytes_different_job_and_different_source_never_replace_original_owner() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let (line, _) = pair(&mut fixture, &measure).into_parts();
    let mut same = MarkdownShapingJob::new(
        fixture.source.clone(),
        &fixture.authority,
        fixture.font.clone(),
        &fixture.font_authority,
        0..text.len(),
        fixture.options.clone(),
    )
    .unwrap();
    fill(&mut same, &fixture.authority, &fixture.font_authority);
    assert!(matches!(
        MarkdownShapedSpoolBuilder::from_safe_slice(&mut same, line),
        Err(MarkdownShapedSpoolError::SafeSlice(
            MarkdownSafeGlyphSliceError::OwnerMismatch
        ))
    ));
    let (line, _) = pair(&mut fixture, &measure).into_parts();
    let mut foreign = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    assert!(MarkdownShapedSpoolBuilder::from_safe_slice(&mut foreign.shape, line).is_err());
}
#[test]
fn an_unspilled_cluster_owner_authorizes_the_slice_until_its_real_close() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let (mut line, _) = pair(&mut fixture, &measure).into_parts();
    let observer = fixture.clusters.resource_observer();
    let ready = observer.snapshot();
    assert!(!ready.file_open && !ready.owner_closed && ready.buffer_bytes == 16 * 1024);
    line.require(&mut fixture.shape).unwrap(); // 同Ready索引仅有原buffer，不需要创建FD来保住切片资格。
    fixture.clusters.cancel();
    let closed = observer.snapshot();
    assert!(closed.owner_closed && !closed.file_open && closed.buffer_bytes == 0);
    assert!(matches!(
        line.require(&mut fixture.shape),
        Err(MarkdownSafeGlyphSliceError::Poisoned)
    )); // 原owner实际退出后，仍活的measure/job不能让旧切片重新通过。
}

#[test]
fn measure_drop_or_index_cancel_revoke_unsealed_slice_but_not_sealed_data() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let (line, tail) = pair(&mut fixture, &measure).into_parts();
    let mut sealed = spool(&mut fixture, line);
    drop(measure);
    assert!(matches!(
        MarkdownShapedSpoolBuilder::from_safe_slice(&mut fixture.shape, tail.unwrap()),
        Err(MarkdownShapedSpoolError::SafeSlice(
            MarkdownSafeGlyphSliceError::Poisoned
        ))
    ));
    let authority = sealed.authority();
    assert!(
        sealed
            .glyph(&authority, &fixture.authority, &fixture.font_authority, 0)
            .unwrap()
            .is_some()
    );
    sealed.close().unwrap();
    let measure = fixture.measure(0, 5000);
    let (line, _) = pair(&mut fixture, &measure).into_parts();
    fixture.clusters.cancel();
    assert!(matches!(
        MarkdownShapedSpoolBuilder::from_safe_slice(&mut fixture.shape, line),
        Err(MarkdownShapedSpoolError::SafeSlice(
            MarkdownSafeGlyphSliceError::Poisoned
        ))
    ));
}
#[test]
fn original_cancel_reaches_builder_and_already_sealed_spool() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let (line, tail) = pair(&mut fixture, &measure).into_parts();
    let mut sealed = spool(&mut fixture, line);
    let seed = measure.resolve_seed(&mut fixture.shape).unwrap();
    seed.cancellation.cancel();
    assert!(matches!(
        MarkdownShapedSpoolBuilder::from_safe_slice(&mut fixture.shape, tail.unwrap()),
        Err(MarkdownShapedSpoolError::SafeSlice(
            MarkdownSafeGlyphSliceError::Cancelled
        ))
    ));
    let authority = sealed.authority();
    assert!(matches!(
        sealed.glyph(&authority, &fixture.authority, &fixture.font_authority, 0),
        Err(MarkdownShapedSpoolError::Cancelled)
    ));
}
#[test]
fn real_source_error_precedes_cancel_and_all_payloads_really_release() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let source = fixture.source.resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let (line, _) = pair(&mut fixture, &measure).into_parts();
    let cancellation = measure
        .resolve_seed(&mut fixture.shape)
        .unwrap()
        .cancellation;
    fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    cancellation.cancel(); // 实际IO已失败之后再取消；原首errno仍必须胜过干净取消。
    assert!(matches!(
        MarkdownShapedSpoolBuilder::from_safe_slice(&mut fixture.shape, line),
        Err(MarkdownShapedSpoolError::SafeSlice(
            MarkdownSafeGlyphSliceError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            )
        ))
    ));
    drop(measure);
    drop(fixture);
    assert!(source.payload_released());
    assert_eq!(backing.mapped_bytes(), 0);
}

#[test]
fn failure_of_still_live_measure_revokes_issued_builder_and_really_closes_output() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let mut measure = fixture.measure(0, 5000);
    let (line, _) = pair(&mut fixture, &measure).into_parts();
    let mut builder =
        MarkdownShapedSpoolBuilder::from_safe_slice(&mut fixture.shape, line).unwrap();
    let observer = builder.resource_observer();
    assert_eq!(builder.step(&mut fixture.shape, 1).unwrap().glyphs, 1);
    assert!(
        measure
            .step(
                &mut fixture.shape,
                &mut fixture.clusters,
                &mut fixture.breaks,
                MarkdownSoftLineMeasureBudget {
                    break_queries: 0,
                    clusters: 1
                }
            )
            .is_err()
    );
    assert!(matches!(
        builder.step(&mut fixture.shape, 1),
        Err(MarkdownShapedSpoolError::SafeSlice(
            MarkdownSafeGlyphSliceError::Poisoned
        ))
    ));
    let resources = observer.snapshot();
    assert!(!resources.file_open && resources.close_succeeded && !resources.cleanup_failed);
}

#[test]
fn real_shape_cancel_revokes_partial_builder_before_any_new_batch_is_signed() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let (line, _) = pair(&mut fixture, &measure).into_parts();
    let mut builder =
        MarkdownShapedSpoolBuilder::from_safe_slice(&mut fixture.shape, line).unwrap();
    let observer = builder.resource_observer();
    builder.step(&mut fixture.shape, 1).unwrap();
    fixture.shape.cancellation().request();
    assert!(builder.step(&mut fixture.shape, 1).is_err());
    let resources = observer.snapshot();
    assert!(!resources.file_open && resources.close_succeeded && !resources.cleanup_failed);
}

#[test]
fn corrupt_metadata_cannot_exceed_actual_glyph_array_and_sealed_owner_releases_source() {
    let text = "01234 56789 01234";
    let mut fixture = Fixture::new(text, 0..text.len(), MarkdownShapingDirection::LeftToRight);
    let measure = fixture.measure(0, 5000);
    let seed = measure.resolve_seed(&mut fixture.shape).unwrap();
    let (line, _) = pair(&mut fixture, &measure).into_parts();
    let mut corrupt = line.facts();
    corrupt.visual_glyphs.end = fixture.shape.profile().unwrap().glyph_count + 1;
    assert!(matches!(
        MarkdownSafeGlyphSlice::make(&seed, &mut fixture.shape, &fixture.clusters, corrupt),
        Err(MarkdownSafeGlyphSliceError::InconsistentCoverage)
    ));
    drop(seed); // 工厂的瞬时同Source借用不参与下面最后真实owner释放判定。
    let source = fixture.source.resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let font = fixture.font.resource_observer();
    let source_authority = fixture.authority.clone();
    let font_authority = fixture.font_authority.clone();
    let mut sealed = spool(&mut fixture, line);
    drop(measure);
    drop(fixture);
    assert!(!source.payload_released()); // 真实readonly仍需要冻结源/字体；不提前签归还。
    let authority = sealed.authority();
    assert!(
        sealed
            .glyph(&authority, &source_authority, &font_authority, 0)
            .unwrap()
            .is_some()
    );
    sealed.close().unwrap();
    assert!(source.payload_released() && font.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

// 历史收据控制复用上方真实fixture；include保持同一测试命名空间，不复制fixture。
include!("spool_tests.rs");
