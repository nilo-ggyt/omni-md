//! 用真实候选、日志与历史验证组合分组；短合成oracle不进入产品存储。

use super::*;
use MarkdownTextInputComposition::{Commit, Start, Update};

mod heap_retirement;
mod retirement;

/// 每项只打开独占合成文件，正文和历史均走正式Core实现。
fn fixture(text: &str, maximum: u64) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("组合历史.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, maximum).unwrap();
    (directory, document)
}

/// 只读取短样本的独立期望，不参与产品选择或日志计算。
fn body(document: &MarkdownDocumentBuffer) -> Vec<u8> {
    assert!(document.byte_len() <= 128); // 只对短合成正文建立oracle，不让产品出现整篇读取路径。
    let mut output = Vec::new();
    document.write_to(&mut output).unwrap();
    output
}

#[derive(Default)]
struct Observe {
    changes: Vec<changes::MarkdownTextChange>,
    reject: bool,
}

impl MarkdownTextChangeObserver for Observe {
    type Error = &'static str;
    type Output = Vec<changes::MarkdownTextChange>;

    // 记录真实观察者回调，用于区分本次输入包络与历史净包络。
    fn changed(&mut self, change: changes::MarkdownTextChange) -> Result<(), Self::Error> {
        self.changes.push(change);
        Ok(())
    }

    fn finish(
        self,
        _: changes::MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<Self::Output, Self::Error> {
        if self.reject {
            return Err("原候选观察者拒绝"); // 必须在真实候选append之后拒绝，覆盖分组的准备回退。
        }
        Ok(self.changes)
    }
}

/// 准备和发布真实同步输入；检查局部通知但不重写生产历史规则。
fn replace(
    document: &mut MarkdownDocumentBuffer,
    range: Range<usize>,
    text: &str,
    after: MarkdownTextSelection,
    composition: MarkdownTextInputComposition,
) -> MarkdownHistoryTextEdit {
    let before = document.selection();
    let (token, changes) = document
        .prepare_composition_text_input_observed(
            range.clone(),
            text,
            before,
            after,
            composition,
            Observe::default(),
        )
        .unwrap();
    assert_eq!(
        changes,
        [changes::MarkdownTextChange {
            old_utf16_range: range,
            inserted_utf16_len: text.encode_utf16().count(),
        }]
    ); // 每次观察者看到真实局部输入，累计净日志只属于组合Undo/Redo。
    document.publish_text_input(&token).unwrap()
}

#[test]
/// 子范围替换保留原候选两侧正文，最终净Undo仍准确覆盖整个组合与两个真实方向选区。
fn partial_non_bmp_marked_updates_keep_one_real_net_journal_and_directed_history() {
    let (_directory, mut document) = fixture("甲原🙂尾", 4096);
    let before = MarkdownTextSelection {
        anchor_utf16: 4,
        head_utf16: 1,
    };
    document.set_selection(before).unwrap();
    replace(
        &mut document,
        1..4,
        "中🙂文",
        MarkdownTextSelection {
            anchor_utf16: 4,
            head_utf16: 2,
        },
        Start,
    );
    replace(
        &mut document,
        2..4,
        "汉😀",
        MarkdownTextSelection {
            anchor_utf16: 5,
            head_utf16: 2,
        },
        Update {
            marked_before: 1..5,
        },
    );
    assert_eq!(body(&document), "甲中汉😀文尾".as_bytes());
    let after = MarkdownTextSelection {
        anchor_utf16: 5,
        head_utf16: 2,
    };
    replace(
        &mut document,
        2..5,
        "好🙂",
        after,
        Commit {
            marked_before: 2..5,
        },
    );
    assert_eq!(body(&document), "甲中好🙂文尾".as_bytes());
    assert!(document.text_input_composition.is_none());
    document
        .set_selection(MarkdownTextSelection::caret(0))
        .unwrap();
    let (edit, changes) = document
        .undo_text_edit_observed(Observe::default())
        .unwrap()
        .unwrap();
    assert_eq!(edit.replaced_utf16_range, 1..6);
    assert_eq!(
        changes,
        [changes::MarkdownTextChange {
            old_utf16_range: 1..6,
            inserted_utf16_len: 3,
        }]
    );
    assert_eq!(body(&document), "甲原🙂尾".as_bytes());
    assert_eq!(document.selection(), before);
    assert!(document.undo_text_edit().is_none());
    let (_, changes) = document
        .redo_text_edit_observed(Observe::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        changes,
        [changes::MarkdownTextChange {
            old_utf16_range: 1..4,
            inserted_utf16_len: 5,
        }]
    );
    assert_eq!(body(&document), "甲中好🙂文尾".as_bytes());
    assert_eq!(document.selection(), after); // Redo恢复真实组合末端方向，不采用Undo前临时caret。
}

#[test]
/// 可失败观察与显式取消都不能提前改组；随后确认仍能一次撤销到首个组合前态。
fn observer_rejection_and_explicit_cancel_preserve_the_original_composition_group() {
    let (_directory, mut document) = fixture("首尾", 4096);
    document
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    replace(
        &mut document,
        1..1,
        "中🙂",
        MarkdownTextSelection::caret(4),
        Start,
    );
    let original_root = document.root.clone();
    let generation = document.generation();
    let selection = document.selection();
    let result = document.prepare_composition_text_input_observed(
        1..4,
        "改😀",
        selection,
        MarkdownTextSelection::caret(4),
        Update {
            marked_before: 1..4,
        },
        Observe {
            reject: true,
            ..Observe::default()
        },
    );
    assert!(matches!(
        result,
        Err(MarkdownObservedEditError::Observer("原候选观察者拒绝"))
    ));
    assert!(piece_tree_links_share_root(&document.root, &original_root));
    assert_eq!(document.generation(), generation);
    assert_eq!(document.selection(), selection);
    assert_eq!(
        document.text_input_composition.as_ref().unwrap().marked,
        1..4
    );
    let (token, _) = document
        .prepare_composition_text_input_observed(
            1..4,
            "改😀",
            selection,
            MarkdownTextSelection::caret(4),
            Update {
                marked_before: 1..4,
            },
            Observe::default(),
        )
        .unwrap();
    assert_eq!(body(&document), "首中🙂尾".as_bytes());
    document.cancel_prepared_text_input(&token).unwrap();
    assert!(!document.has_prepared_text_input());
    assert!(piece_tree_links_share_root(&document.root, &original_root));
    assert_eq!(document.generation(), generation);
    replace(
        &mut document,
        1..4,
        "确认",
        MarkdownTextSelection::caret(3),
        Commit {
            marked_before: 1..4,
        },
    );
    document.undo_text_edit().unwrap();
    assert_eq!(body(&document), "首尾".as_bytes());
    assert!(document.undo_text_edit().is_none()); // 两次失败都没有断开原组或制造中间Undo。
}

#[test]
/// 原映射真实回收失败优先返回EIO，不改已发布root或组合metadata，也保实际最终unmap。
fn source_eio_before_composition_update_keeps_the_complete_previously_published_state() {
    let (_directory, mut document) = fixture("原尾", 4096);
    document
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    replace(
        &mut document,
        1..1,
        "中🙂",
        MarkdownTextSelection::caret(4),
        Start,
    );
    let mapping = document.original_backing_observer().unwrap();
    let root = document.root.as_ref().map(Arc::as_ptr);
    let before = (
        document.generation(),
        document.selection(),
        document.undo.len(),
    );
    document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    document.fail_next_original_reclaim_for_test();
    assert!(document.reclaim_original_read_windows_for_test().is_err());
    assert!(matches!(
        document.prepare_composition_text_input_observed(
            1..4,
            "后继",
            before.1,
            MarkdownTextSelection::caret(3),
            Commit {
                marked_before: 1..4
            },
            Observe::default(),
        ),
        Err(MarkdownObservedEditError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert_eq!(document.root.as_ref().map(Arc::as_ptr), root);
    assert_eq!(
        (
            document.generation(),
            document.selection(),
            document.undo.len()
        ),
        before
    );
    assert_eq!(
        document.text_input_composition.as_ref().unwrap().marked,
        1..4
    );
    assert!(!document.has_prepared_text_input());
    drop(document);
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
}
