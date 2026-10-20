//! 只有原Doc会话能建立cell布局Source；局部范围不升级为文档全局范围。

use super::*;
use crate::markdown_document_buffer::{
    MarkdownCellDraft, MarkdownCellDraftError, MarkdownCellDraftSourceAuthority,
};

impl MarkdownLayoutTextSource {
    /// 捕获同一cell正文root与内容版本；selection/unmark不撤销已有相同正文Source。
    /// 不返回raw Snapshot，不接受公开Range，普通canonical capture也不能补出draft身份。
    pub fn capture_cell_draft(
        document: &MarkdownDocumentBuffer,
        token: &MarkdownCellDraft,
    ) -> Result<Self, MarkdownCellDraftError> {
        let capsule = document.capture_cell_draft(token)?;
        let authority = capsule.authority();
        let facts = capsule.facts(&authority)?;
        let snapshot = capsule.layout_snapshot(&authority)?;
        let draft = capsule.source_authority(&authority)?;
        let source = Self(Arc::new(Payload {
            snapshot,
            generation: facts.content_serial,
            bytes: facts.byte_len,
            utf16: facts.utf16_len,
            domain: SourceDomain::ActiveCellDraft(draft),
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        }));
        // 原capture期间的最后门仍属于同nonce；失败时Source和临时capsule按真实Arc寿命归还。
        capsule.facts(&authority)?;
        Ok(source)
    }

    #[must_use]
    /// Some 仅标识原活动草稿；None 也可能为只读格，完整坐标域必须查询 domain。
    pub fn cell_draft_authority(&self) -> Option<MarkdownCellDraftSourceAuthority> {
        match &self.0.domain {
            SourceDomain::ActiveCellDraft(authority) => Some(authority.clone()),
            SourceDomain::Canonical | SourceDomain::ReadOnlyCell(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_document_buffer::{
        MarkdownCellDraftHistoryDirection, MarkdownTableWindowStep, MarkdownTableWindowTarget,
        MarkdownTextSelection,
    };

    /// 夹具保留真实固定文件和原映射；只有测试的小样本预期值允许构造完整字符串。
    fn document(cell: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("streamed-cell.md");
        std::fs::write(&path, format!("| H | C |\n| --- | --- |\n| {cell} | b |\n")).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    /// 从原表格请求取得真实认证窗口；不能用手造范围绕过草稿的原行列归属。
    fn begin(document: &mut MarkdownDocumentBuffer) -> MarkdownCellDraft {
        let mut request = document
            .table_window_request(
                MarkdownTableWindowTarget::AtUtf16(0),
                0..document.utf16_len(),
                1,
                0..0,
            )
            .unwrap();
        // 沿原请求逐步推进；有限测试预算耗尽表示夹具认证失败，不能换一份来源继续。
        for _ in 0..document.byte_len().saturating_mul(64) + 10000 {
            match request.advance(document, false, 4096).unwrap() {
                MarkdownTableWindowStep::Progress { .. } => {
                    // 当前原请求尚未就绪，下一步仍消费它自己的进度与原Doc。
                }
                MarkdownTableWindowStep::Ready { .. } => {
                    // 只有原请求交付的当前窗口才能建立首个数据行的单元格草稿。
                    let window = request.take_ready_for(document).unwrap().unwrap();
                    return document.begin_cell_draft(&window, 1, 0).unwrap();
                }
                other => panic!("真实单元格未认证: {other:?}"),
            }
        }
        panic!("有限单元格认证请求未结束");
    }

    #[test]
    fn streamed_draft_range_crosses_original_and_edit_pieces_without_changing_either_history() {
        // 原cell本身跨多个4KiB片；插入片同时带中文、代理对和必须原样保留的CR/LF。
        let original = format!("{}😀{}尾", "a".repeat(4093), "b".repeat(4096 + 29));
        let (_directory, mut document) = document(&original);
        let token = begin(&mut document);
        let inserted = "草稿😀\r\n";
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        document
            .replace_cell_draft(
                edit,
                4093..4093,
                inserted,
                MarkdownTextSelection::caret(4093 + inserted.encode_utf16().count()),
                None,
            )
            .unwrap();
        let source = MarkdownLayoutTextSource::capture_cell_draft(&document, &token).unwrap();
        let expected = format!("{}{inserted}{}", &original[..4093], &original[4093..]);
        let range = 4088..8200;
        let units = expected.encode_utf16().collect::<Vec<_>>();
        let selected = String::from_utf16(&units[range.clone()]).unwrap();
        let revision = document.current_revision().unwrap();
        let generation = document.generation();
        let canonical_selection = document.selection();
        let profile = document.storage_profile();
        let facts = document.cell_draft_facts(&token).unwrap();
        let history = document.cell_draft_history_facts(&token).unwrap();
        let mut destination = Vec::new();
        source
            .write_utf16_range_to(range, &mut destination)
            .unwrap();
        assert_eq!(destination, selected.as_bytes());
        assert!(selected.contains("草稿😀\r\n😀"));
        assert!(destination.len() > MARKDOWN_LAYOUT_SOURCE_BYTES);
        assert_eq!(document.current_revision().unwrap(), revision);
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), canonical_selection);
        assert_eq!(document.storage_profile(), profile);
        assert_eq!(document.cell_draft_facts(&token).unwrap(), facts);
        assert_eq!(document.cell_draft_history_facts(&token).unwrap(), history);
    }

    #[test]
    fn invalid_cell_ranges_and_surrogate_halves_never_touch_existing_output() {
        let (_directory, mut document) = document("a😀尾");
        let token = begin(&mut document);
        let source = MarkdownLayoutTextSource::capture_cell_draft(&document, &token).unwrap();
        // 首尾都要先通过同一Source的标量校验；空范围也不能接受半个代理对。
        for (range, expected) in [
            (2..3, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (0..2, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (2..2, MarkdownDocumentBufferError::NotUnicodeBoundary),
            (
                Range { start: 3, end: 1 },
                MarkdownDocumentBufferError::RangeOutsideDocument,
            ),
            (0..5, MarkdownDocumentBufferError::RangeOutsideDocument),
        ] {
            let mut destination = "已有输出".as_bytes().to_vec();
            assert_eq!(
                source.write_utf16_range_to(range, &mut destination),
                Err(expected)
            );
            assert_eq!(destination, "已有输出".as_bytes());
        }
    }

    #[test]
    fn real_cell_edit_revokes_old_source_before_empty_or_nonempty_output() {
        let (_directory, mut document) = document("old😀");
        let token = begin(&mut document);
        let stale = MarkdownLayoutTextSource::capture_cell_draft(&document, &token).unwrap();
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        document
            .replace_cell_draft(edit, 0..3, "new", MarkdownTextSelection::caret(3), None)
            .unwrap();
        // 即使原字符串仍由冻结payload保活，旧内容版本也不得再次输出或借空范围补签。
        for range in [0..0, 0..stale.utf16_len()] {
            let mut destination = b"prefix".to_vec();
            assert_eq!(
                stale.write_utf16_range_to(range, &mut destination),
                Err(MarkdownDocumentBufferError::PreparedTextInputStale)
            );
            assert_eq!(destination, b"prefix");
        }
        let current = MarkdownLayoutTextSource::capture_cell_draft(&document, &token).unwrap();
        let mut destination = Vec::new();
        current
            .write_utf16_range_to(0..current.utf16_len(), &mut destination)
            .unwrap();
        assert_eq!(destination, "new😀".as_bytes());
    }

    #[test]
    fn selection_and_unmark_keep_same_content_source_streamable() {
        let (_directory, mut document) = document("a");
        let token = begin(&mut document);
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        document
            .replace_cell_draft(
                edit,
                0..1,
                "中😀",
                MarkdownTextSelection::caret(3),
                Some(0..3),
            )
            .unwrap();
        let source = MarkdownLayoutTextSource::capture_cell_draft(&document, &token).unwrap();
        let authority = source.cell_draft_authority().unwrap();
        let before = document.cell_draft_facts(&token).unwrap();
        let history = document.cell_draft_history_facts(&token).unwrap();
        // 先结束原marked事实再移动选区；两步真实推进输入serial，但不能撤同正文Source。
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        document.unmark_cell_draft(edit).unwrap();
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        let selected = document
            .set_cell_draft_selection(
                edit,
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: 3,
                },
            )
            .unwrap();
        assert!(selected.serial > before.serial);
        assert_eq!(selected.content_serial, before.content_serial);
        assert!(selected.marked_range.is_none());
        assert!(source.cell_draft_authority().unwrap() == authority);
        let mut destination = Vec::new();
        source
            .write_utf16_range_to(0..source.utf16_len(), &mut destination)
            .unwrap();
        assert_eq!(destination, "中😀".as_bytes());
        assert_eq!(document.cell_draft_history_facts(&token).unwrap(), history);
        assert!(!document.can_undo() && !document.can_redo());
    }

    #[test]
    fn readonly_fd_failure_preserves_real_doc_and_cell_undo_redo() {
        let (_directory, mut document) = document("原文😀");
        // 先在canonical建立两次真实编辑，再Undo一次，使两边历史都实际有内容。
        for text in ["第一段\n", "第二段\n"] {
            let at = document.utf16_len();
            document
                .insert_utf16(
                    at,
                    text,
                    MarkdownTextSelection::caret(at + text.encode_utf16().count()),
                )
                .unwrap();
        }
        document.undo_text_edit().unwrap();
        assert!(document.can_undo() && document.can_redo());
        let token = begin(&mut document);
        // 草稿也保有可Undo与Redo的真实root，避免只断言空栈未发生变化。
        for text in ["草稿😀", "再次输入"] {
            let length = document.cell_draft_facts(&token).unwrap().utf16_len;
            let edit = document.capture_cell_draft_edit(&token).unwrap();
            document
                .replace_cell_draft(
                    edit,
                    0..length,
                    text,
                    MarkdownTextSelection::caret(text.encode_utf16().count()),
                    None,
                )
                .unwrap();
        }
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        document
            .restore_cell_draft_history(edit, MarkdownCellDraftHistoryDirection::Undo)
            .unwrap()
            .unwrap();
        let history = document.cell_draft_history_facts(&token).unwrap();
        assert!(history.can_undo && history.can_redo);
        let source = MarkdownLayoutTextSource::capture_cell_draft(&document, &token).unwrap();
        let revision = document.current_revision().unwrap();
        let generation = document.generation();
        let selection = document.selection();
        let profile = document.storage_profile();
        let facts = document.cell_draft_facts(&token).unwrap();
        // 只读/dev/null是真实OS写入失败，Source输出不能借失败修改Doc或原草稿。
        let mut destination = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(
            source.write_utf16_range_to(0..source.utf16_len(), &mut destination),
            Err(MarkdownDocumentBufferError::WriteUnavailable)
        );
        assert_eq!(document.current_revision().unwrap(), revision);
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert_eq!(document.storage_profile(), profile);
        assert_eq!(document.cell_draft_facts(&token).unwrap(), facts);
        assert_eq!(document.cell_draft_history_facts(&token).unwrap(), history);
        assert!(document.can_undo() && document.can_redo());
        let mut bytes = Vec::new();
        source
            .write_utf16_range_to(0..source.utf16_len(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, "草稿😀".as_bytes());
    }

    #[test]
    fn actual_source_errno_precedes_revoked_cell_and_destination_failure() {
        let (_directory, mut document) = document("a");
        let token = begin(&mut document);
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        document
            .replace_cell_draft(
                edit,
                0..1,
                "纯编辑😀",
                MarkdownTextSelection::caret(5),
                None,
            )
            .unwrap();
        let stale = MarkdownLayoutTextSource::capture_cell_draft(&document, &token).unwrap();
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        document
            .replace_cell_draft(edit, 0..5, "新内容", MarkdownTextSelection::caret(3), None)
            .unwrap();
        // 即使旧Source只含编辑片，也须保留原Doc的健康门；实际回收失败才建立永久EIO。
        let snapshot = document.text_snapshot();
        snapshot.visit_byte_range(0..1, |_| Ok(())).unwrap();
        document.fail_next_original_reclaim_for_test();
        let error = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
        assert_eq!(
            document.reclaim_original_read_windows_for_test(),
            Err(error)
        );
        for range in [0..0, 0..stale.utf16_len()] {
            let mut destination = b"prefix".to_vec();
            assert_eq!(
                stale.write_utf16_range_to(range, &mut destination),
                Err(error)
            );
            assert_eq!(destination, b"prefix");
        }
        // 目标同时不可写，仍应返回原映射首因，不能被Stale或目标WriteUnavailable洗掉。
        let mut readonly = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(
            stale.write_utf16_range_to(0..stale.utf16_len(), &mut readonly),
            Err(error)
        );
    }
}
