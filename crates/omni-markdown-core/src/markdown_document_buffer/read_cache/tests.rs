//! 合成有限输入验证真实读取窗口、UTF-16坐标、撤权、并发和最终资源归还。

use std::cell::Cell;

use tempfile::{TempDir, tempdir};

use super::*;

fn fixture(source: &str) -> (TempDir, MarkdownDocumentBuffer) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("read-window.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
    (directory, document)
}

/// 只统计真正进入原piece字节lease的次数，不以getter次数代替缓存效果。
struct CountedSource<'a> {
    document: &'a MarkdownDocumentBuffer,
    piece_reads: Cell<usize>,
}

impl PieceByteSource for CountedSource<'_> {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.document.original_backing_health()
    }

    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.piece_reads.set(self.piece_reads.get() + 1);
        self.document.piece_bytes(piece)
    }

    // 同一计量包装继续统计真实存储调用，局部range不改成一次完整叶读取。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.piece_reads.set(self.piece_reads.get() + 1);
        self.document.piece_range_bytes(piece, local)
    }
}

/// 最后一个原piece读取返回前真实驱逐，使cache填充阶段能观察到真实IO错误。
struct ReclaimAfterRead<'a>(&'a MarkdownDocumentBuffer);
impl PieceByteSource for ReclaimAfterRead<'_> {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.0.original_backing_health()
    }
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.0.piece_bytes(piece)
    }

    // 保留原文档真实lease；此入口不复制回调后的专用回收负控行为。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.0.piece_range_bytes(piece, local)
    }
    fn with_piece_bytes<R>(
        &self,
        piece: &MarkdownPiece,
        visit: impl FnOnce(&[u8]) -> Result<R, MarkdownDocumentBufferError>,
    ) -> Result<R, MarkdownDocumentBufferError> {
        let result = self.0.with_piece_bytes(piece, visit)?;
        self.0.fail_next_original_reclaim_for_test();
        self.0.reclaim_original_read_windows_for_test()?;
        Ok(result)
    }
}

#[test]
fn lazy_single_box_serves_unit_reads_and_evicts_instead_of_accumulating_windows() {
    let (_directory, document) = fixture(&"a".repeat(READ_WINDOW_UTF16_UNITS * 3));
    let source = CountedSource {
        document: &document,
        piece_reads: Cell::new(0),
    };
    let mut cache = Utf16ReadCache::default();
    assert!(cache.units.is_none());
    cache.copy(&source, &document.root, 0, 0, &mut []).unwrap();
    assert!(cache.units.is_none());
    assert_eq!(source.piece_reads.get(), 0);
    let strong_roots = Arc::strong_count(document.root.as_ref().unwrap());
    let mut address = None;
    for offset in 0..document.utf16_len() {
        // 连续单元请求必须复用同一个真实窗口；每个ASCII窗口正好只读一个4KiB叶。
        let mut unit = [0];
        cache
            .copy(&source, &document.root, 0, offset, &mut unit)
            .unwrap();
        assert_eq!(unit, [u16::from(b'a')]);
        let units = cache.units.as_ref().unwrap();
        let current = units.as_ptr().addr();
        assert_eq!(*address.get_or_insert(current), current);
        assert_eq!(std::mem::size_of_val(units.as_ref()), 8192);
        assert!(cache.valid.len() <= READ_WINDOW_UTF16_UNITS);
    }
    assert_eq!(source.piece_reads.get(), 3);
    assert_eq!(
        Arc::strong_count(document.root.as_ref().unwrap()),
        strong_roots
    );
    cache.copy(&source, &document.root, 0, 0, &mut [0]).unwrap();
    assert_eq!(source.piece_reads.get(), 4); // 旧窗口已经驱逐，不能暗中保留全部访问页。
}

#[test]
fn surrogate_halves_and_cross_window_copies_preserve_every_original_unit() {
    let text = format!("{}😀中{}😀尾", "a".repeat(4095), "b".repeat(4093));
    let (_directory, document) = fixture(&text);
    let snapshot = document.text_snapshot();
    let expected: Vec<u16> = text.encode_utf16().collect();
    for (offset, unit) in expected.iter().copied().enumerate() {
        // 第4095/4096单元刻意拆开代理对；缓存不得用标量边界偷改NSString坐标。
        assert_eq!(document.utf16_code_unit_at(offset), Ok(unit));
        assert_eq!(snapshot.utf16_code_unit_at(offset), Ok(unit));
    }
    for range in [
        4095..4097,
        4096..4100,
        4090..expected.len(),
        8190..expected.len(),
    ] {
        let mut actual = vec![0; range.len()];
        document
            .copy_utf16_code_units(range.start, &mut actual)
            .unwrap();
        assert_eq!(actual, expected[range.clone()]);
        snapshot
            .copy_utf16_code_units(range.start, &mut actual)
            .unwrap();
        assert_eq!(actual, expected[range]);
    }
}

#[test]
fn real_root_and_generation_both_revoke_hits_even_when_lengths_match() {
    let (_directory, mut document) = fixture("旧😀文");
    let old = document.text_snapshot();
    assert_eq!(document.utf16_code_unit_at(0), Ok(0x65e7));
    document
        .replace_utf16_range(0..1, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    assert_eq!(document.utf16_code_unit_at(0), Ok(0x65b0));
    document.undo_text_edit().unwrap();
    assert_eq!(document.utf16_code_unit_at(0), Ok(0x65e7));
    document.redo_text_edit().unwrap();
    assert_eq!(document.utf16_code_unit_at(0), Ok(0x65b0));
    assert_eq!(old.utf16_code_unit_at(0), Ok(0x65e7));

    let (_other_directory, other) = fixture("异😀文");
    let source = CountedSource {
        document: &other,
        piece_reads: Cell::new(0),
    };
    let mut cache = Utf16ReadCache::default();
    cache
        .copy(&document, &document.root, 7, 0, &mut [0])
        .unwrap();
    let mut actual = [0];
    // 同generation、同长度的另一root必须重读，不能只靠外部代次或长度命中。
    cache.copy(&source, &other.root, 7, 0, &mut actual).unwrap();
    assert_eq!(actual, [0x5f02]);
    assert_eq!(source.piece_reads.get(), 1);
    cache.copy(&source, &other.root, 8, 0, &mut actual).unwrap();
    assert_eq!(source.piece_reads.get(), 2);
}

#[test]
fn invalid_ranges_do_not_write_or_allocate_and_empty_eof_remains_valid() {
    let (_directory, document) = fixture("😀a");
    let mut cache = Utf16ReadCache::default();
    let mut output = [0xbeef; 2];
    for offset in [usize::MAX, 2] {
        assert_eq!(
            cache.copy(&document, &document.root, 0, offset, &mut output),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        assert_eq!(output, [0xbeef; 2]);
        assert!(cache.units.is_none());
    }
    cache
        .copy(&document, &document.root, 0, 3, &mut [])
        .unwrap();
    assert!(cache.units.is_none());
    let (_empty_directory, empty) = fixture("");
    assert_eq!(empty.copy_utf16_code_units(0, &mut []), Ok(()));
    assert_eq!(
        empty.utf16_code_unit_at(0),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    );
    assert!(empty.utf16_read_cache.lock().unwrap().units.is_none());
}

#[test]
fn real_reclaim_failure_rejects_both_filled_hits_and_unpublished_partial_windows() {
    let (_directory, document) = fixture(&"x".repeat(READ_WINDOW_UTF16_UNITS * 2));
    let snapshot = document.text_snapshot();
    assert_eq!(document.utf16_code_unit_at(0), Ok(120));
    assert_eq!(snapshot.utf16_code_unit_at(0), Ok(120));
    document.fail_next_original_reclaim_for_test();
    let failure = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(failure)
    );
    // 显式真实驱逐的msync错误先毒化共享源，再重查已经填满的两个cache。
    assert_eq!(snapshot.visit_byte_range(0..1, |_| Ok(())), Err(failure));
    assert_eq!(document.utf16_code_unit_at(0), Err(failure));
    assert_eq!(snapshot.utf16_code_unit_at(0), Err(failure));
    assert_eq!(document.copy_utf16_code_units(0, &mut []), Err(failure));

    let (_second_directory, second) = fixture("尚未发布");
    let source = ReclaimAfterRead(&second);
    let mut cache = Utf16ReadCache::default();
    let mut unit = [0xbeef];
    assert_eq!(
        cache.copy(&source, &second.root, 0, 0, &mut unit),
        Err(failure)
    );
    assert_eq!(unit, [0xbeef]);
    assert_eq!(cache.units.as_ref().unwrap()[0], 0x5c1a); // 临时窗口确实已填入“尚”，错误前未发布。
    assert!(cache.valid.is_empty());
    assert_eq!(
        cache.copy(&source, &second.root, 0, 0, &mut unit),
        Err(failure)
    );
}

#[test]
fn shared_snapshot_window_is_thread_safe_and_last_owner_releases_real_mapping() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MarkdownTextSnapshot>();
    let text = "中😀ab".repeat(3000);
    let expected: Arc<[u16]> = text.encode_utf16().collect::<Vec<_>>().into();
    let (directory, document) = fixture(&text);
    let observer = document.original_backing_observer().unwrap();
    let root = Arc::downgrade(document.root.as_ref().unwrap());
    let snapshot = document.text_snapshot();
    snapshot.utf16_code_unit_at(0).unwrap();
    let mut workers = Vec::new();
    for phase in 0..4 {
        let snapshot = snapshot.clone();
        let expected = Arc::clone(&expected);
        workers.push(std::thread::spawn(move || {
            for offset in (phase..expected.len()).step_by(4) {
                // 四线程交错跨window，锁内完整填充才能对外发布，不能混入另一页的单元。
                assert_eq!(snapshot.utf16_code_unit_at(offset), Ok(expected[offset]));
            }
        }));
    }
    drop(document);
    drop(directory);
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(!observer.unmap_succeeded());
    assert!(root.upgrade().is_some());
    drop(snapshot);
    assert!(root.upgrade().is_none());
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(!observer.failed());
}

#[test]
fn poisoned_document_cache_is_typed_and_original_source_failure_still_has_priority() {
    let (_directory, document) = fixture("原文");
    assert_eq!(document.utf16_code_unit_at(0), Ok(0x539f));
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = document.utf16_read_cache.lock().unwrap();
        panic!("合成cache锁poison");
    }));
    assert!(poisoned.is_err());
    assert_eq!(
        document.utf16_code_unit_at(0),
        Err(MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)
    );
    let snapshot = document.text_snapshot();
    document.fail_next_original_reclaim_for_test();
    let failure = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(failure)
    );
    assert_eq!(snapshot.visit_byte_range(0..1, |_| Ok(())), Err(failure));
    // 原源错误不能被更晚检查的派生锁错误遮住，也不能为了锁poison改用新snapshot。
    assert_eq!(document.utf16_code_unit_at(0), Err(failure));
}
