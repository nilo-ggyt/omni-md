//! 外层模板借用、匿名坐标记录与完整事务失败；不启动 NSApplication。
use super::*;

fn found(range: Range<usize>, captures: Vec<Option<Range<usize>>>) -> Found {
    Found {
        range,
        precise: true,
        captures,
    }
}

#[test]
fn anonymous_spool_streams_literals_and_source_ranges() {
    // 模板在 reader 外层存活；literal 明确 Borrowed，capture 只保留旧 snapshot 坐标。
    let template = ReplacementTemplate::parse("😀$1", true, 2);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(1..3, vec![Some(1..3), Some(10..12)]))
        .unwrap();
    assert_eq!(spool.count(), 1);
    assert_eq!(spool.inserted_utf16(), 4);
    let mut replacements = spool.into_replacements().unwrap();
    let replacement = replacements.next().unwrap().unwrap();
    assert_eq!(replacement.range_utf16, 1..3);
    let parts = replacement.parts.collect::<Result<Vec<_>, _>>().unwrap();
    assert!(matches!(
        &parts[0],
        MarkdownReplacementPart::Literal(Cow::Borrowed("😀"))
    ));
    assert_eq!(parts[1], MarkdownReplacementPart::SourceRange(10..12));
    assert!(replacements.next().is_none());
}

#[test]
fn unread_parts_cannot_be_reinterpreted_as_another_replacement() {
    // 尚有 literal token 时不得推进 outer iterator；同一 scratch 不能冒充独立的多记录快照。
    let template = ReplacementTemplate::parse("x", false, 0);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool.push(&found(0..1, vec![])).unwrap();
    spool.push(&found(2..3, vec![])).unwrap();
    let mut replacements = spool.into_replacements().unwrap();
    let first = replacements.next().unwrap().unwrap();
    assert!(matches!(
        replacements.next(),
        Some(Err(
            MarkdownDocumentBufferError::ReplacementInputUnavailable
        ))
    ));
    drop(first);
    assert!(replacements.next().is_none());
}

#[test]
fn selection_mapping_keeps_reversed_anchor_and_utf16_widths() {
    // 同一 capture token 的两个输入长度不同，映射仍按旧坐标累积，不交换反向 anchor/head。
    let template = ReplacementTemplate::parse("$1", true, 2);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(1..3, vec![Some(1..3), Some(10..11)]))
        .unwrap();
    spool
        .push(&found(5..6, vec![Some(5..6), Some(10..13)]))
        .unwrap();
    let selection = MarkdownTextSelection {
        anchor_utf16: 8,
        head_utf16: 2,
    };
    assert_eq!(
        spool.map_selection(selection).unwrap(),
        MarkdownTextSelection {
            anchor_utf16: 9,
            head_utf16: 2
        }
    );
    let ranges: Vec<_> = spool
        .into_replacements()
        .unwrap()
        .map(|replacement| {
            // map_selection 预读结束之后必须 rewind 到完整 header，不受 BufReader 底层偏移影响。
            let replacement = replacement.unwrap();
            let range = replacement.range_utf16;
            replacement.parts.collect::<Result<Vec<_>, _>>().unwrap();
            range
        })
        .collect();
    assert_eq!(ranges, [1..3, 5..6]);
}

#[test]
fn spool_capacity_and_missing_capture_reject_before_canonical_mutation() {
    // 不创建 256 MiB fixture，人工逼近声明上限；缺少模板要求的槽也不是合法的 undefined。
    let template = ReplacementTemplate::parse("x", false, 0);
    let mut capacity = ReplacementSpool::new(&template).unwrap();
    capacity.bytes = MAX_SPOOL_BYTES - 7;
    assert!(capacity.push(&found(0..1, vec![])).is_err());
    assert_eq!(capacity.count(), 0);
    let capture = ReplacementTemplate::parse("$1", true, 2);
    let mut incomplete = ReplacementSpool::new(&capture).unwrap();
    assert!(incomplete.push(&found(0..1, vec![])).is_err());
    assert_eq!(incomplete.count(), 0);
}

#[test]
fn truncated_second_record_rolls_back_canonical_text_and_redo() {
    // 第二个 header 的短读发生在首项已消费之后；core 仍须保留原文、选区、代次和 redo。
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Spool.md");
    std::fs::write(&path, "abc def").unwrap();
    let mut document = omni_markdown_core::MarkdownDocumentBuffer::open(&path, 4096).unwrap();
    document
        .replace_utf16_range(0..1, "Z", MarkdownTextSelection::caret(1))
        .unwrap();
    document.undo_text_edit().unwrap();
    let generation = document.generation();
    let selection = document.selection();
    let history = (
        document.can_undo(),
        document.can_redo(),
        document.is_dirty(),
    );
    let template = ReplacementTemplate::parse("replacement", false, 0);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool.push(&found(0..1, vec![])).unwrap();
    spool.push(&found(4..5, vec![])).unwrap();
    let stream = spool.into_replacements().unwrap();
    let bytes = stream
        .shared
        .state
        .borrow()
        .file
        .get_ref()
        .metadata()
        .unwrap()
        .len();
    stream
        .shared
        .state
        .borrow()
        .file
        .get_ref()
        .set_len(bytes - 1)
        .unwrap();
    assert!(
        document
            .apply_replacements(stream, selection, MarkdownTextSelection::caret(0))
            .is_err()
    );
    let mut actual = Vec::new();
    document.write_to(&mut actual).unwrap();
    assert_eq!(actual, b"abc def");
    assert_eq!(document.generation(), generation);
    assert_eq!(document.selection(), selection);
    assert_eq!(
        (
            document.can_undo(),
            document.can_redo(),
            document.is_dirty()
        ),
        history
    );
    document.redo_text_edit().unwrap();
    let mut redone = Vec::new();
    document.write_to(&mut redone).unwrap();
    assert_eq!(redone, b"Zbc def");
}

#[test]
fn anonymous_spool_replays_captures_as_one_reversible_edit() {
    // 两项的 $1 均引用同一旧 root，第一项提交准备不能改变第二项的 capture 正文。
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Capture.md");
    std::fs::write(&path, "A😀 B42").unwrap();
    let mut document = omni_markdown_core::MarkdownDocumentBuffer::open(&path, 4096).unwrap();
    let template = ReplacementTemplate::parse("$1", true, 2);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(0..3, vec![Some(0..3), Some(5..7)]))
        .unwrap();
    spool
        .push(&found(4..7, vec![Some(4..7), Some(1..3)]))
        .unwrap();
    // 原生Find必须传原完整反向memento，而非把查询有序范围重建成新的正向选择。
    let before = MarkdownTextSelection {
        anchor_utf16: 7,
        head_utf16: 1,
    };
    let after = spool.map_selection(before).unwrap();
    assert_eq!(
        after,
        MarkdownTextSelection {
            anchor_utf16: 5,
            head_utf16: 2,
        }
    );
    assert!(
        document
            .apply_replacements(spool.into_replacements().unwrap(), before, after)
            .unwrap()
            .is_some()
    );
    let text = |document: &omni_markdown_core::MarkdownDocumentBuffer| {
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        String::from_utf8(bytes).unwrap()
    };
    assert_eq!(text(&document), "42 😀");
    assert_eq!(document.selection(), after);
    document.undo_text_edit().unwrap();
    assert_eq!(text(&document), "A😀 B42");
    assert_eq!(document.selection(), before);
    document.redo_text_edit().unwrap();
    assert_eq!(text(&document), "42 😀");
    assert_eq!(document.selection(), after);
}

#[test]
fn repeated_capture_tokens_share_one_wire_slot_and_empty_captures_stay_empty() {
    // 重复 token 不重复写 capture；缺席分支与空范围具有不同模板结果。
    let template = ReplacementTemplate::parse("$1/$1/$2/$3", true, 4);
    assert_eq!(template.captures, [1, 2, 3]);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(0..3, vec![Some(0..3), Some(1..2), None, Some(3..3)]))
        .unwrap();
    assert_eq!(spool.bytes, 16 + 17 + 1 + 17);
    assert_eq!(spool.inserted_utf16(), 14);
    let mut records = spool.into_replacements().unwrap();
    let parts = records
        .next()
        .unwrap()
        .unwrap()
        .parts
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(parts.iter().filter(|part| matches!(part, MarkdownReplacementPart::SourceRange(range) if *range == (1..2))).count(), 2);
    assert!(parts.iter().any(|part| matches!(
        part,
        MarkdownReplacementPart::Literal(Cow::Borrowed("undefined"))
    )));
    assert!(parts.iter().any(
        |part| matches!(part, MarkdownReplacementPart::SourceRange(range) if *range == (3..3))
    ));
}

#[test]
fn dense_literal_record_profile_keeps_exact_wire_and_borrowed_counts() {
    // 与保存的旧 65k profile 用相同记录量；指标只代表该纯记录算法，不代替产品验收。
    const COUNT: usize = 65_536;
    let started = std::time::Instant::now();
    let template = ReplacementTemplate::parse("x", false, 0);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    for index in 0..COUNT {
        spool.push(&found(index..index + 1, vec![])).unwrap();
    }
    let wire_bytes = spool.bytes;
    let stream = spool.into_replacements().unwrap();
    let write_us = started.elapsed().as_micros();
    assert_eq!(wire_bytes, COUNT as u64 * 16);
    let started = std::time::Instant::now();
    let mut consumed = 0;
    let mut borrowed = 0;
    let mut owned = 0;
    for replacement in stream {
        let replacement = replacement.unwrap();
        assert_eq!(replacement.range_utf16, consumed..consumed + 1);
        for part in replacement.parts {
            match part.unwrap() {
                MarkdownReplacementPart::Literal(Cow::Borrowed("x")) => borrowed += 1, // 同一 arena 借用不产生每记录 String。
                MarkdownReplacementPart::Literal(Cow::Owned(_)) => owned += 1, // 显式计数，不能把 owned 路径默认为零。
                _ => panic!("literal fixture returned a different token"), // 任一模板/capture 误解都失败。
            }
        }
        consumed += 1;
    }
    let read_us = started.elapsed().as_micros();
    assert_eq!(consumed, COUNT);
    assert_eq!(borrowed, COUNT);
    assert_eq!(owned, 0);
    eprintln!(
        "dense_spool_profile records={consumed} wire_bytes={wire_bytes} borrowed_literal_records={borrowed} owned_literal_records={owned} write_us={write_us} read_us={read_us}"
    );
}

#[test]
fn corrupted_capture_tag_cannot_be_accepted_as_a_missing_capture() {
    // wire 的未定义 tag 不是合法的 nil；不重同步、不忽略损坏，也不创建 replacement。
    let template = ReplacementTemplate::parse("$1", true, 2);
    let mut spool = ReplacementSpool::new(&template).unwrap();
    spool
        .push(&found(0..1, vec![Some(0..1), Some(0..1)]))
        .unwrap();
    spool.file.seek(SeekFrom::Start(16)).unwrap();
    spool.file.write_all(&[2]).unwrap();
    let mut records = spool.into_replacements().unwrap();
    assert!(matches!(
        records.next(),
        Some(Err(
            MarkdownDocumentBufferError::ReplacementInputUnavailable
        ))
    ));
    assert!(records.next().is_none());
}
