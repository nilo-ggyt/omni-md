//! 用真实canonical打开、编辑与冻结读取验证新窗口所有权，不以独立C控制替代Core路径。

use super::*;
use crate::markdown_document_buffer::piece_tree::build_original_piece_tree;

#[test]
fn initial_streaming_tree_matches_original_metrics_digest_and_scalar_boundaries() {
    for source in [
        String::new(),
        "\u{feff}中😀\r\n".repeat(2400),
        format!("{}😀中\r\nend", "x".repeat(4095)),
        format!("{}\r\n中😀\r", "x".repeat(4095)),
        "\u{2029}\u{85}\u{2028}e\u{301}".repeat(2300),
    ] {
        let fixture = MarkdownBufferFixture::new(&source);
        let (old_root, old_digest) = build_original_piece_tree(&source);
        let expected = old_root
            .as_ref()
            .map_or_else(MarkdownDocumentMetrics::default, |root| root.metrics());
        assert_eq!(fixture.buffer.current_revision().unwrap(), old_digest);
        assert_eq!(
            fixture
                .buffer
                .root
                .as_ref()
                .map_or_else(MarkdownDocumentMetrics::default, |root| root.metrics()),
            expected
        );
        let snapshot = fixture.buffer.text_snapshot();
        let mut utf16 = 0;
        for (ordinal, (byte, scalar)) in source.char_indices().enumerate() {
            let inspect = ordinal < 16
                || ordinal % 127 == 0
                || byte.abs_diff(4096) < 8
                || byte.abs_diff(8192) < 8;
            if inspect {
                assert_eq!(snapshot.byte_offset_for_utf16(utf16).unwrap(), byte);
                assert_eq!(snapshot.utf16_offset_for_byte(byte).unwrap(), utf16);
            }
            if inspect && scalar.len_utf16() == 2 {
                let pair: Vec<_> = scalar.encode_utf16(&mut [0; 2]).to_vec();
                assert_eq!(snapshot.utf16_code_unit_at(utf16 + 1).unwrap(), pair[1]);
            }
            utf16 += scalar.len_utf16();
        }
        assert_eq!(snapshot.byte_offset_for_utf16(utf16).unwrap(), source.len());
    }
}

#[test]
fn malformed_utf8_across_each_initial_window_boundary_is_rejected() {
    for tail in [
        &b"\xf0\x9f\x98"[..],
        &b"\xed\xa0\x80"[..],
        &b"\xe4\xff\xad"[..],
        &b"\x80"[..],
    ] {
        for padding in 4092..=4097 {
            let directory = tempdir().unwrap();
            let path = directory.path().join("invalid.md");
            let mut source = vec![b'a'; padding];
            source.extend_from_slice(tail);
            fs::write(&path, source).unwrap();
            assert!(matches!(
                MarkdownDocumentBuffer::open(&path, 16384),
                Err(MarkdownDocumentBufferError::InvalidUtf8Document)
            ));
        }
    }
}

#[test]
fn frozen_history_and_external_replacement_keep_same_mapping_until_final_snapshot_drop() {
    let original = format!("{}😀中\r\n", "x".repeat(8191));
    let mut fixture = MarkdownBufferFixture::new(&original);
    let observer = fixture.buffer.original_backing_observer().unwrap();
    let frozen = fixture.buffer.text_snapshot();
    let revision = fixture.buffer.current_revision().unwrap();
    fixture
        .buffer
        .replace_utf16_range(1..3, "新增😀", MarkdownTextSelection::caret(5))
        .unwrap();
    assert_ne!(fixture.buffer.current_revision().unwrap(), revision);
    fixture.buffer.undo_text_edit().unwrap();
    assert_eq!(fixture.buffer.current_revision().unwrap(), revision);
    fixture.buffer.redo_text_edit().unwrap();
    assert_ne!(fixture.buffer.current_revision().unwrap(), revision);
    fs::write(fixture.buffer.path(), b"outside").unwrap();
    let mut writer = RecordingWriter::default();
    fixture.buffer.write_to(&mut writer).unwrap();
    assert!(writer.maximum_write_bytes <= ORIGINAL_PIECE_BYTES);
    drop(fixture);
    assert!(!observer.unmap_succeeded());
    let mut actual = Vec::new();
    frozen
        .visit_byte_range(0..frozen.byte_len(), |bytes| {
            actual.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
    assert_eq!(actual, original.as_bytes());
    drop(frozen);
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(!observer.failed());
}

#[test]
fn callback_sentinel_cannot_hide_actual_reclaim_failure_or_sign_metadata_queries() {
    let source = format!("中😀\r\nimmutable{}", "x".repeat(128 * 1024));
    let fixture = MarkdownBufferFixture::new(&source);
    let snapshot = fixture.buffer.text_snapshot();
    let backing = fixture.buffer.original_snapshot.as_ref().unwrap();
    // 当前读取页先实际装入；另有无pin页供回调内的真实驱逐触发故障。
    snapshot.visit_byte_range(0..1, |_| Ok(())).unwrap();
    backing.fail_next_reclaim_for_test();
    let result = snapshot.visit_byte_range(0..1, |_| {
        assert_eq!(
            backing.reclaim_unpinned_for_test(),
            Err(crate::original_backing::OriginalBackingError::Unavailable(
                libc::EIO
            ))
        );
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    });
    assert_eq!(
        result,
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(
        fixture.buffer.current_revision(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(
        snapshot.byte_offset_for_utf16(0),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(
        snapshot.text_checking_paragraph_range(0..0),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(snapshot.visit_byte_range(0..0, |_| Ok(())), result);
}
