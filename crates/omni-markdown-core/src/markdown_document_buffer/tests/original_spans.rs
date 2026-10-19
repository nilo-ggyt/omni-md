//! 共享原始小目录的实际存储、跨层坐标和持久历史回归；完整oracle仅属于有限合成夹具。

use super::*;
use std::ops::Range;

#[test]
fn actual_original_span_does_not_inflate_the_persistent_node() {
    // 比较真实生产类型，不用布局镜像代替编译器实际布局；普通编辑叶仍决定最大variant。
    let node = std::mem::size_of::<MarkdownPieceTreeNode>();
    let piece = std::mem::size_of::<super::super::MarkdownPiece>();
    let span = std::mem::size_of::<super::super::OriginalSpan>();
    assert_eq!(node, piece);
    assert!(span <= piece);
    println!("event=actual_original_span_layout node={node} piece={piece} span={span}");
}

#[test]
fn original_spans_preserve_real_four_kib_borrows_and_all_scalar_coordinates() {
    // 夹具跨多个64KiB物理叶，并在4KiB与64KiB两种目录边界放置跨片CRLF。
    let mut source = "x".repeat(192 * 1024);
    for boundary in [4096, 65536, 131072] {
        source.replace_range(boundary - 1..boundary + 1, "\r\n");
    }
    for byte in [128, 65536 + 128, 131072 + 128] {
        // 等长替换保留上面已知物理边界，同时加入代理项、组合字符和段落终止符。
        source.replace_range(byte..byte + 13, "中😀e\u{301}\u{2029}");
    }
    let fixture = MarkdownBufferFixture::new(&source);
    let snapshot = fixture.buffer.text_snapshot();
    let mut actual = Vec::new();
    snapshot
        .visit_byte_range(0..snapshot.byte_len(), |bytes| {
            assert!(bytes.len() <= ORIGINAL_PIECE_BYTES);
            actual.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
    assert_eq!(actual, source.as_bytes());
    assert_tree_is_avl(&fixture.buffer.root);
    let mut utf16 = 0;
    for (ordinal, (byte, scalar)) in source.char_indices().enumerate() {
        // 抽查全部标量流中的固定频率点与两层边界邻域；低代理项仍按NSString单元读取。
        if ordinal % 127 == 0
            || [4096, 65536, 131072]
                .iter()
                .any(|at| byte.abs_diff(*at) < 16)
        {
            assert_eq!(snapshot.byte_offset_for_utf16(utf16), Ok(byte));
            assert_eq!(snapshot.utf16_offset_for_byte(byte), Ok(utf16));
        }
        if scalar.len_utf16() == 2 {
            assert_eq!(
                snapshot.byte_offset_for_utf16(utf16 + 1),
                Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
            );
            assert_eq!(
                snapshot.utf16_code_unit_at(utf16 + 1),
                Ok(scalar.encode_utf16(&mut [0; 2])[1])
            );
        }
        utf16 += scalar.len_utf16();
    }
    assert_eq!(snapshot.byte_offset_for_utf16(utf16), Ok(source.len()));

    // 独立逐byte物理行oracle核对CRLF合并；段落检查仍按原始CR/LF/PS分别定位。
    let mut line_starts = vec![0];
    let mut cursor = 0;
    while cursor < source.len() {
        match source.as_bytes()[cursor] {
            b'\r' if source.as_bytes().get(cursor + 1) == Some(&b'\n') => {
                cursor += 2;
                line_starts.push(cursor);
            }
            b'\r' | b'\n' => {
                cursor += 1;
                line_starts.push(cursor);
            }
            _ => cursor += 1,
        }
    }
    assert_eq!(fixture.buffer.line_count(), line_starts.len());
    for (line, byte) in line_starts.into_iter().enumerate() {
        assert_eq!(fixture.buffer.byte_offset_for_line(line), Ok(byte));
    }
    let units: Vec<_> = source.encode_utf16().collect();
    for start in (0..=units.len()).step_by(113) {
        for end in [start, (start + 1).min(units.len())] {
            let expected = checking_oracle(&units, start..end);
            assert_eq!(
                snapshot.text_checking_paragraph_range(start..end),
                Ok(expected)
            );
        }
    }
}

fn checking_oracle(units: &[u16], request: Range<usize>) -> Range<usize> {
    // 此独立有限数组只用于测试；严格before、空caret和CRLF端点与原平台合同一致。
    let terminator = |unit: u16| matches!(unit, 0x000d | 0x000a | 0x2029);
    let start = units[..request.start]
        .iter()
        .rposition(|unit| terminator(*unit))
        .map_or(0, |at| at + 1);
    let anchor = if request.is_empty() {
        request.end
    } else {
        request.end - 1
    };
    let Some(at) = units[anchor..]
        .iter()
        .position(|unit| terminator(*unit))
        .map(|at| anchor + at)
    else {
        return start..units.len();
    };
    let end = at + 1 + usize::from(units[at] == 0x000d && units.get(at + 1) == Some(&0x000a));
    start..end
}

#[test]
fn shared_original_directories_keep_all_history_entries_and_frozen_resource_release() {
    let original = "中文😀a\r\nb e\u{301}\u{2029}".repeat(8_000);
    let mut fixture = MarkdownBufferFixture::new(&original);
    let observer = fixture.buffer.original_backing_observer().unwrap();
    let frozen = fixture.buffer.text_snapshot();
    let mut expected = original.clone();
    let mut history = Vec::new();
    for ordinal in 0..MAX_EDIT_HISTORY_ENTRIES {
        // 每次在不同原始目录附近落入一个真实标量边界；历史保存原root，不能展开全部小片。
        let at = (ordinal * 1597) % expected.len();
        let at = (at..=expected.len())
            .find(|at| expected.is_char_boundary(*at))
            .unwrap();
        let unit = expected[..at].encode_utf16().count();
        history.push(expected.clone());
        expected.insert_str(at, "增😀");
        fixture
            .buffer
            .insert_utf16(unit, "增😀", MarkdownTextSelection::caret(unit + 3))
            .unwrap();
        assert_tree_is_avl(&fixture.buffer.root);
    }
    assert_eq!(
        fixture.buffer.storage_profile().undo_entries,
        MAX_EDIT_HISTORY_ENTRIES
    );
    for before in history.iter().rev() {
        // 128次真实Undo逐步恢复原事务字节与目录共享，不能只核最终摘要碰巧相同。
        fixture.buffer.undo_text_edit().unwrap();
        assert_eq!(streamed_text(&fixture.buffer), *before);
        assert_tree_is_avl(&fixture.buffer.root);
    }
    assert!(!fixture.buffer.can_undo());
    assert_eq!(streamed_text(&fixture.buffer), original);
    drop(fixture);
    assert!(!observer.unmap_succeeded());
    let mut restored = Vec::new();
    frozen
        .visit_byte_range(0..frozen.byte_len(), |bytes| {
            assert!(bytes.len() <= ORIGINAL_PIECE_BYTES);
            restored.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
    assert_eq!(restored, original.as_bytes());
    drop(frozen);
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(!observer.failed());
}
