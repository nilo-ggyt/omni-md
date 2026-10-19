//! 选区输出保留原字节和历史；错误发生时不把部分目标当作完整复制。

use super::*;

#[test]
fn selected_original_and_unsaved_pieces_preserve_crlf_emoji_and_exact_partial_bytes() {
    // CR/LF跨原4KiB片；后面保留多个原片，并在它们之间加入真实未保存编辑片。
    let original = format!(
        "{}\r\n中😀e\u{301}{}\r\n尾",
        "a".repeat(ORIGINAL_PIECE_BYTES - 1),
        "b".repeat(ORIGINAL_PIECE_BYTES + 27)
    );
    let mut fixture = MarkdownBufferFixture::new(&original);
    let inserted = "未保存😀\r\n";
    fixture
        .buffer
        .replace_utf16_range(
            ORIGINAL_PIECE_BYTES + 1..ORIGINAL_PIECE_BYTES + 2,
            inserted,
            MarkdownTextSelection::caret(
                ORIGINAL_PIECE_BYTES + 1 + inserted.encode_utf16().count(),
            ),
        )
        .unwrap();
    let expected = original.replacen('中', inserted, 1);
    let range = ORIGINAL_PIECE_BYTES - 5..ORIGINAL_PIECE_BYTES * 2 + 3;
    let expected_units = expected.encode_utf16().collect::<Vec<_>>();
    let selected = String::from_utf16(&expected_units[range.clone()]).unwrap();
    let revision = fixture.buffer.current_revision().unwrap();
    let generation = fixture.buffer.generation();
    let selection = fixture.buffer.selection();
    let backing = fixture.buffer.original_backing_observer().unwrap();
    let mut destination = Vec::new();
    fixture
        .buffer
        .write_utf16_range_to(range, &mut destination)
        .unwrap();
    assert_eq!(destination, selected.as_bytes());
    assert!(selected.contains("\r\n未保存😀\r\n😀e\u{301}"));
    assert_eq!(fixture.buffer.current_revision().unwrap(), revision);
    assert_eq!(fixture.buffer.generation(), generation);
    assert_eq!(fixture.buffer.selection(), selection);
    assert!(fixture.buffer.is_dirty() && fixture.buffer.can_undo() && !fixture.buffer.can_redo());
    fixture.buffer.undo_text_edit().unwrap();
    assert_eq!(streamed_text(&fixture.buffer), original);
    fixture.buffer.redo_text_edit().unwrap();
    assert_eq!(streamed_text(&fixture.buffer), expected);
    drop(fixture);
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

#[test]
fn invalid_ranges_and_surrogate_halves_are_rejected_before_touching_the_destination() {
    let fixture = MarkdownBufferFixture::new("a😀\r\n中");
    for (range, expected) in [
        (2..3, MarkdownDocumentBufferError::NotUnicodeBoundary),
        (0..2, MarkdownDocumentBufferError::NotUnicodeBoundary),
        (2..2, MarkdownDocumentBufferError::NotUnicodeBoundary),
        (
            std::ops::Range { start: 4, end: 1 },
            MarkdownDocumentBufferError::RangeOutsideDocument,
        ),
        (0..7, MarkdownDocumentBufferError::RangeOutsideDocument),
    ] {
        let mut destination = "已有输出".as_bytes().to_vec();
        assert_eq!(
            fixture.buffer.write_utf16_range_to(range, &mut destination),
            Err(expected)
        );
        assert_eq!(
            destination,
            "已有输出".as_bytes(),
            "非法范围不得留下部分目标"
        );
    }
}

#[test]
fn real_writer_failure_keeps_canonical_selection_and_undo_redo_unchanged() {
    let mut fixture = MarkdownBufferFixture::new("原文😀\r\n正文");
    fixture
        .buffer
        .replace_utf16_range(0..2, "草稿", MarkdownTextSelection::caret(2))
        .unwrap();
    fixture
        .buffer
        .replace_utf16_range(0..2, "再次编辑", MarkdownTextSelection::caret(4))
        .unwrap();
    fixture.buffer.undo_text_edit().unwrap(); // 同时保有Undo和Redo，不能只检查原来空的历史。
    let revision = fixture.buffer.current_revision().unwrap();
    let generation = fixture.buffer.generation();
    let selection = fixture.buffer.selection();
    let original = streamed_text(&fixture.buffer);
    // macOS的只读/dev/null FD是真实OS写入失败；不用只返回错误的假writer替代资源操作。
    let mut destination = fs::File::open("/dev/null").unwrap();
    assert_eq!(
        fixture
            .buffer
            .write_utf16_range_to(0..fixture.buffer.utf16_len(), &mut destination),
        Err(MarkdownDocumentBufferError::WriteUnavailable)
    );
    assert_eq!(fixture.buffer.current_revision().unwrap(), revision);
    assert_eq!(fixture.buffer.generation(), generation);
    assert_eq!(fixture.buffer.selection(), selection);
    assert_eq!(streamed_text(&fixture.buffer), original);
    assert!(fixture.buffer.is_dirty() && fixture.buffer.can_undo() && fixture.buffer.can_redo());
}

#[test]
fn empty_and_eof_ranges_write_nothing_but_cannot_bypass_failed_source_health() {
    let empty = MarkdownBufferFixture::new("");
    let mut destination = b"prefix".to_vec();
    empty
        .buffer
        .write_utf16_range_to(0..0, &mut destination)
        .unwrap();
    let fixture = MarkdownBufferFixture::new(&"原文😀\r\n".repeat(12_000));
    let length = fixture.buffer.utf16_len();
    fixture
        .buffer
        .write_utf16_range_to(length..length, &mut destination)
        .unwrap();
    assert_eq!(destination, b"prefix");
    // 先实际读取再触发原无pin页驱逐；注入位本身不能被当作已经发生的EIO。
    let snapshot = fixture.buffer.text_snapshot();
    snapshot.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.buffer.fail_next_original_reclaim_for_test();
    assert_eq!(
        fixture.buffer.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    for range in [0..0, length..length] {
        assert_eq!(
            fixture.buffer.write_utf16_range_to(range, &mut destination),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        );
        assert_eq!(destination, b"prefix");
    }
    let observer = fixture.buffer.original_backing_observer().unwrap();
    drop((snapshot, fixture));
    assert!(observer.unmap_succeeded() && observer.mapped_bytes() == 0);
    assert_eq!(observer.first_errno(), Some(libc::EIO));
}

#[test]
fn actual_reclaim_failure_during_a_failed_write_keeps_the_original_source_errno() {
    struct ReclaimingWriter<'a> {
        document: &'a MarkdownDocumentBuffer,
        readonly: fs::File,
    }
    impl Write for ReclaimingWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            // 当前原叶仍被pin；另一个已读页可真实驱逐，检验回调错误不能洗掉原Source首错。
            self.document.fail_next_original_reclaim_for_test();
            assert_eq!(
                self.document.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
            self.readonly.write(bytes) // 同时保留真实只读FD的写入失败，不用伪造writer结果。
        }
        fn flush(&mut self) -> io::Result<()> {
            self.readonly.flush()
        }
    }
    let fixture = MarkdownBufferFixture::new(&"x".repeat(128 * 1024));
    let snapshot = fixture.buffer.text_snapshot();
    snapshot
        .visit_byte_range(64 * 1024..64 * 1024 + 1, |_| Ok(()))
        .unwrap();
    let generation = fixture.buffer.generation();
    let selection = fixture.buffer.selection();
    let observer = fixture.buffer.original_backing_observer().unwrap();
    {
        let mut writer = ReclaimingWriter {
            document: &fixture.buffer,
            readonly: fs::File::open("/dev/null").unwrap(),
        };
        assert_eq!(
            fixture.buffer.write_utf16_range_to(0..1, &mut writer),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        );
    } // writer的真实FD及其文档借用先结束，再验证原映射最终归还。
    assert_eq!(fixture.buffer.generation(), generation);
    assert_eq!(fixture.buffer.selection(), selection);
    assert!(!fixture.buffer.is_dirty() && !fixture.buffer.can_undo() && !fixture.buffer.can_redo());
    drop((snapshot, fixture));
    assert!(observer.unmap_succeeded() && observer.mapped_bytes() == 0);
    assert_eq!(observer.first_errno(), Some(libc::EIO));
}
