//! 有限表格cell的单次源替换计划；补格时保留真实pipe、容器、EOL与原canonical Undo入口。

use super::{table_source::MarkdownTableSourceIdentity, *};
use crate::markdown_viewport_presentation::{
    MarkdownPresentationStyles,
    table_window::{
        MarkdownProjectedTableWindow, MarkdownTableWindowCellSource, MarkdownTableWindowError,
        TABLE_WINDOW_MAX_BYTES, TABLE_WINDOW_MAX_COLUMNS,
    },
};

#[derive(Debug)]
/// 计划只保一次有限replacement和root Weak。原生层须在自己的意图token仍有效时提交。
pub struct MarkdownTableCellEditPlan {
    pub(super) cell: VerifiedMarkdownTableCellSource,
    replacement: String,
    /// 实际缺格分隔符和显式替换正文分开计量；后者不能冒充草稿激活前缀。
    pub(super) separator_prefix_bytes: usize,
}

#[derive(Clone, Debug)]
/// 原成员行与Header/End的编辑上下文；不包含inline样式Ready或正文副本。
pub(super) struct TableCellEditContext {
    pub header: Range<usize>,
    pub delimiter: Range<usize>,
    pub row_bytes: Range<usize>,
    pub row_utf16: Range<usize>,
    pub row_ordinal: usize,
    pub column: usize,
    pub columns: usize,
    pub end: Option<pulldown_cmark::block_frontier::VerifiedTableEnd>,
}

/// 原实际格后只有连续缺格；真实尾pipe已充当第一条分隔符，不能再次补写。
pub(super) fn missing_cell_separators(
    column: usize,
    first_missing: usize,
    trailing_pipe: bool,
) -> Result<usize, MarkdownTableWindowError> {
    let before = column
        .checked_sub(first_missing)
        .ok_or(MarkdownTableWindowError::ParserMismatch)?;
    Ok(before + usize::from(!trailing_pipe))
}

impl MarkdownTableCellEditPlan {
    /// 草稿会话在消费计划前捕获原列方向；不从 raw 文字或当前光标重新查另一张表。
    ///
    /// # Errors
    /// 原来源失败或计划所属根失效时拒绝，不消费计划或创建草稿。
    pub fn alignment_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<crate::MarkdownTableAlignment, MarkdownTableRequestError> {
        self.cell.alignment_for(document)
    }

    /// 返回前重核真实root及代次；NSString构造等可重入操作之后仍须在提交边界再次检查。
    pub fn replacement_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(&Range<usize>, &str), MarkdownTableWindowError> {
        if !self.cell.source.is_current(document) {
            // 编辑、Undo、换note都不能复用旧整数坐标，即使新长度恰好相同。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok((&self.cell.range, &self.replacement))
    }
}

impl MarkdownDocumentBuffer {
    /// 仅供Core内部组合已验证票据；外部不能用伪造投影与相同代次坐标创建编辑计划。
    /// escaped_cell_source沿既有单元格输入规则事先转义pipe并归一化用户换行，此处不改用户字节。
    pub(crate) fn plan_projected_table_cell_edit(
        &self,
        source: &MarkdownTableSourceIdentity,
        window: &MarkdownProjectedTableWindow,
        row_ordinal: usize,
        column_index: usize,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableCellEditPlan, MarkdownTableWindowError> {
        if !source.is_current(self) || window.generation != self.generation() {
            // 先拒绝其他root，不能读取外来投影的行尾或由它推导补格前缀。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let cell = self.projected_table_cell_source(window, row_ordinal, column_index)?;
        self.plan_table_cell_source_replacement(cell, escaped_cell_source)
    }

    /// 认证窗口和原编辑计划共用同一份端点规则；返回纯结构来源，不构造 replacement 或 inline Ready。
    pub(super) fn projected_table_cell_source(
        &self,
        window: &MarkdownProjectedTableWindow,
        row_ordinal: usize,
        column_index: usize,
    ) -> Result<VerifiedMarkdownTableCellSource, MarkdownTableWindowError> {
        let row = window
            .row(row_ordinal)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let cell = window
            .cell(row_ordinal, column_index)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        if self.utf16_offset_for_byte(row.byte_range.start).ok() != Some(row.utf16_range.start)
            || self.utf16_offset_for_byte(row.byte_range.end).ok() != Some(row.utf16_range.end)
        {
            // row的byte/UTF16坐标必须同时落在当前piece源，不能拿投影坐标替换原文。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let range = match &cell.source {
            MarkdownTableWindowCellSource::Present(range) => {
                if range.start < row.utf16_range.start
                    || range.start > range.end
                    || range.end > row.utf16_range.end
                    || self.byte_offset_for_utf16(range.start).is_err()
                    || self.byte_offset_for_utf16(range.end).is_err()
                {
                    // 显式空格 trim 后也可以是零长 Present；只证明真实范围，不补新列。
                    return Err(MarkdownTableWindowError::InvalidSource);
                }
                range.clone()
            }
            MarkdownTableWindowCellSource::ImplicitEmpty {
                insertion_anchor_utf16,
            } => {
                if *insertion_anchor_utf16 != row.utf16_range.end {
                    // 缺格只有原行内容末端锚点，不产生可读取字符，也不能跨 EOL 借邻行正文。
                    return Err(MarkdownTableWindowError::InvalidSource);
                }
                *insertion_anchor_utf16..*insertion_anchor_utf16
            }
        };
        let source = self
            .table_source_identity()
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let implicit_empty = matches!(
            cell.source,
            MarkdownTableWindowCellSource::ImplicitEmpty { .. }
        );
        // 有限窗口与流式 Scanner 在签发格票时各自证实同一种补列事实，消费计划时不再解释范围。
        // 此路径只读取原有限投影的尾部 padding；正式巨格直接携带原 Scanner 的结果，不重读整格。
        let separators = if implicit_empty {
            let insertion_anchor_utf16 = range.start;
            let cells = window
                .cells
                .get(row.cell_index_range.clone())
                .ok_or(MarkdownTableWindowError::InvalidSource)?;
            let first_missing = cells
                .iter()
                .position(|cell| {
                    matches!(
                        cell.source,
                        MarkdownTableWindowCellSource::ImplicitEmpty { .. }
                    )
                })
                .ok_or(MarkdownTableWindowError::ParserMismatch)?;
            if cells[first_missing..]
                .iter()
                .any(|cell| match &cell.source {
                    MarkdownTableWindowCellSource::ImplicitEmpty {
                        insertion_anchor_utf16: anchor,
                    } => *anchor != insertion_anchor_utf16,
                    MarkdownTableWindowCellSource::Present(_) => true,
                })
            {
                // 原 Parser 只在同一行尾补连续缺格；中间再有真实格不能按空列插入。
                return Err(MarkdownTableWindowError::ParserMismatch);
            }
            let tail_start = if let Some(index) = first_missing.checked_sub(1) {
                match &cells[index].source {
                    // 最后一个真实格末尾之后才可能有结构尾 pipe 或行内 padding。
                    MarkdownTableWindowCellSource::Present(range) => range.end,
                    MarkdownTableWindowCellSource::ImplicitEmpty { .. } => {
                        return Err(MarkdownTableWindowError::ParserMismatch);
                    }
                }
            } else {
                // 没有真实前驱的极小行仍受原有限窗口的独立 64KiB 读取上限约束。
                row.utf16_range.start
            };
            if tail_start < row.utf16_range.start || tail_start > insertion_anchor_utf16 {
                return Err(MarkdownTableWindowError::InvalidSource);
            }
            let tail_byte = self
                .byte_offset_for_utf16(tail_start)
                .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
            let tail = self
                .copy_table_source_range(tail_byte..row.byte_range.end, 64 * 1024)
                .map_err(|_| MarkdownTableWindowError::SourceLimit)?;
            let trimmed = tail.trim_end_matches([' ', '\t']);
            let trailing_pipe = trimmed.ends_with('|') && {
                // 字符之外必须有原 Parser 的 TABLE_BOUNDARY 证据；escaped pipe 不能算闭列。
                let at = tail_start + trimmed.encode_utf16().count() - 1;
                let index = window.runs.partition_point(|run| run.utf16_range.end <= at);
                window.runs.get(index).is_some_and(|run| {
                    run.utf16_range.contains(&at)
                        && run
                            .styles
                            .contains(MarkdownPresentationStyles::TABLE_BOUNDARY)
                })
            };
            missing_cell_separators(column_index, first_missing, trailing_pipe)?
        } else {
            0 // 显式空格的零长原范围是现成格，不能因显示为空额外插入分隔符。
        };
        Ok(VerifiedMarkdownTableCellSource {
            source,
            context: TableCellEditContext {
                header: window.header_byte_range.clone(),
                delimiter: window.delimiter_byte_range.clone(),
                row_bytes: row.byte_range.clone(),
                row_utf16: row.utf16_range.clone(),
                row_ordinal,
                column: column_index,
                columns: window.column_count,
                end: None, // 原 verified owner 在边界附加 End；metadata 本身不能推断表尾。
            },
            range,
            implicit_empty,
            separators,
            alignment: cell.alignment,
            inline_seed: None, // 普通格已由 verified window 的完整语义授权；此编辑端点工厂不另签 inline。
        })
    }

    /// 短窗口与流式原格扫描共用唯一计划构造；上游各自交出真实GFM端点，不另定义补格协议。
    pub(super) fn plan_table_cell_source_replacement(
        &self,
        cell: VerifiedMarkdownTableCellSource,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableCellEditPlan, MarkdownTableWindowError> {
        let separators = cell.separators;
        let range = &cell.range;
        let context = &cell.context;
        if !cell.source.is_current(self)
            || range.start < context.row_utf16.start
            || range.start > range.end
            || range.end > context.row_utf16.end
        {
            // 上游已经认证行与格的标量端点；这里只核原root及相同行范围，不重复回读整格找坐标。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        if context.columns == 0
            || context.columns > TABLE_WINDOW_MAX_COLUMNS
            || context.column >= context.columns
            || separators > context.columns
        {
            return Err(MarkdownTableWindowError::ColumnLimit);
        }
        if escaped_cell_source.len() > TABLE_WINDOW_MAX_BYTES
            || escaped_cell_source.contains(['\r', '\n'])
        {
            // 这仍是原有限显式替换入口；长草稿沿同一plan进入流式sanitize，而非扩大String准入。
            return Err(MarkdownTableWindowError::SourceLimit);
        }
        let total = escaped_cell_source
            .len()
            .saturating_add(separators.saturating_mul(3));
        if total > TABLE_WINDOW_MAX_BYTES {
            // 补格分隔符同样计入单批源预算，不能在payload准入后额外无界增长。
            return Err(MarkdownTableWindowError::SourceLimit);
        }
        let mut replacement = String::with_capacity(total);
        for _ in 0..separators {
            // 一次源替换同时补齐目标之前的空列；最多column_count条，保留一次Undo动作。
            replacement.push_str(" | ");
        }
        replacement.push_str(escaped_cell_source);
        Ok(MarkdownTableCellEditPlan {
            cell,
            replacement,
            separator_prefix_bytes: separators * 3,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_viewport_presentation::table_window::{
        MarkdownTableGlobalContext, MarkdownTableSourceLine, MarkdownTableWindowInput,
        project_table_window,
    };

    fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("S2-owned-cell-edit.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    fn try_projected(
        source: &str,
        generation: u64,
        prefix: usize,
    ) -> Result<MarkdownProjectedTableWindow, MarkdownTableWindowError> {
        // 纯fixture可遍历自身短源建立已知行，不把此函数放入真实global producer。
        let mut byte_start = 0;
        let mut utf16_start = 0;
        let mut lines = Vec::new();
        for (source_line, text) in source.split_inclusive('\n').enumerate() {
            lines.push(MarkdownTableSourceLine {
                text,
                byte_start,
                utf16_start,
                source_line,
                row_ordinal: source_line.saturating_sub(1),
                container_prefix_bytes: prefix,
            });
            byte_start += text.len();
            utf16_start += text.encode_utf16().count();
        }
        project_table_window(&MarkdownTableWindowInput {
            generation,
            context_revision: generation,
            header: lines[0],
            delimiter: lines[1],
            body: &lines[2..],
            include_header: false,
            inherited_styles: MarkdownPresentationStyles::default(),
            container_prefix_runs: &[],
            global: MarkdownTableGlobalContext::Ready {
                reference_definitions: "",
                underline: &[],
            },
            active_selection: usize::MAX..usize::MAX,
        })
    }

    fn projected(source: &str, generation: u64, prefix: usize) -> MarkdownProjectedTableWindow {
        try_projected(source, generation, prefix)
            .unwrap_or_else(|error| panic!("source={source:?} error={error:?}"))
    }

    fn apply_and_check(row: &str, target: usize, expected_row: &str, prefix: &str, eol: &str) {
        let source = format!(
            "{prefix}| H1 | H2 | H3 | H4 |{eol}{prefix}| - | - | - | - |{eol}{prefix}{row}{eol}"
        );
        let expected = format!(
            "{prefix}| H1 | H2 | H3 | H4 |{eol}{prefix}| - | - | - | - |{eol}{prefix}{expected_row}{eol}"
        );
        let (_directory, mut document) = fixture(&source);
        let window = projected(&source, document.generation(), prefix.len());
        let identity = document.table_source_identity().unwrap();
        let plan = document
            .plan_projected_table_cell_edit(&identity, &window, 1, target, "新😀")
            .unwrap();
        let (range, replacement) = plan.replacement_for(&document).unwrap();
        let selection =
            MarkdownTextSelection::caret(range.start + replacement.encode_utf16().count());
        let range = range.clone();
        let replacement = replacement.to_owned();
        document
            .replace_utf16_range(range, &replacement, selection)
            .unwrap();
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 64 * 1024)
                .unwrap(),
            expected
        );
        let updated = projected(&expected, document.generation(), prefix.len());
        let MarkdownTableWindowCellSource::Present(range) =
            &updated.cell(1, target).unwrap().source
        else {
            panic!("已补目标仍不是真实Present cell");
        };
        let actual = document.materialize_viewport(range.clone(), 0).unwrap();
        assert_eq!(actual.text, "新😀");
        // 计划交给原canonical单次替换；一次Undo完整还原source/容器/EOL，旧plan永不复活。
        assert!(document.undo().is_some());
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 64 * 1024)
                .unwrap(),
            source
        );
        assert!(plan.replacement_for(&document).is_err());
        assert!(document.redo().is_some());
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 64 * 1024)
                .unwrap(),
            expected
        );
    }

    #[test]
    fn implicit_cells_add_only_required_separators_and_preserve_atomic_history() {
        for (row, target, expected) in [
            ("a", 1, "a | 新😀"),
            ("a", 3, "a |  |  | 新😀"),
            ("|a|", 1, "|a|新😀"),
            ("|a|", 3, "|a| |  | 新😀"),
            ("|a||", 3, "|a|| | 新😀"),
            ("a\\|b", 1, "a\\|b | 新😀"),
            ("a\\\\|", 1, "a\\\\| | 新😀"),
            ("`a|b`", 3, "`a|b` |  | 新😀"),
        ] {
            // 每个case由真实固定Parser给出缺格/marker，期望不按pipe奇偶或code span自行猜测。
            apply_and_check(row, target, expected, "", "\n");
        }
    }

    #[test]
    fn explicit_empty_and_crlf_quote_container_do_not_gain_a_spurious_column() {
        // 单独一条pipe不是固定Parser认可的body row，不能以补格计划把非成员“修成”同一张表。
        let non_member = "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|\n";
        assert_eq!(
            try_projected(non_member, 0, 0),
            Err(MarkdownTableWindowError::ParserMismatch)
        );
        apply_and_check("|a||", 1, "|a|新😀|", "", "\n");
        apply_and_check("|a|  ", 2, "|a|   | 新😀", "> ", "\r\n");
    }

    #[test]
    fn invalid_intent_source_or_non_normalized_input_preserves_bytes_and_history() {
        let source = "| H1 | H2 |\n| - | - |\n| a |\n";
        let (_directory, mut document) = fixture(source);
        let window = projected(source, document.generation(), 0);
        let identity = document.table_source_identity().unwrap();
        assert!(
            document
                .plan_projected_table_cell_edit(&identity, &window, 1, 1, "x\ny")
                .is_err()
        );
        assert!(
            document
                .plan_projected_table_cell_edit(&identity, &window, 999, 1, "x")
                .is_err()
        );
        assert!(!document.can_undo());
        let plan = document
            .plan_projected_table_cell_edit(&identity, &window, 1, 1, "x")
            .unwrap();
        document
            .insert_utf16(0, "changed", MarkdownTextSelection::caret(7))
            .unwrap();
        assert!(plan.replacement_for(&document).is_err());
        assert!(
            document
                .plan_projected_table_cell_edit(&identity, &window, 1, 1, "x")
                .is_err()
        );
    }
}
