//! Exercise the real item producer and paged file; an item directory is not a committed soft line.
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
    // The private directory owns only synthetic source data; the document and captured
    // source retain their real backing; font owns the payload and catalog keeps weak tickets.
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
    catalog: MarkdownLineFontCatalog,
    font: MarkdownLineRegisteredFont,
}
impl Fixture {
    fn new(text: &str) -> Self {
        // Materialize the finite oracle as an actual UTF-8 file before capturing its
        // immutable source, so identical text in another fixture has another authority.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("分页原值.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, (text.len() + 65536) as u64).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let catalog = MarkdownLineFontCatalog::new();
        // Resolve genuine font bytes rather than manufacturing glyph metrics or a
        // registration; every later style response carries this catalog's real ticket.
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
        // Capture the original source authority once; the coordinator must not borrow
        // authority from a later document capture when its asynchronous work completes.
        MarkdownLineItemCoordinator::new(
            self.source.clone(),
            &self.source.authority(),
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap()
    }
    fn store(&self, cancellation: MarkdownLineComposeCancellation) -> GlyphStore {
        // The store receives the same source/catalog but owns its own unique paged
        // writer. Sharing source bytes does not make two stores interchangeable.
        GlyphStore::new(
            self.source.clone(),
            &self.source.authority(),
            self.catalog.clone(),
            cancellation,
        )
        .unwrap()
    }
    fn next(&self, coordinator: &mut MarkdownLineItemCoordinator) -> Option<MarkdownLineItem> {
        // The finite text oracle uses actual ligature shaping, language and point size;
        // callers that need another style use the same producer through next_with_style.
        self.next_with_style(
            coordinator,
            MarkdownLineComposeStyle {
                presentation: Default::default(),
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
        // A yielded quantum is not completion. Supply styles only in response to the
        // producer's current original request, then transfer the actual ready item.
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
                    // Respond to the requested coverage with the original registration;
                    // copying a font name or recapturing a source cannot authorize it.
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
    // Drive the real writer one record at a time; only its completed proof can be
    // used after the source job is dropped. The observer measures actual owned pages.
    store.begin(item).unwrap();
    for _ in 0..20000 {
        let result = store.step(item, 1).unwrap();
        if let Some(proof) = result {
            return proof;
        }
        assert!(store.observer().snapshot().mapped_bytes <= 16 * 1024);
    }
    panic!("有限producer未完成真实记录写入");
}

#[test]
fn real_mixed_items_share_one_store_and_match_original_glyphs_clusters_after_job_drop() {
    // Several scripts and both visual directions must survive persistence in one
    // store, while every expected glyph still comes from the actual original job.
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
        let mut expected = Vec::new(); // This finite test oracle is not a production glyph mirror.
        while let Some(glyph) = item.glyph(expected.len()).unwrap() {
            expected.push(glyph);
        }
        let proof = capture(&mut store, &mut item);
        // Build the comparison index from the original job, independently of the
        // store's serialized directory, before retiring both original owners.
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
            // Reusing a verified directory must preserve every UTF-16 coordinate
            // inside the original cluster, not merely its first scalar.
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
        drop(item); // Stored reads must remain valid after the original job and index are gone.
        for (ordinal, glyph) in expected.iter().enumerate() {
            assert_eq!(store.glyph(&proof, ordinal).unwrap(), Some(*glyph));
        }
        assert!(store.glyph(&proof, expected.len()).unwrap().is_none());
        // Persisted style and predecessor traversal are observable stored results;
        // neither may depend on a still-live shaping job.
        let style = store.style(&proof).unwrap();
        assert_eq!(style.presentation, expected_style.presentation);
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
    assert!(observer.snapshot().mapped_bytes <= 16 * 1024);
    // Retiring the writer must close its actual file and release its mapped pages,
    // rather than merely discarding an item-count claim.
    drop(store);
    let state = observer.snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
}

#[test]
fn cancellation_during_real_capture_closes_unique_file_without_issuing_an_origin() {
    // Cancel after the real producer has started writing; a previously yielded step
    // cannot become a proof or reopen the writer on a repeated call.
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
    let state = observer.snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
    assert!(matches!(store.step(&mut item, 1), Err(Error::Cancelled)));
}

#[test]
fn same_bytes_other_store_or_expired_font_never_resurrect_stored_origin() {
    // Original store identity and actual font lifetime are independent authorities.
    // Another writer with the same source may not consume this writer's proof.
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
    // Drop every strong font owner before checking expiry. The catalog and stored
    // registration are weak permissions and must not retain the payload themselves.
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
    // Fill the genuine stored directory first, then consume the existing reclaim
    // failure injection through the real source operation. This is not a kernel EIO.
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
    ); // A saved directory must not translate the original source cause into cache success or Poisoned.
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
    // Query the same verified directory after either revocation path; successful
    // earlier reads do not exempt a later cache hit from source/font/cancel guards.
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
            drop(fixture.font); // The directory cannot retain the final registered font owner.
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
        // Both failures retire the real writer and its buffer, not merely the
        // directory's ability to answer the next query.
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
fn sealed_store_moves_the_unique_file_and_batches_exact_original_glyphs_with_real_release() {
    // Persist enough glyphs to cross a reader batch, then retire the original job.
    // Sealing transfers the unique file rather than keeping the producer alive.
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
    // Only this finite oracle keeps expected glyphs; the product reads bounded
    // batches from the same sealed paged file.
    let mut expected = Vec::new();
    for ordinal in 0..count {
        expected.push(item.glyph(ordinal).unwrap().unwrap());
    }
    let observer = store.observer();
    store.seal_items().unwrap();
    let sealed = observer.snapshot();
    assert!(sealed.owner_closed); // Writer ownership ended, but the transferred reader owns the same FD.
    assert!(
        sealed.file_open
            && sealed.mapped_bytes == 0
            && !sealed.cleanup_failed
            && !sealed.reader_failed
    );
    drop(item);
    drop(coordinator); // Later reads use the sealed FD after the original job and item have gone.
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
    // Cover every persisted glyph with batches of at most 256, including the final
    // short batch and a legal empty range at EOF.
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
    drop(fixture); // Weak observers cannot keep the actual source, font or backing alive.
    assert!(source_observer.payload_released());
    assert!(font_observer.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn metadata_uses_the_actual_record_budget_and_invalid_large_batch_permanently_rejects() {
    // Use real style metadata large enough to require multiple writer quanta;
    // read it back through the stored style API before testing batch rejection.
    let fixture = Fixture::new("Office");
    let mut coordinator = fixture.coordinator();
    let style = MarkdownLineComposeStyle {
        presentation: crate::MarkdownPresentationStyles::UNDERLINE
            .union(crate::MarkdownPresentationStyles::STRIKETHROUGH),
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
    assert_eq!(restored.presentation, style.presentation);
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
    // The oversized external batch is rejected before any output slot is changed;
    // its failure also retires this store instead of silently truncating the range.
    let mut invalid = [blank; 257];
    assert!(matches!(
        store.glyphs(&proof, 0..257, &mut invalid),
        Err(Error::InvalidRequest)
    ));
    assert!(invalid.iter().all(|value| *value == blank)); // Reject before reading or changing caller output.
    assert!(matches!(store.glyph(&proof, 0), Err(Error::Poisoned)));
    let state = store.observer().snapshot();
    assert!(!state.file_open && state.mapped_bytes == 0 && !state.cleanup_failed);
}
