//! 用真实分项job与真实分页文件验证producer，绝不把Item目录当成已完成软行。
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownDocumentBufferError, MarkdownLineComposeBudget,
    MarkdownLineItemAnswer, MarkdownLineItemCoordinator, MarkdownLineRegisteredFont,
};
mod lazy_graphemes;
mod lines;
mod providers;
#[cfg(all(feature = "paged-shaping-frontier", target_os = "macos"))]
mod starts;
mod stored_prefix;

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
    catalog: MarkdownLineFontCatalog,
    font: MarkdownLineRegisteredFont,
}
impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("分页原值.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, (text.len() + 65536) as u64).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let catalog = MarkdownLineFontCatalog::new();
        let actual = crate::MarkdownLayoutFont::resolve(
            Arc::new(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
                ))
                .as_slice(),
            ),
            "Roboto-Regular",
            &[],
        )
        .unwrap();
        let font = catalog.register(&actual).unwrap();
        Self {
            _directory: directory,
            document,
            source,
            catalog,
            font,
        }
    }
    fn coordinator(&self) -> MarkdownLineItemCoordinator {
        MarkdownLineItemCoordinator::new(
            self.source.clone(),
            &self.source.authority(),
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap()
    }
    fn store(&self, cancellation: MarkdownLineComposeCancellation) -> GlyphStore {
        GlyphStore::new(
            self.source.clone(),
            &self.source.authority(),
            self.catalog.clone(),
            cancellation,
        )
        .unwrap()
    }
    fn next(&self, coordinator: &mut MarkdownLineItemCoordinator) -> Option<MarkdownLineItem> {
        self.next_with_style(
            coordinator,
            MarkdownLineComposeStyle {
                points: 16.0,
                language: Some("en-GB".to_owned()),
                features: vec![crate::MarkdownShapingFeature {
                    tag: *b"liga",
                    value: 1,
                    source_utf16_start: 0,
                    source_utf16_end: u32::MAX,
                }],
            },
        )
    }
    fn next_with_style(
        &self,
        coordinator: &mut MarkdownLineItemCoordinator,
        style: MarkdownLineComposeStyle,
    ) -> Option<MarkdownLineItem> {
        for _ in 0..20000 {
            match coordinator
                .step(MarkdownLineComposeBudget::default())
                .unwrap()
                .answer
            {
                MarkdownLineItemAnswer::Yielded => {}
                MarkdownLineItemAnswer::EndOfSource => return None,
                #[cfg(feature = "paged-shaping-frontier")]
                MarkdownLineItemAnswer::NeedsFrontier(reason) => {
                    panic!("旧整item对照不应启前沿: {reason:?}")
                }
                MarkdownLineItemAnswer::NeedsControl(control) => {
                    panic!("无控制夹具不应产生{control:?}")
                }
                MarkdownLineItemAnswer::ItemReady(_) => {
                    return Some(coordinator.take_item().unwrap());
                }
                MarkdownLineItemAnswer::NeedStyle(request) => {
                    let range = request.source_range();
                    coordinator
                        .provide_style(
                            request
                                .respond_registered(range, &self.font, style.clone())
                                .unwrap(),
                        )
                        .unwrap();
                }
            }
        }
        panic!("有限实际job未Ready");
    }
}
fn capture(store: &mut GlyphStore, item: &mut MarkdownLineItem) -> StoredItemProof {
    store.begin(item).unwrap();
    for _ in 0..20000 {
        let before = store.index.as_ref().unwrap().len();
        let result = store.step(item, 1).unwrap();
        assert!(
            store.index.as_ref().unwrap().len() - before <= 1,
            "目录/语言元数据也不能额外越过本次record配额"
        );
        if let Some(proof) = result {
            return proof;
        }
        assert!(store.observer().snapshot().mapped_bytes <= 16 * 1024);
    }
    panic!("有限producer未完成真实记录写入");
}

#[test]
fn a_live_writer_without_a_file_is_healthy_until_its_actual_close() {
    let fixture = Fixture::new("Office");
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let observer = store.observer();
    let initial = observer.snapshot();
    assert!(!initial.file_open && !initial.owner_closed && initial.buffer_bytes == 0);
    store.guard().unwrap();
    let Some(Rows::Writing(index)) = &mut store.index else {
        panic!("新store必须持有原writer");
    };
    index.close().unwrap(); // 原owner确实退出后，空FD状态不允许后续Store重新取得权限。
    assert!(observer.snapshot().owner_closed);
    assert!(matches!(store.guard(), Err(Error::Poisoned)));
}

#[test]
fn real_mixed_items_share_one_store_and_match_original_glyphs_clusters_after_job_drop() {
    let fixture = Fixture::new("Office Ελληνικά אבג abc");
    let mut coordinator = fixture.coordinator();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let observer = store.observer();
    let mut last = None;
    let mut count = 0;
    let mut rtl = false;
    while let Some(mut item) = fixture.next(&mut coordinator) {
        let expected_facts = item.facts().clone();
        let expected_style = item.shaping_style().clone();
        let mut expected = Vec::new(); // 仅有限小夹具oracle；产品没有逐项Vec或字形mirror。
        while let Some(glyph) = item.glyph(expected.len()).unwrap() {
            expected.push(glyph);
        }
        let proof = capture(&mut store, &mut item);
        let mut original_index =
            MarkdownGlyphClusterIndex::new(&mut item.job, &item.authority, &item.font).unwrap();
        while original_index.step(&mut item.job, 5).unwrap().status
            != MarkdownGlyphClusterStatus::Ready
        {}
        let actual = store.facts(&proof).unwrap();
        let directory = store.directory(&proof).unwrap();
        assert_eq!(actual.facts, expected_facts);
        assert_eq!(actual.glyphs, expected.len());
        assert!(
            fixture
                .catalog
                .require_registration(&actual.registration)
                .unwrap()
                .font()
                .permits(&item.font)
        );
        let mut offset = expected_facts.source_utf16.start;
        let mut x = 0;
        let mut y = 0;
        let mut clusters = 0;
        while offset < expected_facts.source_utf16.end {
            let original = match original_index.containing(&mut item.job, offset).unwrap() {
                MarkdownGlyphClusterAnswer::Known(Some(value)) => value,
                _ => panic!("实际cluster覆盖缺口"),
            };
            assert_eq!(
                store.cluster(&proof, offset).unwrap().as_ref(),
                Some(&original)
            );
            // 同步发布路径复用已验证目录后，cluster内部各UTF16坐标仍须与原Job索引完全相同。
            for point in original.source_utf16.clone() {
                assert_eq!(
                    store
                        .cluster_in_directory(&directory, point)
                        .unwrap()
                        .as_ref()
                        .map(|(_, cluster)| cluster),
                    Some(&original)
                );
            }
            offset = original.source_utf16.end;
            x += original.x_advance_26_6;
            y += original.y_advance_26_6;
            clusters += 1;
        }
        assert_eq!(actual.clusters, clusters);
        assert_eq!((actual.x_advance_26_6, actual.y_advance_26_6), (x, y));
        assert!(store.cluster(&proof, offset).unwrap().is_none());
        assert!(
            store
                .cluster_in_directory(&directory, offset)
                .unwrap()
                .is_none()
        );
        drop(original_index);
        drop(item); // 历史复用不能靠未释放的原Job或cluster索引维持权限。
        for (ordinal, glyph) in expected.iter().enumerate() {
            assert_eq!(store.glyph(&proof, ordinal).unwrap(), Some(*glyph));
        }
        assert!(store.glyph(&proof, expected.len()).unwrap().is_none());
        let style = store.style(&proof).unwrap();
        assert_eq!(style.language, expected_style.language);
        assert_eq!(style.features, expected_style.features);
        assert_eq!(style.points, expected_style.points);
        if let Some(previous) = last {
            assert_eq!(store.previous(&proof).unwrap().unwrap().key, previous);
        } else {
            assert!(store.previous(&proof).unwrap().is_none());
        }
        last = Some(proof.key);
        count += 1;
        rtl |= expected_facts.direction == MarkdownShapingDirection::RightToLeft;
    }
    assert!(rtl && count >= 4);
    assert_eq!(store.items, count);
    assert_eq!(store.source_end, fixture.source.utf16_len());
    assert!(observer.snapshot().mapped_bytes <= 16 * 1024);
    drop(store);
    let state = observer.snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
}

#[test]
fn cancellation_during_real_capture_closes_unique_file_without_issuing_an_origin() {
    let fixture = Fixture::new("Office Office Office");
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let cancellation = MarkdownLineComposeCancellation::new();
    let mut store = fixture.store(cancellation.clone());
    let observer = store.observer();
    store.begin(&mut item).unwrap();
    assert!(store.step(&mut item, 1).unwrap().is_none());
    cancellation.cancel();
    assert!(matches!(store.step(&mut item, 1), Err(Error::Cancelled)));
    assert_eq!(store.items, 0);
    let state = observer.snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
    assert!(matches!(store.step(&mut item, 1), Err(Error::Cancelled)));
}

#[test]
fn same_bytes_other_store_or_expired_font_never_resurrect_stored_origin() {
    let fixture = Fixture::new("Office");
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let proof = capture(&mut store, &mut item);
    let mut other = fixture.store(MarkdownLineComposeCancellation::new());
    assert!(matches!(other.facts(&proof), Err(Error::SourceMismatch)));
    let mut other_clusters = fixture.store(MarkdownLineComposeCancellation::new());
    assert!(matches!(
        other_clusters.cluster(&proof, 0),
        Err(Error::SourceMismatch)
    ));
    let other_state = other.observer().snapshot();
    assert!(!other_state.file_open && other_state.mapped_bytes == 0);
    let font_observer = fixture.font.font().resource_observer();
    drop(item);
    drop(coordinator);
    drop(fixture.font);
    assert!(font_observer.payload_released());
    assert!(matches!(store.glyph(&proof, 0), Err(Error::FontExpired)));
    let state = store.observer().snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
}

#[test]
fn source_io_failure_precedes_cached_origin_and_preserves_true_close_receipt() {
    let fixture = Fixture::new("Office");
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let proof = capture(&mut store, &mut item);
    let directory = store.directory(&proof).unwrap();
    assert!(store.glyph(&proof, 0).unwrap().is_some());
    fixture.document.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(
        matches!(store.cluster_in_directory(&directory,0),Err(Error::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno))) if errno==libc::EIO)
    ); // 同步保存的真实目录也不能把原Source错误转换为普通Poisoned或健康缓存命中。
    assert!(
        matches!(store.facts(&proof),Err(Error::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno))) if errno==libc::EIO)
    );
    let state = store.observer().snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
    assert!(
        matches!(store.glyph(&proof,0),Err(Error::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno))) if errno==libc::EIO)
    );
}

#[test]
fn verified_directory_queries_still_observe_cancellation_and_actual_font_expiry() {
    for expire_font in [false, true] {
        let fixture = Fixture::new("Office");
        let mut coordinator = fixture.coordinator();
        let mut item = fixture.next(&mut coordinator).unwrap();
        let cancellation = MarkdownLineComposeCancellation::new();
        let mut store = fixture.store(cancellation.clone());
        let proof = capture(&mut store, &mut item);
        let directory = store.directory(&proof).unwrap();
        assert!(store.cluster_in_directory(&directory, 0).unwrap().is_some());
        let observer = store.observer();
        let font_observer = fixture.font.font().resource_observer();
        drop((item, coordinator));
        if expire_font {
            drop(fixture.font); // 目录没有字体强owner，实际最后注册值消失后必须撤销旧查询资格。
            assert!(font_observer.payload_released());
            assert!(matches!(
                store.cluster_in_directory(&directory, 0),
                Err(Error::FontExpired)
            ));
        } else {
            cancellation.cancel();
            assert!(matches!(
                store.cluster_in_directory(&directory, 0),
                Err(Error::Cancelled)
            ));
        }
        let closed = observer.snapshot();
        assert!(
            closed.owner_closed
                && !closed.file_open
                && closed.buffer_bytes == 0
                && !closed.cleanup_failed
        );
    }
}

#[test]
fn verified_directory_does_not_hide_real_cluster_record_eof_or_preserve_partial_output() {
    let fixture = Fixture::new(&"Office ".repeat(110));
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    // 使用已有测试FD适配器写入真实同inode，随后截短文件；不增加产品错误注入或替代索引。
    let file = tempfile::NamedTempFile::new().unwrap();
    let index = PagedLayoutIndex::<64>::test_with_file(file.reopen().unwrap()).unwrap();
    store.observer = index.resource_observer();
    store.index = Some(Rows::Writing(index));
    let proof = capture(&mut store, &mut item);
    let directory = store.directory(&proof).unwrap();
    assert!(store.cluster_in_directory(&directory, 0).unwrap().is_some());
    let observer = store.observer();
    assert!(observer.snapshot().file_open);
    file.as_file().set_len(0).unwrap(); // 原cluster在较早已回写页；保留Directory不能取代其真实record读取。
    assert!(
        matches!(store.cluster_in_directory(&directory, 0), Err(Error::Storage(error))
        if error.kind() == std::io::ErrorKind::UnexpectedEof)
    );
    assert!(matches!(
        store.cluster_in_directory(&directory, 0),
        Err(Error::Poisoned)
    ));
    let closed = observer.snapshot();
    assert!(
        closed.owner_closed
            && !closed.file_open
            && closed.buffer_bytes == 0
            && !closed.cleanup_failed
    );
}

#[test]
fn sealed_store_moves_the_unique_file_and_batches_exact_original_glyphs_with_real_release() {
    let fixture = Fixture::new(&"Office ".repeat(110));
    let source_observer = fixture.source.resource_observer();
    let backing = fixture.source.original_backing_observer().unwrap();
    let font_observer = fixture.font.font().resource_observer();
    let mut coordinator = fixture.coordinator();
    let mut item = fixture.next(&mut coordinator).unwrap();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let proof = capture(&mut store, &mut item);
    let count = store.facts(&proof).unwrap().glyphs;
    assert!(count > 512);
    let mut expected = Vec::new();
    for ordinal in 0..count {
        expected.push(item.glyph(ordinal).unwrap().unwrap());
    }
    let observer = store.observer();
    store.seal_items().unwrap();
    let sealed = observer.snapshot();
    assert!(sealed.owner_closed); // Writer已实际移交原FD，下面的Reading仍必须允许真实读取。
    assert!(
        sealed.file_open
            && sealed.mapped_bytes == 0
            && !sealed.cleanup_failed
            && !sealed.reader_failed
    );
    drop(item);
    drop(coordinator); // 再读取的是原唯一sealed FD，不靠原job、item或临时cluster索引。
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
    let mut start = 0;
    while start < count {
        let end = (start + batch.len()).min(count);
        store
            .glyphs(&proof, start..end, &mut batch[..end - start])
            .unwrap();
        assert_eq!(&batch[..end - start], &expected[start..end]);
        start = end;
    }
    store.glyphs(&proof, count..count, &mut []).unwrap();
    store.close().unwrap();
    let closed = observer.snapshot();
    assert!(
        !closed.file_open
            && closed.mapped_bytes == 0
            && !closed.cleanup_failed
            && !closed.reader_failed
    );
    drop(fixture); // 所有真实Source/Font都归还后才核底层资源；observer没有保活强边。
    assert!(source_observer.payload_released());
    assert!(font_observer.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn metadata_uses_the_actual_record_budget_and_invalid_large_batch_permanently_rejects() {
    let fixture = Fixture::new("Office");
    let mut coordinator = fixture.coordinator();
    let style = MarkdownLineComposeStyle {
        points: 16.0,
        language: Some("x".repeat(64)),
        features: vec![
            crate::MarkdownShapingFeature {
                tag: *b"kern",
                value: 1,
                source_utf16_start: 0,
                source_utf16_end: u32::MAX
            };
            64
        ],
    };
    let mut item = fixture
        .next_with_style(&mut coordinator, style.clone())
        .unwrap();
    let mut store = fixture.store(MarkdownLineComposeCancellation::new());
    let proof = capture(&mut store, &mut item);
    let restored = store.style(&proof).unwrap();
    assert_eq!(restored.features, style.features);
    assert_eq!(restored.language, style.language);
    store.seal_items().unwrap();
    let blank = MarkdownShapedGlyph {
        glyph_id: u32::MAX,
        source_utf16_cluster: 0,
        flags: 0,
        x_advance: 0,
        y_advance: 0,
        x_offset: 0,
        y_offset: 0,
    };
    let mut invalid = [blank; 257];
    assert!(matches!(
        store.glyphs(&proof, 0..257, &mut invalid),
        Err(Error::InvalidRequest)
    ));
    assert!(invalid.iter().all(|value| *value == blank)); // 大预算先拒，不先读/写输出再截短请求。
    assert!(matches!(store.glyph(&proof, 0), Err(Error::Poisoned)));
    let state = store.observer().snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
}
