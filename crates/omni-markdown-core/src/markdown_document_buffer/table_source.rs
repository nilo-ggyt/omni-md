//! S2结构producer的真实piece-tree字节借用；不构造全文/整表String或强持第二份源图。

use std::sync::Weak;

use super::*;
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableUnderline, MarkdownTableWindowError,
    context::{MarkdownTableContextFrame, MarkdownTableUnderlineContext},
};

#[cfg(test)]
use crate::markdown_viewport_presentation::table_window::references::MarkdownTableReferenceQueries;

const MAX_TABLE_SOURCE_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MarkdownTableUnderlineRequestStep {
    pub source_bytes: usize,
    pub utf8_validation_bytes: usize,
    pub source_tree_steps: usize,
    pub ready: bool,
}

#[derive(Debug)]
pub(crate) struct MarkdownTableUnderlineRequest {
    source: MarkdownTableSourceIdentity,
    context: Option<MarkdownTableUnderlineContext>,
    pairs: Vec<MarkdownTableUnderline>,
    ready: bool,
}

impl MarkdownTableUnderlineRequest {
    /// 每step只借一片源并同步计量UTF16；完成时移交有限metadata，无逐pair源前缀查询。
    pub(crate) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, MarkdownTableWindowError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        if budget == 0 || budget > MAX_TABLE_SOURCE_FRAME_BYTES {
            return Err(MarkdownTableWindowError::SourceLimit);
        }
        if self.ready {
            return Ok(MarkdownTableUnderlineRequestStep {
                source_bytes: 0,
                utf8_validation_bytes: 0,
                source_tree_steps: 0,
                ready: true,
            });
        }
        let context = self
            .context
            .as_mut()
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        if let Some(pairs) = context.take_ready_utf16_pairs() {
            // 配对时已按原源frame同步计量UTF16；这里只有Vec所有权转移，没有隐藏的N次leaf解码。
            self.pairs = pairs;
            let previous = self.context.take();
            drop(previous);
            self.ready = true;
            return Ok(MarkdownTableUnderlineRequestStep {
                source_bytes: 0,
                utf8_validation_bytes: 0,
                source_tree_steps: 0,
                ready: true,
            });
        }
        let frame = document
            .table_source_frame(context.next_byte(), budget)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let step = context.advance(
            MarkdownTableContextFrame {
                generation: frame.generation,
                byte_start: frame.byte_start,
                bytes: &frame.bytes,
                real_eof: frame.real_eof,
            },
            budget,
        );
        // 回收成功后才消费step，不能让原parser提前终止遮住真实映射错误。
        frame
            .finish()
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let step = step?;
        Ok(MarkdownTableUnderlineRequestStep {
            source_bytes: step.consumed,
            utf8_validation_bytes: step.utf8_validation_bytes,
            // 每次frame借用都会沿真实AVL路径找leaf；这项与scalar解码字节分开计费。
            source_tree_steps: document
                .root
                .as_ref()
                .map_or(0, |root| usize::from(root.height())),
            ready: false,
        })
    }

    /// 读取完成值仍核同root/代次；旧请求即使已经Ready也不能服务切换后的另一个note。
    pub(crate) fn ready_pairs_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&[MarkdownTableUnderline]>, MarkdownTableWindowError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok(self.ready.then_some(self.pairs.as_slice()))
    }
}

pub(crate) struct BorrowedMarkdownTableSourceFrame<'a> {
    pub generation: u64,
    pub byte_start: usize,
    pub(super) bytes: PieceBytes<'a>,
    pub real_eof: bool,
}

impl BorrowedMarkdownTableSourceFrame<'_> {
    /// 消费者必须在发布任何成功step或Ready之前完成真实窗口回收；失败不得继续旧票。
    pub(crate) fn finish(self) -> Result<(), MarkdownDocumentBufferError> {
        self.bytes.finish()
    }
}

impl std::fmt::Debug for BorrowedMarkdownTableSourceFrame<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只输出结构坐标，避免把借用的个人正文写入默认日志。
        formatter
            .debug_struct("BorrowedMarkdownTableSourceFrame")
            .field("generation", &self.generation)
            .field("byte_start", &self.byte_start)
            .field("byte_count", &self.bytes.len())
            .field("real_eof", &self.real_eof)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct MarkdownTableSourceIdentity {
    generation: u64,
    byte_len: usize,
    root: Weak<MarkdownPieceTreeNode>,
}

impl std::fmt::Debug for MarkdownTableSourceIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MarkdownTableSourceIdentity")
            .field("generation", &self.generation)
            .field("byte_len", &self.byte_len)
            .finish_non_exhaustive()
    }
}

impl MarkdownTableSourceIdentity {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    pub(super) fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// root Weak保持allocation身份，generation拒绝Undo回到相同root时擅自复活旧状态。
    pub(crate) fn is_current(&self, document: &MarkdownDocumentBuffer) -> bool {
        document.original_backing_health().is_ok()
            && self.generation == document.generation
            && self.byte_len == document.byte_len()
            && document
                .root
                .as_ref()
                .is_some_and(|root| Weak::ptr_eq(&self.root, &Arc::downgrade(root)))
    }
}

impl MarkdownDocumentBuffer {
    /// 先凭真实scanner的ASCII计数排除无关definition，再复制并规范化有限完整label。
    /// candidate的真实block/scope身份仍由global producer负责，不能用本结果倒推成员权限。
    #[cfg(test)]
    pub(crate) fn table_reference_candidate_label(
        &self,
        source: &MarkdownTableSourceIdentity,
        candidate: &pulldown_cmark::reference_frontier::ReferenceCandidate,
        queries: &MarkdownTableReferenceQueries,
        continuation_prefix: impl Fn(&[u8]) -> Option<usize>,
    ) -> Result<Option<String>, MarkdownTableWindowError> {
        if !source.is_current(self) || candidate.source_revision != self.generation() {
            // 同generation的不同note也必须由捕获root Weak拒绝，不能只比对u64。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        if !queries.may_match_ascii_count(candidate.label_ordinary_ascii) {
            // 巨大普通ASCII标签在此零正文复制返回；不读取它的URL/title或构造全局label表。
            return Ok(None);
        }
        let end = candidate
            .label_end
            .checked_add(1)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let raw = self
            .copy_table_source_range(
                candidate.label_start..end,
                queries.maximum_candidate_label_bytes(),
            )
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let normalized = candidate
            .normalized_label(&raw, continuation_prefix)
            .map_err(|_| MarkdownTableWindowError::ParserMismatch)?;
        if !queries.needs_definition(&normalized)? {
            // cheap排除只是必要条件；最后仍使用原label规范化及固定UniCase比较。
            return Ok(None);
        }
        Ok(Some(normalized.into_string()))
    }

    /// 只复制已经选中的有限完整源range。Vec直接移交String，不再复制第二次正文。
    pub(crate) fn copy_table_source_range(
        &self,
        byte_range: Range<usize>,
        maximum_bytes: usize,
    ) -> Result<String, MarkdownDocumentBufferError> {
        let Some(length) = byte_range.end.checked_sub(byte_range.start) else {
            // 倒序range不能利用饱和减法变成空请求，再交给后续定义解析获得错误权限。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        };
        if maximum_bytes > MAX_TABLE_SOURCE_FRAME_BYTES
            || length > maximum_bytes
            || byte_range.end > self.byte_len()
        {
            // 分配前限制的是本批实际正文，不以总源长度或此前扫描过的字节数授权超量复制。
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        let mut bytes = Vec::with_capacity(length);
        let mut next = byte_range.start;
        while next < byte_range.end {
            // 一个range最多64KiB，所有片段复制量之和恰为length；不循环扫描无关piece或源尾。
            let frame = self.table_source_frame(next, byte_range.end - next)?;
            if frame.bytes.is_empty() {
                // 非空range没有取得真实字节表示源/坐标不一致，不忙重试或返回部分字符串。
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            bytes.extend_from_slice(&frame.bytes);
            next += frame.bytes.len();
            frame.finish()?;
        }
        String::from_utf8(bytes).map_err(|_| MarkdownDocumentBufferError::RangeOutsideDocument)
    }

    /// 只有当前非空piece root创建请求；该请求不强持document、映射、history或源帧。
    pub(crate) fn table_underline_request(
        &self,
        byte_coverage: &[Range<usize>],
    ) -> Result<MarkdownTableUnderlineRequest, MarkdownTableWindowError> {
        Ok(MarkdownTableUnderlineRequest {
            source: self
                .table_source_identity()
                .ok_or(MarkdownTableWindowError::InvalidSource)?,
            context: Some(MarkdownTableUnderlineContext::new(
                self.generation(),
                self.byte_len(),
                byte_coverage,
            )?),
            pairs: Vec::new(),
            ready: false,
        })
    }
    /// 空源没有表，无需创建可外带的cursor。非空请求仅持root Weak，不延长映射/编辑段生命。
    pub(crate) fn table_source_identity(&self) -> Option<MarkdownTableSourceIdentity> {
        self.root.as_ref().map(|root| MarkdownTableSourceIdentity {
            generation: self.generation,
            byte_len: self.byte_len(),
            root: Arc::downgrade(root),
        })
    }

    /// 借出包含指定byte的真实4KiB逻辑片；允许frame切在UTF8内部，由结构scanner续接。
    /// 返回借用不能越过document寿命，调用者须在任何可重入native操作前结束该借用。
    pub(crate) fn table_source_frame(
        &self,
        byte_start: usize,
        maximum_bytes: usize,
    ) -> Result<BorrowedMarkdownTableSourceFrame<'_>, MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if maximum_bytes == 0 || maximum_bytes > MAX_TABLE_SOURCE_FRAME_BYTES {
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        let source_end = self.byte_len();
        if byte_start > source_end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if byte_start == source_end {
            return Ok(BorrowedMarkdownTableSourceFrame {
                generation: self.generation,
                byte_start,
                bytes: PieceBytes::Edited(&[]),
                real_eof: true,
            });
        }
        let mut node = self
            .root
            .as_deref()
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        let mut local = byte_start;
        loop {
            match node {
                MarkdownPieceTreeNode::Branch { left, right, .. } => {
                    let left_len = left.metrics().byte_len;
                    if local < left_len {
                        node = left.as_ref();
                    } else {
                        local -= left_len;
                        node = right.as_ref();
                    }
                }
                MarkdownPieceTreeNode::OriginalSpan(span) => {
                    let hit = span.locate_byte(local, false)?;
                    let end = hit
                        .local
                        .saturating_add(maximum_bytes)
                        .min(hit.piece.byte_len);
                    let bytes = self.piece_range_bytes(&hit.piece, hit.local..end)?;
                    let real_eof = byte_start + bytes.len() == source_end;
                    return Ok(BorrowedMarkdownTableSourceFrame {
                        generation: self.generation,
                        byte_start,
                        bytes,
                        real_eof,
                    });
                }
                MarkdownPieceTreeNode::Leaf(piece) => {
                    let end = local.saturating_add(maximum_bytes).min(piece.byte_len);
                    let bytes = self.piece_range_bytes(piece, local..end)?;
                    // 先从仍活lease读取有限长度事实，再移交唯一owner；不Clone或提前Drop来绕过借用。
                    let real_eof = byte_start + bytes.len() == source_end;
                    return Ok(BorrowedMarkdownTableSourceFrame {
                        generation: self.generation,
                        byte_start,
                        bytes,
                        real_eof,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulldown_cmark::block_frontier::SourceFrame;
    use pulldown_cmark::reference_frontier::{
        ReferenceCandidate, ReferenceDefinitionScanner, ReferenceStep,
    };

    fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("S2-owned-source.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    fn reference(document: &MarkdownDocumentBuffer, maximum: usize) -> ReferenceCandidate {
        let identity = document.table_source_identity().unwrap();
        let mut scanner =
            ReferenceDefinitionScanner::new(document.generation(), document.byte_len(), 0).unwrap();
        loop {
            assert!(identity.is_current(document));
            let frame = document
                .table_source_frame(scanner.next_byte(), maximum)
                .unwrap();
            let step = scanner
                .advance(
                    SourceFrame {
                        source_revision: frame.generation,
                        byte_start: frame.byte_start,
                        bytes: &frame.bytes,
                        real_eof: frame.real_eof,
                    },
                    maximum,
                )
                .unwrap();
            frame.finish().unwrap();
            match step {
                ReferenceStep::Candidate { candidate, .. } => return candidate,
                ReferenceStep::Progress { consumed } | ReferenceStep::NeedInput { consumed } => {
                    assert!(consumed > 0);
                }
                _ => panic!("本fixture为同一真实行内的完整定义，不需要容器续行或fallback"),
            }
        }
    }

    #[test]
    fn source_frames_borrow_exact_original_and_edited_piece_bytes_at_every_byte() {
        let original = "中😀e\u{301}\r\nsource".repeat(1000);
        let (_directory, mut document) = fixture(&original);
        document
            .insert_utf16(1, "edited😀", MarkdownTextSelection::caret(9))
            .unwrap();
        let expected = format!("中edited😀{}", &original["中".len()..]);
        for start in 0..=expected.len() {
            let frame = document.table_source_frame(start, 3).unwrap();
            assert_eq!(
                &*frame.bytes,
                &expected.as_bytes()[start..start + frame.bytes.len()]
            );
            assert!(frame.bytes.len() <= 3);
            assert_eq!(frame.real_eof, start + frame.bytes.len() == expected.len());
            frame.finish().unwrap();
        }
    }

    #[test]
    fn reference_source_adapter_uses_real_piece_leaves_and_preserves_edit_undo_redo_bytes() {
        let source = "[r]: <https://example.test/😀> \"Title中\"\r\nrest\n";
        let (_directory, mut document) = fixture(source);
        for maximum in 1..=source.len() {
            let candidate = reference(&document, maximum);
            assert_eq!(
                &source[candidate.destination_start..candidate.destination_end],
                "https://example.test/😀"
            );
            assert_eq!(candidate.span_end, source.find("\r\n").unwrap());
        }
        let old = document.table_source_identity().unwrap();
        document
            .replace_utf16_range(1..2, "new", MarkdownTextSelection::caret(4))
            .unwrap();
        assert!(!old.is_current(&document));
        assert_eq!(reference(&document, 3).label_end, 4);
        document.undo().unwrap();
        assert!(
            !old.is_current(&document),
            "同root的Undo仍推进generation，旧cursor不能自动复活"
        );
        assert_eq!(reference(&document, 3).label_end, 2);
        document.redo().unwrap();
        assert_eq!(reference(&document, 3).label_end, 4);
    }

    #[test]
    fn finite_reference_label_copy_uses_piece_source_and_rejects_stale_root() {
        let (_directory, mut document) = fixture("[Maße]: <https://x.test/中> '标题'\n");
        let identity = document.table_source_identity().unwrap();
        let candidate = reference(&document, 1);
        let queries = MarkdownTableReferenceQueries::from_projection("[x][MASSE]", true).unwrap();
        assert_eq!(
            document
                .table_reference_candidate_label(&identity, &candidate, &queries, |_| Some(0))
                .unwrap(),
            Some("Maße".to_owned())
        );
        let (_other_directory, other) = fixture("[Maße]: /other\n");
        assert_eq!(
            other.table_reference_candidate_label(&identity, &candidate, &queries, |_| Some(0)),
            Err(MarkdownTableWindowError::InvalidSource)
        );
        document
            .insert_utf16(0, "changed", MarkdownTextSelection::caret(7))
            .unwrap();
        assert_eq!(
            document.table_reference_candidate_label(&identity, &candidate, &queries, |_| Some(0)),
            Err(MarkdownTableWindowError::InvalidSource)
        );
    }

    #[test]
    fn source_range_copy_is_bounded_and_rejects_partial_utf8() {
        let (_directory, document) = fixture("中😀tail");
        assert_eq!(document.copy_table_source_range(0..7, 7).unwrap(), "中😀");
        assert!(document.copy_table_source_range(0..7, 6).is_err());
        assert!(document.copy_table_source_range(1..3, 2).is_err());
        let invalid = Range { start: 4, end: 3 };
        assert!(document.copy_table_source_range(invalid, 16).is_err());
        assert!(document.copy_table_source_range(0..20, 20).is_err());
    }

    #[test]
    fn same_generation_different_document_and_accepted_drop_cannot_reuse_source_identity() {
        let (_first_dir, first) = fixture("same bytes");
        let (_second_dir, second) = fixture("same bytes");
        let identity = first.table_source_identity().unwrap();
        assert!(!identity.is_current(&second));
        assert!(identity.root.strong_count() > 0);
        drop(first);
        assert_eq!(
            identity.root.strong_count(),
            0,
            "源身份不能强持原root或document payload"
        );
        let (_empty_dir, empty) = fixture("");
        assert!(empty.table_source_identity().is_none());
        assert!(empty.table_source_frame(0, 1).unwrap().real_eof);
    }

    #[test]
    fn real_source_context_maps_byte_pairs_to_utf16_without_reading_huge_content_as_string() {
        let mut source = String::from("😀<u>");
        source.extend(std::iter::repeat_n('中', 400_000));
        source.push_str("</u>tail");
        let (_directory, mut document) = fixture(&source);
        let coverage = 4..10;
        let mut request = document
            .table_underline_request(std::slice::from_ref(&coverage))
            .unwrap();
        while request.ready_pairs_for(&document).unwrap().is_none() {
            let step = request.advance(&document, 4096).unwrap();
            assert!(step.ready || step.source_bytes > 0 || step.utf8_validation_bytes > 0);
            assert!(step.source_bytes <= 4096);
            assert!(step.utf8_validation_bytes <= step.source_bytes * 4);
        }
        assert_eq!(
            request.ready_pairs_for(&document).unwrap().unwrap(),
            &[MarkdownTableUnderline {
                open: 2..5,
                content: 5..400_005,
                close: 400_005..400_009
            }]
        );
        assert!(
            request.context.is_none(),
            "最终UTF16值就绪后不保byte中间集合"
        );
        document
            .insert_utf16(0, "x", MarkdownTextSelection::caret(1))
            .unwrap();
        assert_eq!(
            request.ready_pairs_for(&document),
            Err(MarkdownTableWindowError::InvalidSource)
        );
        assert_eq!(
            request.advance(&document, 1),
            Err(MarkdownTableWindowError::InvalidSource)
        );
    }

    #[test]
    fn dense_tags_in_large_edit_leaf_do_not_trigger_per_pair_prefix_decoding() {
        let (_directory, mut document) = fixture("seed");
        // 单次大编辑产生真实edit leaf；旧实现每64pair会重复解码这个leaf的长前缀。
        let source = format!("{}{}", "中".repeat(18_000), "<u>😀</u>".repeat(128));
        document
            .replace_utf16_range(0..4, &source, MarkdownTextSelection::caret(0))
            .unwrap();
        let coverage = 0..document.byte_len();
        let mut request = document
            .table_underline_request(std::slice::from_ref(&coverage))
            .unwrap();
        let mut reads = 0;
        let mut validations = 0;
        let mut tree_steps = 0;
        loop {
            let step = request.advance(&document, 4096).unwrap();
            reads += step.source_bytes;
            validations += step.utf8_validation_bytes;
            tree_steps += step.source_tree_steps;
            assert!(step.source_bytes <= 4096);
            assert!(step.utf8_validation_bytes <= 4 * step.source_bytes);
            if step.ready {
                break;
            }
        }
        // 所有byte只扫一次；UTF8校验至多常数次，不随着pair数重复扫描leaf前缀。
        assert_eq!(reads, source.len());
        assert!(validations <= source.len() * 4);
        assert!(
            tree_steps
                <= (source.len().div_ceil(4096) + 2)
                    * usize::from(document.root.as_ref().unwrap().height())
        );
        assert_eq!(
            request.ready_pairs_for(&document).unwrap().unwrap().len(),
            128
        );
    }
}
