//! 真实表尾的末格提交与追加行共用一次canonical替换，不以可见窗口末端猜测整表结束。

use std::ops::Range;

use super::{
    MarkdownDocumentBuffer, MarkdownDocumentBufferError,
    table_cell_edit::{MarkdownTableCellEditPlan, TableCellEditContext},
    table_source::MarkdownTableSourceIdentity,
    table_window_request::{VerifiedMarkdownTableViewport, VerifiedMarkdownTableWindow},
    visit_piece_tree_range,
};
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowError, TABLE_WINDOW_MAX_BYTES,
};

const MAX_APPEND_SOURCE_BYTES: usize = 64 * 1024;

/// 只持有限替换正文、下一格绝对坐标和Weak源身份；不保窗口、表格AST或原文强引用。
pub struct MarkdownTableRowAppendPlan {
    source: MarkdownTableSourceIdentity,
    range: Range<usize>,
    replacement: String,
    next_row_ordinal: usize,
}

impl std::fmt::Debug for MarkdownTableRowAppendPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 诊断只给身份与有限计量，不能因调试计划把用户尚未提交的cell草稿写入日志。
        formatter
            .debug_struct("MarkdownTableRowAppendPlan")
            .field("source", &self.source)
            .field("range", &self.range)
            .field("replacement_bytes", &self.replacement.len())
            .field("next_cell", &self.next_cell())
            .finish()
    }
}

impl MarkdownTableRowAppendPlan {
    /// 提交前重新验证同root与generation；原生层还须核验自己的当前编辑意图。
    ///
    /// # Errors
    /// 源已编辑、Undo、换note或释放时拒绝旧计划，不返回可写的陈旧坐标。
    pub fn replacement_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(&Range<usize>, &str), MarkdownTableWindowError> {
        if !self.source.is_current(document) {
            // generation即使回到相同root也不复活，避免一次Tab被第二次投递重复追加。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok((&self.range, &self.replacement))
    }

    /// 下一位置只是重新查询真实窗口的坐标，不是已发布的新cell或新window权限。
    pub fn next_cell(&self) -> (usize, usize) {
        (self.next_row_ordinal, 0)
    }
}

/// 同一真实End认证出的表尾续写；只持Weak源与原范围，不保Doc、尾部String或新提交权限。
pub(super) struct VerifiedCellRowAppendTail {
    source: MarkdownTableSourceIdentity,
    range: Range<usize>,
    tail: Range<usize>,
    prefix: Range<usize>,
    eol: &'static str,
    columns: usize,
    extra_eol: bool,
    final_eol: bool,
    byte_len: usize,
    next_row_ordinal: usize,
}

impl VerifiedCellRowAppendTail {
    /// 原End决定范围与下一格，坐标本身不能给另一文档签发追加权。
    pub(super) fn replacement_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableWindowError> {
        if !self.source.is_current(document) {
            // 原Doc/root/g任一改变都撤销真实End，不能把旧范围拼到后继内容。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok(&self.range)
    }

    /// 只给成功后重新查询的逻辑行列；这份坐标不保活window或提供激活权。
    pub(super) fn next_cell(&self) -> (usize, usize) {
        (self.next_row_ordinal, 0)
    }
    /// 已由真实范围与固定新行计算的后缀extent，供写入前的原文档预算准入。
    pub(super) fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// 原源范围逐4KiB交付给同一次输出；巨型padding和容器前缀不再复制为大String。
    pub(super) fn write_to(
        &self,
        document: &MarkdownDocumentBuffer,
        write: &mut impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        document.original_backing_health()?;
        if !self.source.is_current(document) {
            return Err(MarkdownDocumentBufferError::PreparedTextInputStale);
        }
        visit_piece_tree_range(
            document,
            document.root.as_deref(),
            self.tail.clone(),
            0,
            write,
        )?;
        if self.extra_eol {
            write(self.eol.as_bytes())?;
        } // 原EOF末行无终止符时，先分开新行。
        visit_piece_tree_range(
            document,
            document.root.as_deref(),
            self.prefix.clone(),
            0,
            write,
        )?;
        for _ in 0..self.columns {
            write(b"| ")?; // 每格真实保留一空格，单列也不能生成Parser拒绝的裸pipe行。
        }
        write(b"|")?;
        if self.final_eol {
            write(self.eol.as_bytes())?;
        } // 原文件已终止时新行也保持同一实际EOL。
        document.original_backing_health()?;
        if !self.source.is_current(document) {
            return Err(MarkdownDocumentBufferError::PreparedTextInputStale);
        }
        Ok(())
    }
}

impl MarkdownDocumentBuffer {
    /// 只在同表真实End证明末行、末列后合并当前格草稿与新空行；本方法不修改文档。
    /// escaped_cell_source沿既有cell输入规则事先转义pipe并归一化换行，不能传未提交的多行值。
    ///
    /// # Errors
    /// 尚无真实End时返回ContextPending，保留原草稿等后继结构请求。身份、坐标或预算无效时
    /// 整体拒绝，不补写一半cell、不构造全文/整表String，也不把局部窗口末行升级为表尾。
    pub fn plan_verified_table_row_append(
        &self,
        window: &VerifiedMarkdownTableWindow,
        row_ordinal: usize,
        column_index: usize,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableRowAppendPlan, MarkdownTableWindowError> {
        let end = window
            .table_end_for(self)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?
            .ok_or(MarkdownTableWindowError::ContextPending)?;
        self.plan_table_row_append_with_end(
            window,
            end,
            row_ordinal,
            column_index,
            escaped_cell_source,
        )
    }

    /// 同一批次的末行和真实End共用原单次替换计划；不把另一请求的窗口拼接为本发布权限。
    pub fn plan_verified_table_viewport_row_append(
        &self,
        batch: &VerifiedMarkdownTableViewport,
        window_index: usize,
        row_ordinal: usize,
        column_index: usize,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableRowAppendPlan, MarkdownTableWindowError> {
        let window = batch
            .window_at_for(self, window_index)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let end = batch
            .required_end_for_window(self, window_index)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?
            .ok_or(MarkdownTableWindowError::ContextPending)?;
        self.plan_table_row_append_with_end(
            window,
            end,
            row_ordinal,
            column_index,
            escaped_cell_source,
        )
    }

    fn plan_table_row_append_with_end(
        &self,
        window: &VerifiedMarkdownTableWindow,
        end: &pulldown_cmark::block_frontier::VerifiedTableEnd,
        row_ordinal: usize,
        column_index: usize,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableRowAppendPlan, MarkdownTableWindowError> {
        // 旧公开入口继续由原cell计划补齐缺格；最终候选仍是原有限的一次源替换。
        let cell = self.plan_verified_table_cell_edit(
            window,
            row_ordinal,
            column_index,
            escaped_cell_source,
        )?;
        let (range, replacement) = cell.replacement_for(self)?;
        self.finish_table_row_append_with_end(
            &cell.cell.context,
            end,
            range.clone(),
            replacement.to_owned(),
        )
    }

    /// 原plan已认证行列与补格前缀；长格和短窗口只借同一真实End派生尾部。
    pub(super) fn verified_cell_row_append_tail_for_plan(
        &self,
        plan: &MarkdownTableCellEditPlan,
    ) -> Result<VerifiedCellRowAppendTail, MarkdownTableWindowError> {
        let (range, _) = plan.replacement_for(self)?;
        let end = plan
            .cell
            .context
            .end
            .as_ref()
            .ok_or(MarkdownTableWindowError::ContextPending)?;
        self.table_row_append_tail_with_end(&plan.cell.context, end, range.clone())
    }

    /// 旧公开有限String计划保原复制预算；规则与长Cell文件续写共用同一个真实表尾证明。
    fn finish_table_row_append_with_end(
        &self,
        context: &TableCellEditContext,
        end: &pulldown_cmark::block_frontier::VerifiedTableEnd,
        cell_range: Range<usize>,
        mut replacement: String,
    ) -> Result<MarkdownTableRowAppendPlan, MarkdownTableWindowError> {
        let tail = self.table_row_append_tail_with_end(context, end, cell_range)?;
        if tail.tail.len() > MAX_APPEND_SOURCE_BYTES || tail.prefix.len() > MAX_APPEND_SOURCE_BYTES
        {
            return Err(MarkdownTableWindowError::SourceLimit); // 旧显式String入口仍不复制巨型尾部。
        }
        let total = replacement
            .len()
            .checked_add(tail.byte_len)
            .filter(|count| *count <= TABLE_WINDOW_MAX_BYTES)
            .ok_or(MarkdownTableWindowError::SourceLimit)?;
        replacement.reserve_exact(total - replacement.len());
        tail.write_to(self, &mut |bytes| {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)?;
            replacement.push_str(text);
            Ok(())
        })
        .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        Ok(MarkdownTableRowAppendPlan {
            source: tail.source,
            range: tail.range,
            replacement,
            next_row_ordinal: tail.next_row_ordinal,
        })
    }

    /// 统一认证行列、End、容器与EOL；返回的只是这份原Source的有限描述符，不读取整尾字节。
    fn table_row_append_tail_with_end(
        &self,
        context: &TableCellEditContext,
        end: &pulldown_cmark::block_frontier::VerifiedTableEnd,
        cell_range: Range<usize>,
    ) -> Result<VerifiedCellRowAppendTail, MarkdownTableWindowError> {
        let row_ordinal = context.row_ordinal;
        let column_index = context.column;
        let last_row = end.last_body_row_ordinal().unwrap_or(0);
        if end.source_revision() != self.generation()
            || end.header_byte_start() != context.header.start
            || end.columns() != context.columns
            || row_ordinal != last_row
            || column_index.checked_add(1) != Some(end.columns())
        {
            // 真实H身份、整表列数与最后body ordinal必须同时一致；H/D-only仅允许header末格。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let last_content_end = if row_ordinal == 0 {
            // 只有H/D时，追加位置在D后；当前H格的原尾与完整D必须一起原样保留。
            context.delimiter.end
        } else {
            context.row_bytes.end
        };
        let terminal_bytes = end
            .byte_end()
            .checked_sub(last_content_end)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        if terminal_bytes > 2
            || end.byte_end() > self.byte_len()
            || self.byte_offset_for_utf16(end.utf16_end()).ok() != Some(end.byte_end())
            || (!end.ended_with_eol() && !end.ended_at_source_eof())
        {
            // End只能紧接最后真实行的EOL；下一块或省略行不能被这一连续替换吞入。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let prefix_range = end.continuation_prefix_byte_range();
        if prefix_range.start != context.delimiter.start
            || prefix_range.start > prefix_range.end
            || prefix_range.end > context.delimiter.end
        {
            // 延续前缀只能借真实D行，不能重用H的列表/脚注opening marker新建另一个容器。
            return Err(MarkdownTableWindowError::InvalidSource);
        }

        // cell_range来自原封闭cell计划；原CellDraft还在准入时认证同一行列和implicit前缀。
        if cell_range.start > cell_range.end || cell_range.end > end.utf16_end() {
            // 下层计划仍须落在当前表尾之前，避免错误范围扩成跨块删除。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let tail_start = self
            .byte_offset_for_utf16(cell_range.end)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let tail_bytes = end
            .byte_end()
            .checked_sub(tail_start)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let prefix_bytes = prefix_range.end - prefix_range.start;
        let terminal = self
            .copy_table_source_range(last_content_end..end.byte_end(), 2)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let eol = if end.ended_with_eol() {
            match terminal.as_str() {
                "\n" => "\n",
                "\r\n" => "\r\n",
                "\r" => "\r",
                _ => {
                    // typed End的终止标量必须与当前真实源相符，不能默默改写非法尾部。
                    return Err(MarkdownTableWindowError::InvalidSource);
                }
            }
        } else {
            if !terminal.is_empty() {
                // 无EOF EOL时保留零终止形式，不能丢掉被误计为EOL的用户字符。
                return Err(MarkdownTableWindowError::InvalidSource);
            }
            // EOF末行无EOL：沿用D实际EOL；D本身就是末行时，H仍有真实分行EOL可取。
            match self.table_append_eol_after(context.delimiter.end)? {
                Some(eol) => eol,
                None => {
                    // D确实没有EOL才借H的分行形式，不额外读取另一个无关源位置。
                    self.table_append_eol_after(context.header.end)?
                        .ok_or(MarkdownTableWindowError::InvalidSource)?
                }
            }
        };
        let next_row_ordinal = last_row
            .checked_add(1)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let new_row_bytes = end
            .columns()
            .checked_mul(2)
            .and_then(|count| count.checked_add(1))
            .ok_or(MarkdownTableWindowError::SourceLimit)?;
        let extra_eol = if end.ended_with_eol() { 0 } else { eol.len() };
        let final_eol = if end.ended_with_eol() { eol.len() } else { 0 };
        let total_bytes = tail_bytes
            .checked_add(extra_eol)
            .and_then(|count| count.checked_add(prefix_bytes))
            .and_then(|count| count.checked_add(new_row_bytes))
            .and_then(|count| count.checked_add(final_eol))
            .ok_or(MarkdownTableWindowError::SourceLimit)?;
        Ok(VerifiedCellRowAppendTail {
            source: self
                .table_source_identity()
                .ok_or(MarkdownTableWindowError::InvalidSource)?,
            range: cell_range.start..end.utf16_end(),
            tail: tail_start..end.byte_end(),
            prefix: prefix_range,
            eol,
            columns: end.columns(),
            extra_eol: !end.ended_with_eol(),
            final_eol: end.ended_with_eol(),
            byte_len: total_bytes,
            next_row_ordinal,
        })
    }

    /// 最多借两byte识别真实EOL；返回static选择值，不缓存原行或搜索全篇换行风格。
    fn table_append_eol_after(
        &self,
        byte_end: usize,
    ) -> Result<Option<&'static str>, MarkdownTableWindowError> {
        if byte_end > self.byte_len() {
            // 无效坐标先拒绝，不能用饱和clamp把它变成“没有EOL”。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let first = self
            .table_source_frame(byte_end, 1)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let first_byte = first.bytes.first().copied();
        first
            .finish()
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        match first_byte {
            Some(b'\n') => Ok(Some("\n")),
            Some(b'\r') => {
                // 跨piece只再读取一个真实单元；两个窗口都须实际回收后才发布EOL事实。
                let next = self
                    .table_source_frame(byte_end + 1, 1)
                    .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
                let is_lf = next.bytes.first() == Some(&b'\n');
                next.finish()
                    .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
                Ok(Some(if is_lf { "\r\n" } else { "\r" }))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_document_buffer::{
        MarkdownTableWindowStep, MarkdownTableWindowTarget, MarkdownTextSelection,
    };
    use std::sync::Arc;

    fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        // 只在独占临时测试文件中取得真实snapshot/piece root，不用手造window或End证明。
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("S2-last-cell-append.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    fn window(document: &MarkdownDocumentBuffer, row: usize) -> VerifiedMarkdownTableWindow {
        let mut request = document
            .table_window_request(
                MarkdownTableWindowTarget::RequiredRow {
                    header_byte_start: 0,
                    row_ordinal: row,
                },
                0..0,
                17,
                usize::MAX..usize::MAX,
            )
            .unwrap();
        for _ in 0..document
            .byte_len()
            .saturating_mul(64)
            .saturating_add(10_000)
        {
            // driver保留有限燃料；每次沿真实piece源推进，不用投影结果倒推整表成员。
            match request.advance(document, false, 4096).unwrap() {
                MarkdownTableWindowStep::Progress { .. } => {}
                MarkdownTableWindowStep::Ready { .. } => {
                    return request.take_ready_for(document).unwrap().unwrap();
                }
                other => panic!("真实目标row没有得到完整语义window: {other:?}"),
            }
        }
        panic!("有限append fixture请求没有结束");
    }

    fn assert_history(source: &str, row: usize, draft: &str, expected: &str) {
        let (_directory, mut document) = fixture(source);
        let verified = window(&document, row);
        let columns = verified.presentation_for(&document).unwrap().column_count;
        let owners = Arc::strong_count(document.root.as_ref().unwrap());
        let plan = document
            .plan_verified_table_row_append(&verified, row, columns - 1, draft)
            .unwrap();
        assert_eq!(plan.next_cell(), (row + 1, 0));
        assert_eq!(Arc::strong_count(document.root.as_ref().unwrap()), owners);
        let (range, replacement) = plan.replacement_for(&document).unwrap();
        let range = range.clone();
        let replacement = replacement.to_owned();
        document
            .replace_utf16_range(range, &replacement, MarkdownTextSelection::caret(0))
            .unwrap();
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 65536)
                .unwrap(),
            expected,
        );
        // 当前格和新增行只有一条canonical事务；一次Undo即回到原始字节且不复活旧计划。
        assert!(document.undo().is_some());
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 65536)
                .unwrap(),
            source,
        );
        assert!(!document.can_undo());
        assert!(plan.replacement_for(&document).is_err());
        assert!(document.redo().is_some());
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 65536)
                .unwrap(),
            expected,
        );
        // 空新行必须再次被真实producer/固定Parser认可，不能只以期望字符串相等代替成员证明。
        let new_window = window(&document, row + 1);
        let presentation = new_window.presentation_for(&document).unwrap();
        assert_eq!(presentation.column_count, columns);
        assert_eq!(presentation.row(row + 1).unwrap().row_ordinal, row + 1);
        assert_eq!(
            new_window
                .table_end_for(&document)
                .unwrap()
                .unwrap()
                .last_body_row_ordinal(),
            Some(row + 1)
        );
    }

    #[test]
    fn final_cell_commit_and_new_row_share_one_undo_and_preserve_unicode_and_following_block() {
        for (source, expected) in [
            (
                "| H1 | H2 |\n| - | - |\n| a | b |\n",
                "| H1 | H2 |\n| - | - |\n| a | 新\\|😀 |\n| | |\n",
            ),
            (
                "| H1 | H2 |\r\n| - | - |\r\n| a | b |\r\n\r\n后继段落😀\r\n",
                "| H1 | H2 |\r\n| - | - |\r\n| a | 新\\|😀 |\r\n| | |\r\n\r\n后继段落😀\r\n",
            ),
        ] {
            // 同一真实编辑路径覆盖LF/CRLF、escaped pipe、代理对和不属于table的后继正文。
            assert_history(source, 1, "新\\|😀", expected);
        }
    }

    #[test]
    fn missing_last_cell_and_single_column_rows_append_valid_gfm_members() {
        assert_history(
            "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|a|\n",
            1,
            "新😀",
            "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|a| |  | 新😀\n| | | | |\n",
        );
        // 一个空格cell的单列表尾必须是`| |`；裸pipe没有真实body row权限。
        assert_history("| H |\n| - |\n| a |", 1, "中", "| H |\n| - |\n| 中 |\n| |");
    }

    #[test]
    fn continuation_prefix_and_original_eof_eol_are_preserved_for_body_and_header_only_tables() {
        for (source, row, expected) in [
            (
                "> > | H1 | H2 |\r\n> > | - | - |\r\n> > | a | b |",
                1,
                "> > | H1 | H2 |\r\n> > | - | - |\r\n> > | a | 新😀 |\r\n> > | | |",
            ),
            (
                "3. | H1 | H2 |\r\n   | - | - |",
                0,
                "3. | H1 | 新😀 |\r\n   | - | - |\r\n   | | |",
            ),
            (
                "| H1 | H2 |\n| - | - |\n",
                0,
                "| H1 | 新😀 |\n| - | - |\n| | |\n",
            ),
        ] {
            // D的continuation字节继续原quote/list，绝不复制H的opening marker建立另一个块。
            assert_history(source, row, "新😀", expected);
        }
    }

    #[test]
    fn partial_window_or_nonfinal_coordinate_preserves_source_and_draft() {
        let source = "| H1 | H2 |\n| - | - |\n| one | x |\n| two | y |\n";
        let (_directory, document) = fixture(source);
        let partial = window(&document, 1);
        assert!(partial.table_end_for(&document).unwrap().is_none());
        assert!(matches!(
            document.plan_verified_table_row_append(&partial, 1, 1, "draft"),
            Err(MarkdownTableWindowError::ContextPending),
        ));
        let final_window = window(&document, 2);
        assert!(matches!(
            document.plan_verified_table_row_append(&final_window, 2, 0, "draft"),
            Err(MarkdownTableWindowError::InvalidSource),
        ));
        assert_eq!(
            document
                .copy_table_source_range(0..source.len(), 65536)
                .unwrap(),
            source
        );
        assert!(!document.can_undo());
    }

    #[test]
    fn plan_rejects_other_root_and_old_generation_and_does_not_keep_document_alive() {
        let source = "| H1 | H2 |\n| - | - |\n| a | b |\n";
        let (_first_directory, mut document) = fixture(source);
        let (_other_directory, other) = fixture(source);
        let verified = window(&document, 1);
        let plan = document
            .plan_verified_table_row_append(&verified, 1, 1, "draft")
            .unwrap();
        let weak = Arc::downgrade(document.root.as_ref().unwrap());
        assert!(plan.replacement_for(&other).is_err());
        document
            .insert_utf16(0, "改", MarkdownTextSelection::caret(1))
            .unwrap();
        assert!(plan.replacement_for(&document).is_err());
        document.undo().unwrap();
        assert!(plan.replacement_for(&document).is_err());
        assert!(
            document
                .plan_verified_table_row_append(&verified, 1, 1, "draft")
                .is_err()
        );
        drop(verified);
        drop(document);
        // plan、旧window都只有Weak。Root恢复相同内容或drop后不能留下snapshot/history强持有。
        assert_eq!(weak.strong_count(), 0);
    }

    #[test]
    fn five_thousand_rows_only_copy_last_cell_tail_and_reject_oversized_draft_before_mutation() {
        let mut source = String::from("| H1 | H2 |\n| - | - |\n");
        for row in 1..=5000 {
            source.push_str(&format!("| ROW{row} | 中😀 |\n"));
        }
        let (_directory, document) = fixture(&source);
        let verified = window(&document, 5000);
        assert_eq!(verified.presentation_for(&document).unwrap().rows.len(), 1);
        let plan = document
            .plan_verified_table_row_append(&verified, 5000, 1, "新😀")
            .unwrap();
        let (range, replacement) = plan.replacement_for(&document).unwrap();
        assert!(range.start > document.utf16_len() - 16);
        assert!(replacement.len() < 32);
        assert_eq!(plan.next_cell(), (5001, 0));
        let oversized = "x".repeat(TABLE_WINDOW_MAX_BYTES + 1);
        assert!(matches!(
            document.plan_verified_table_row_append(&verified, 5000, 1, &oversized),
            Err(MarkdownTableWindowError::SourceLimit),
        ));
        assert_eq!(document.byte_len(), source.len());
        assert!(!document.can_undo());
    }
}
