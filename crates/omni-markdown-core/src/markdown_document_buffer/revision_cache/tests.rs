//! 摘要命中、真实piece读取、编辑与Weak资源边界；测试不替外部文件冲突路径签权限。
use super::*;
use std::cell::Cell;

fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("摘要缓存.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
    (directory, document)
}

struct CountedSource<'a> {
    document: &'a MarkdownDocumentBuffer,
    reads: Cell<usize>,
    failed: Cell<bool>,
}
impl PieceByteSource for CountedSource<'_> {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.document.original_backing_health()?;
        if self.failed.get() {
            // 这个纯接口负面对照只模拟原健康层已返回的首因，不冒称内核msync被注入。
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO,
            ))
        } else {
            Ok(())
        }
    }
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.reads.set(self.reads.get() + 1);
        self.document.piece_bytes(piece)
    }

    // 局部读取也走原文档真实存储，不能在计量包装中扩大请求范围。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.reads.set(self.reads.get() + 1);
        self.document.piece_range_bytes(piece, local)
    }
}

#[test]
fn same_immutable_root_reuses_exact_digest_without_touching_any_piece() {
    let text = "中🙂 a\r\n".repeat(2000);
    let (_directory, document) = fixture(&text);
    let source = CountedSource {
        document: &document,
        reads: Cell::new(0),
        failed: Cell::new(false),
    };
    let empty = MarkdownDocumentRevision(Sha256::digest(b"").into());
    let mut cache = RevisionCache::seeded(&None, empty);
    let expected = MarkdownDocumentRevision(Sha256::digest(text.as_bytes()).into());
    assert_eq!(cache.read(&source, &document.root).unwrap(), expected);
    let actual_reads = source.reads.get();
    assert!(actual_reads > 1); // 真正跨过多个原piece的lease，不只统计getter调用。
    for _ in 0..100 {
        assert_eq!(cache.read(&source, &document.root).unwrap(), expected);
    }
    assert_eq!(source.reads.get(), actual_reads);
    source.failed.set(true);
    assert_eq!(
        cache.read(&source, &document.root),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(source.reads.get(), actual_reads); // 健康失败在缓存命中前拒绝，不能读别的源恢复。
}

#[test]
fn open_seed_is_verified_and_edits_undo_redo_match_independent_sha() {
    let initial = "原文🙂\r\n";
    let (_directory, mut document) = fixture(initial);
    let initial_sha = MarkdownDocumentRevision(Sha256::digest(initial.as_bytes()).into());
    assert!(
        document
            .revision_cache
            .lock()
            .unwrap()
            .matches(&document.root)
    );
    assert_eq!(document.current_revision().unwrap(), initial_sha);
    let length = document.utf16_len();
    document
        .insert_utf16(length, "新内容中", MarkdownTextSelection::caret(length + 4))
        .unwrap();
    let expected =
        MarkdownDocumentRevision(Sha256::digest(format!("{initial}新内容中").as_bytes()).into());
    assert_eq!(document.current_revision().unwrap(), expected);
    document.undo().unwrap();
    assert_eq!(document.current_revision().unwrap(), initial_sha);
    document.redo().unwrap();
    assert_eq!(document.current_revision().unwrap(), expected);
    assert_ne!(expected, initial_sha);
}

#[test]
fn selection_changes_do_not_invalidate_immutable_content_or_keep_old_roots_alive() {
    let (_directory, mut document) = fixture("source🙂");
    let original = document.current_revision().unwrap();
    let strong = Arc::strong_count(document.root.as_ref().unwrap());
    let cache = RevisionCache::seeded(&document.root, original);
    assert_eq!(Arc::strong_count(document.root.as_ref().unwrap()), strong);
    document
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    assert_eq!(document.current_revision().unwrap(), original);
    let backing = document.original_backing_observer().unwrap();
    drop(document);
    assert!(cache.root.as_ref().unwrap().upgrade().is_none());
    assert!(backing.unmap_succeeded()); // 这个仍活Weak缓存没有阻止真实最后munmap。
    assert_eq!(backing.mapped_bytes(), 0);
}

#[test]
fn same_bytes_in_a_different_root_do_not_reuse_another_root_key() {
    let (_first_directory, first) = fixture("same bytes");
    let (_second_directory, second) = fixture("same bytes");
    let mut cache = RevisionCache::seeded(&first.root, first.current_revision().unwrap());
    let counted = CountedSource {
        document: &second,
        reads: Cell::new(0),
        failed: Cell::new(false),
    };
    assert!(!cache.matches(&second.root));
    assert_eq!(
        cache.read(&counted, &second.root).unwrap(),
        second.current_revision().unwrap()
    );
    assert!(counted.reads.get() > 0);
}

#[test]
fn poisoned_revision_lock_is_typed_failure_and_never_recovered() {
    let (_directory, document) = fixture("锁错误仍保留正文");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = document.revision_cache.lock().unwrap();
        panic!("合成摘要锁poison");
    }));
    assert!(result.is_err());
    assert_eq!(
        document.current_revision(),
        Err(MarkdownDocumentBufferError::RevisionCacheUnavailable)
    );
    assert_eq!(
        document.current_revision(),
        Err(MarkdownDocumentBufferError::RevisionCacheUnavailable)
    );
    let mut unit = [0];
    document.copy_utf16_code_units(0, &mut unit).unwrap(); // 不相关正文读取仍使用原独立健康窗口。
    assert_eq!(unit[0], '锁' as u16);
}
