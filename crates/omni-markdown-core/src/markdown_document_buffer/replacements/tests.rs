//! 合成正文上的事务、共享、Unicode 与有界密集输出回归；不代替 native Find 验收。

use super::*;
use std::cell::Cell;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use tempfile::{TempDir, tempdir};

type Parts = Vec<Result<MarkdownReplacementPart<'static>, MarkdownDocumentBufferError>>;
type Replacement = MarkdownReplacement<Parts>;

fn fixture(text: &str, maximum: usize) -> (TempDir, MarkdownDocumentBuffer) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("synthetic.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, maximum as u64).unwrap();
    (directory, document)
}

fn literal(range: Range<usize>, text: &str) -> Result<Replacement, MarkdownDocumentBufferError> {
    Ok(MarkdownReplacement {
        range_utf16: range,
        parts: vec![Ok(MarkdownReplacementPart::Literal(text.to_owned().into()))],
    })
}

fn text(document: &MarkdownDocumentBuffer) -> String {
    // 只有有限的合成测试输出可物化；生产事务从不通过完整 String 取得结果。
    let mut bytes = Vec::new();
    document.write_to(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

fn identity(root: &PieceTreeLink) -> Option<usize> {
    root.as_ref().map(|root| Arc::as_ptr(root).addr())
}

/// 既比较数据与 geometry，也比较 append 的内容、容量及两侧 history 身份。
struct Snapshot {
    text: String,
    selection: MarkdownTextSelection,
    generation: u64,
    dirty: bool,
    root: Option<usize>,
    profile: MarkdownDocumentStorageProfile,
    // 与不可变值相同地共享段，验证失败回滚没有写穿测试保存的原字节。
    segments: Vec<Arc<MarkdownEditSegment>>,
    segment_capacity: usize,
    undo: Vec<(
        Option<usize>,
        MarkdownTextSelection,
        MarkdownReplacementMetrics,
    )>,
    redo: Vec<(
        Option<usize>,
        MarkdownTextSelection,
        MarkdownReplacementMetrics,
    )>,
}

impl Snapshot {
    fn capture(document: &MarkdownDocumentBuffer) -> Self {
        let history = |entries: &VecDeque<MarkdownEditorMemento>| {
            entries
                .iter()
                .map(|entry| (identity(&entry.root), entry.selection, entry.replacement))
                .collect()
        };
        Self {
            text: text(document),
            selection: document.selection,
            generation: document.generation,
            dirty: document.is_dirty(),
            root: identity(&document.root),
            profile: document.storage_profile(),
            segments: document.edits.segments.clone(),
            segment_capacity: document.edits.segments.capacity(),
            undo: history(&document.undo),
            redo: history(&document.redo),
        }
    }

    fn verify(&self, document: &MarkdownDocumentBuffer) {
        let after = Self::capture(document);
        assert_eq!(after.text, self.text);
        assert_eq!(after.selection, self.selection);
        assert_eq!(after.generation, self.generation);
        assert_eq!(after.dirty, self.dirty);
        assert_eq!(after.root, self.root);
        assert_eq!(after.profile, self.profile);
        assert_eq!(after.segments, self.segments);
        assert_eq!(after.segment_capacity, self.segment_capacity);
        assert_eq!(after.undo, self.undo);
        assert_eq!(after.redo, self.redo);
    }
}

fn seed_redo(document: &mut MarkdownDocumentBuffer, inserted: &str) {
    // 即使失败命令曾经追加，原 redo 仍必须可用，不能只证明当前正文没变。
    document
        .replace_utf16_range(0..1, inserted, MarkdownTextSelection::caret(0))
        .unwrap();
    document.undo_text_edit().unwrap();
    assert!(document.can_redo());
}

#[test]
fn unicode_crlf_and_old_capture_ranges_commit_one_reversible_envelope() {
    let (_directory, mut document) = fixture("A😀\r\nBeta gamma", 4096);
    let before = MarkdownTextSelection {
        anchor_utf16: 15,
        head_utf16: 0,
    };
    let after = MarkdownTextSelection {
        anchor_utf16: 18,
        head_utf16: 0,
    };
    let replacements = vec![
        Ok(MarkdownReplacement {
            range_utf16: 1..3,
            parts: vec![
                Ok(MarkdownReplacementPart::Literal("<".into())),
                Ok(MarkdownReplacementPart::SourceRange(10..15)),
                Ok(MarkdownReplacementPart::Literal(">".into())),
            ],
        }),
        Ok(MarkdownReplacement {
            range_utf16: 5..9,
            // 第一项已替换 emoji，但 capture 仍从旧 canonical root 取它，不能读候选输出。
            parts: vec![Ok(MarkdownReplacementPart::SourceRange(1..3))],
        }),
    ];
    let edit = document
        .apply_replacements(replacements, before, after)
        .unwrap()
        .unwrap();
    assert_eq!(text(&document), "A<gamma>\r\n😀 gamma");
    assert_eq!(edit.replaced_utf16_range, 1..9);
    assert_eq!(edit.change_in_utf16_length, 3);
    assert_eq!(edit.selection_after, after);
    assert_eq!(document.storage_profile().undo_entries, 1);
    let undone = document.undo_text_edit().unwrap();
    assert_eq!(undone.replaced_utf16_range, 1..12);
    assert_eq!(undone.change_in_utf16_length, -3);
    assert_eq!(undone.selection_after, before);
    assert_eq!(text(&document), "A😀\r\nBeta gamma");
    assert_eq!(document.redo_text_edit().unwrap(), edit);
    assert_eq!(text(&document), "A<gamma>\r\n😀 gamma");
}

#[test]
fn many_unicode_ranges_and_old_captures_match_an_independent_string_model_across_pieces() {
    let input = "前😀\r\nAbé漢🙂\n".repeat(2000);
    let (_directory, mut document) = fixture(&input, input.len() * 4);
    // 原文中先加入一个 edit piece，后续 SourceRange 必须与文件映射采用同一旧坐标空间。
    document
        .replace_utf16_range(5..6, "𠀀Z", MarkdownTextSelection::caret(0))
        .unwrap();
    let original = text(&document);
    let mut units = 0;
    let mut positions: Vec<_> = original
        .char_indices()
        .map(|(byte, scalar)| {
            let position = (byte, units);
            units += scalar.len_utf16();
            position
        })
        .collect();
    positions.push((original.len(), units));
    let count = positions.len() - 1;
    let mut expected = String::new();
    let mut consumed = 0;
    let mut replacements = Vec::new();
    for index in (0..count).step_by(13) {
        // 模型只用标准 str 的 scalar/byte 边界；不调用 core 的 mapper 来计算预期。
        let end = (index + 3).min(count);
        let captured = (index * 7 + 9) % count;
        expected.push_str(&original[consumed..positions[index].0]);
        expected.push('置');
        expected.push_str(&original[positions[captured].0..positions[captured + 1].0]);
        consumed = positions[end].0;
        replacements.push(Ok(MarkdownReplacement {
            range_utf16: positions[index].1..positions[end].1,
            parts: vec![
                Ok(MarkdownReplacementPart::Literal("置".into())),
                Ok(MarkdownReplacementPart::SourceRange(
                    positions[captured].1..positions[captured + 1].1,
                )),
            ],
        }));
    }
    expected.push_str(&original[consumed..]);
    let before = MarkdownTextSelection {
        anchor_utf16: units,
        head_utf16: 0,
    };
    let after = MarkdownTextSelection::caret(expected.encode_utf16().count());
    let receipt = document
        .apply_replacements(replacements, before, after)
        .unwrap()
        .unwrap();
    assert_eq!(text(&document), expected);
    assert_eq!(document.storage_profile().undo_entries, 2);
    assert_eq!(document.undo_text_edit().unwrap().selection_after, before);
    assert_eq!(text(&document), original);
    assert_eq!(document.redo_text_edit().unwrap(), receipt);
    assert_eq!(text(&document), expected);
}

#[test]
fn empty_input_preserves_selection_generation_and_existing_redo() {
    let (_directory, mut document) = fixture("abc😀", 4096);
    seed_redo(&mut document, "x");
    let snapshot = Snapshot::capture(&document);
    assert!(
        document
            .apply_replacements(
                std::iter::empty::<Result<Replacement, MarkdownDocumentBufferError>>(),
                MarkdownTextSelection::caret(0),
                MarkdownTextSelection::caret(5),
            )
            .unwrap()
            .is_none()
    );
    snapshot.verify(&document);
    let error = document
        .apply_replacements(
            std::iter::empty::<Result<Replacement, MarkdownDocumentBufferError>>(),
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(4),
        )
        .unwrap_err();
    assert_eq!(error, MarkdownDocumentBufferError::NotUnicodeBoundary);
    snapshot.verify(&document);
}

#[test]
fn both_iterator_levels_rollback_appended_segments_capacity_and_redo() {
    for (inner_failure, rejection) in [
        (
            false,
            MarkdownDocumentBufferError::ReplacementInputUnavailable,
        ),
        (
            true,
            MarkdownDocumentBufferError::ReplacementInputUnavailable,
        ),
        (false, MarkdownDocumentBufferError::FindInterrupted),
        (true, MarkdownDocumentBufferError::FindInterrupted),
    ] {
        let (_directory, mut document) = fixture(&"a".repeat(96 * 1024), 384 * 1024);
        seed_redo(&mut document, "existing append tail");
        let snapshot = Snapshot::capture(&document);
        let first = MarkdownReplacement {
            range_utf16: 0..1,
            parts: vec![Ok(MarkdownReplacementPart::Literal(
                "x".repeat(EDIT_SEGMENT_BYTES * 5 + 32).into(),
            ))],
        };
        let replacements = if inner_failure {
            // 第一个 literal 已实际分配新 segment，再模拟当前 parts 的读取失败。
            let mut first = first;
            first.parts.push(Err(rejection));
            vec![Ok(first)]
        } else {
            // 第一项消费完后外层输入失败，两侧 iterator 必须走同一个完整回滚。
            vec![Ok(first), Err(rejection)]
        };
        let error = document
            .apply_replacements(
                replacements,
                MarkdownTextSelection::caret(0),
                MarkdownTextSelection::caret(0),
            )
            .unwrap_err();
        assert_eq!(error, rejection);
        snapshot.verify(&document);
        assert!(document.redo_text_edit().is_some());
        assert!(text(&document).starts_with("existing append tail"));
    }
}

#[test]
fn invalid_late_ranges_and_capture_boundaries_preserve_all_prior_state() {
    for (range, part, expected) in [
        (
            Range { start: 8, end: 7 },
            None,
            MarkdownDocumentBufferError::RangeOutsideDocument,
        ),
        (
            100_000..100_001,
            None,
            MarkdownDocumentBufferError::RangeOutsideDocument,
        ),
        (
            0..1,
            None,
            MarkdownDocumentBufferError::ReplacementRangesNotOrdered,
        ),
        (4..5, None, MarkdownDocumentBufferError::NotUnicodeBoundary),
        (
            5..6,
            Some(3..4),
            MarkdownDocumentBufferError::NotUnicodeBoundary,
        ),
        (
            5..6,
            Some(Range { start: 9, end: 8 }),
            MarkdownDocumentBufferError::RangeOutsideDocument,
        ),
    ] {
        let input = format!("abc😀{}", "a".repeat(8192));
        let (_directory, mut document) = fixture(&input, 384 * 1024);
        seed_redo(&mut document, "previous");
        let snapshot = Snapshot::capture(&document);
        let second = MarkdownReplacement {
            range_utf16: range,
            parts: vec![Ok(part.map_or_else(
                || MarkdownReplacementPart::Literal("y".into()),
                MarkdownReplacementPart::SourceRange,
            ))],
        };
        let error = document
            .apply_replacements(
                vec![literal(0..1, &"x".repeat(70 * 1024)), Ok(second)],
                MarkdownTextSelection::caret(0),
                MarkdownTextSelection::caret(0),
            )
            .unwrap_err();
        assert_eq!(error, expected);
        snapshot.verify(&document);
    }
}

#[test]
fn touching_ranges_are_ordered_but_duplicate_empty_positions_are_rejected() {
    let (_directory, mut document) = fixture("abc", 4096);
    document
        .apply_replacements(
            vec![literal(0..1, "A"), literal(1..2, "B"), literal(2..2, "!")],
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(3),
        )
        .unwrap();
    assert_eq!(text(&document), "AB!c");
    let snapshot = Snapshot::capture(&document);
    let error = document
        .apply_replacements(
            vec![literal(1..1, "x"), literal(1..1, "y")],
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(0),
        )
        .unwrap_err();
    assert_eq!(
        error,
        MarkdownDocumentBufferError::ReplacementRangesNotOrdered
    );
    snapshot.verify(&document);
}

#[test]
fn invalid_final_selection_rolls_back_after_actual_large_append() {
    for (after, expected) in [
        (
            MarkdownTextSelection::caret(1),
            MarkdownDocumentBufferError::NotUnicodeBoundary,
        ),
        (
            MarkdownTextSelection::caret(usize::MAX),
            MarkdownDocumentBufferError::RangeOutsideDocument,
        ),
    ] {
        let (_directory, mut document) = fixture("original", 384 * 1024);
        seed_redo(&mut document, "old tail");
        let snapshot = Snapshot::capture(&document);
        let replacement = format!("😀{}", "x".repeat(70 * 1024));
        let error = document
            .apply_replacements(
                vec![literal(0..8, &replacement)],
                MarkdownTextSelection::caret(0),
                after,
            )
            .unwrap_err();
        assert_eq!(error, expected);
        snapshot.verify(&document);
    }
}

#[test]
fn lifetime_append_budget_and_live_size_failures_preserve_existing_redo() {
    let (_directory, mut document) = fixture(&"a".repeat(8192), 16 * 1024);
    // 先用较长的整文替换消耗追加预算，再 undo；live 文本恢复较短但旧 segment 仍有效。
    document
        .replace_utf16_range(
            0..8192,
            &"z".repeat(10 * 1024),
            MarkdownTextSelection::caret(0),
        )
        .unwrap();
    document.undo_text_edit().unwrap();
    let snapshot = Snapshot::capture(&document);
    let error = document
        .apply_replacements(
            vec![literal(0..8192, &"x".repeat(8192))],
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(0),
        )
        .unwrap_err();
    assert_eq!(error, MarkdownDocumentBufferError::EditBufferLimitExceeded);
    snapshot.verify(&document);
    let error = document
        .apply_replacements(
            vec![literal(0..0, &"x".repeat(16 * 1024 + 1))],
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(0),
        )
        .unwrap_err();
    assert_eq!(error, MarkdownDocumentBufferError::DocumentTooLarge);
    snapshot.verify(&document);
}

#[test]
fn giant_capture_reuses_old_edited_pieces_without_appended_text_copy() {
    let original = "🙂漢".repeat(64 * 1024);
    let (_directory, mut document) = fixture("seed", original.len() * 3);
    let units = original.encode_utf16().count();
    document
        .replace_utf16_range(0..4, &original, MarkdownTextSelection::caret(units))
        .unwrap();
    let before = document.storage_profile();
    let receipt = document
        .apply_replacements(
            vec![Ok(MarkdownReplacement {
                range_utf16: 0..units,
                parts: vec![
                    Ok(MarkdownReplacementPart::SourceRange(0..units)),
                    Ok(MarkdownReplacementPart::SourceRange(0..units)),
                ],
            })],
            MarkdownTextSelection::caret(units),
            MarkdownTextSelection::caret(units * 2),
        )
        .unwrap()
        .unwrap();
    let after = document.storage_profile();
    assert_eq!(after.appended_edit_bytes, before.appended_edit_bytes);
    assert_eq!(
        after.allocated_edit_segment_bytes,
        before.allocated_edit_segment_bytes
    );
    assert_eq!(after.undo_entries, before.undo_entries + 1);
    assert_eq!(document.byte_len(), original.len() * 2);
    assert_eq!(receipt.change_in_utf16_length, units as isize);
    assert_eq!(text(&document), original.repeat(2));
    document.undo_text_edit().unwrap();
    assert_eq!(text(&document), original);
}

#[test]
fn dense_replacements_pack_metadata_by_output_chunks_without_match_list() {
    let input = "a ".repeat(256 * 1024);
    let (_directory, mut document) = fixture(&input, input.len() * 3);
    let started = std::time::Instant::now();
    let replacements = (0..input.len()).step_by(2).map(|offset| {
        // 每次只分配当前一个 literal，外层没有全量 range/match Vec。
        Ok(MarkdownReplacement {
            range_utf16: offset..offset + 1,
            parts: std::iter::once(Ok(MarkdownReplacementPart::Literal("b".into()))),
        })
    });
    document
        .apply_replacements(
            replacements,
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(input.len()),
        )
        .unwrap();
    let elapsed = started.elapsed();
    let profile = document.storage_profile();
    assert_eq!(text(&document), "b ".repeat(256 * 1024));
    assert_eq!(profile.appended_edit_bytes, input.len());
    assert!(profile.piece_count <= input.len().div_ceil(ORIGINAL_PIECE_BYTES) + 1);
    assert!(profile.tree_height < 20);
    assert_eq!(profile.undo_entries, 1);
    eprintln!(
        "dense replace profile: bytes={} matches={} pieces={} height={} elapsed_us={}",
        input.len(),
        input.len() / 2,
        profile.piece_count,
        profile.tree_height,
        elapsed.as_micros()
    );
}

/// 匿名输入文件完全由当前 iterator 拥有；错误后必须随参数正常 drop，不进入文档或 history。
struct FailingInput {
    _file: std::fs::File,
    items: std::vec::IntoIter<Result<Replacement, MarkdownDocumentBufferError>>,
    dropped: Rc<Cell<usize>>,
}

impl Iterator for FailingInput {
    type Item = Result<Replacement, MarkdownDocumentBufferError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.items.next()
    }
}

impl Drop for FailingInput {
    fn drop(&mut self) {
        // 只记录测试所有权事实，文件由 std File 正常析构，不在 Drop 做产品 I/O。
        self.dropped.set(self.dropped.get() + 1);
    }
}

#[test]
fn failed_input_releases_its_owned_anonymous_descriptor_and_payload() {
    const CHILD_MARKER: &str = "OMNI_REPLACEMENT_INPUT_FD_TEST_CHILD";
    if std::env::var_os(CHILD_MARKER).is_none() {
        // 并行测试可立即复用 fd 整数；独立纯 core child 只运行本例，避免把别人的新文件当成泄漏。
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "markdown_document_buffer::replacements::tests::failed_input_releases_its_owned_anonymous_descriptor_and_payload",
                "--test-threads=1",
            ])
            .env(CHILD_MARKER, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "pure input-fd child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (_directory, mut document) = fixture("original", 384 * 1024);
    let snapshot = Snapshot::capture(&document);
    let file = tempfile::tempfile().unwrap();
    let descriptor = file.as_raw_fd();
    let dropped = Rc::new(Cell::new(0));
    let input = FailingInput {
        _file: file,
        items: vec![
            literal(0..1, &"x".repeat(70 * 1024)),
            Err(MarkdownDocumentBufferError::ReplacementInputUnavailable),
        ]
        .into_iter(),
        dropped: dropped.clone(),
    };
    let error = document
        .apply_replacements(
            input,
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(0),
        )
        .unwrap_err();
    assert_eq!(
        error,
        MarkdownDocumentBufferError::ReplacementInputUnavailable
    );
    assert_eq!(dropped.get(), 1);
    // SAFETY: F_GETFD 仅查询本测试此前拥有的整数 descriptor，不解引用地址或修改其他文件。
    assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF)
    );
    snapshot.verify(&document);
}

#[test]
fn borrowed_action_arena_is_not_retained_by_document_or_history() {
    // arena 属于调用栈，返回后立即 drop；canonical/history 必须已有自身的 chunk 字节。
    let (_directory, mut document) = fixture("abc", 4096);
    let arena = String::from("置😀");
    let replacement = MarkdownReplacement {
        range_utf16: 0..3,
        parts: std::iter::once(Ok(MarkdownReplacementPart::Literal(Cow::Borrowed(
            arena.as_str(),
        )))),
    };
    document
        .apply_replacements(
            std::iter::once(Ok(replacement)),
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(3),
        )
        .unwrap();
    drop(arena);
    assert_eq!(text(&document), "置😀");
    document.undo_text_edit().unwrap();
    assert_eq!(text(&document), "abc");
    document.redo_text_edit().unwrap();
    assert_eq!(text(&document), "置😀");
}

#[test]
fn borrowed_literal_then_failed_input_preserves_snapshot_and_redo() {
    // 借用并不降低失败原子性：已经拷贝的候选字节不能因下一 token 失败而残留。
    let (_directory, mut document) = fixture("abc def", 32768);
    seed_redo(&mut document, "redo");
    let before = Snapshot::capture(&document);
    let arena = "借".repeat(4096);
    let replacement = MarkdownReplacement {
        range_utf16: 0..1,
        parts: [
            Ok(MarkdownReplacementPart::Literal(Cow::Borrowed(
                arena.as_str(),
            ))),
            Err(MarkdownDocumentBufferError::ReplacementInputUnavailable),
        ],
    };
    assert_eq!(
        document.apply_replacements(
            std::iter::once(Ok(replacement)),
            document.selection(),
            MarkdownTextSelection::caret(0)
        ),
        Err(MarkdownDocumentBufferError::ReplacementInputUnavailable)
    );
    drop(arena);
    before.verify(&document);
}
