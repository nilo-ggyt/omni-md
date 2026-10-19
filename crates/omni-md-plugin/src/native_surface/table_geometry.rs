//! Models bounded visible-table geometry, cell identity and keyboard navigation.

use super::*;

/// 当前视口及两侧 overscan 的行高元数据上限；不随整张表的行数增长。
const MAX_PROJECTED_TABLE_ROWS: usize = 256;
// 这些数值只限制当前可见投影的准备峰值，超限显示完整原文，不代表最终大表功能规格。
const MAX_PREPARED_TABLE_CELLS: usize = 1_024;
const MAX_PREPARED_TABLE_SOURCE_UTF16_UNITS: usize = 64 * 1_024;
const MAX_PREPARED_TABLE_COLUMNS: usize = 256;
const MAX_PREPARED_TABLE_COLUMN_SLOTS: usize = 1_024;

/// 纯标量预算先于任何原生单元格/列宽数组；旧快照与新批次各自受相同上限约束。
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct NativeMarkdownTablePreparationBudget {
    pub(super) row_slots: usize,
    pub(super) cells: usize,
    pub(super) source_utf16_units: usize,
    pub(super) column_slots: usize,
}

/// 表头定义列数，不能由未显示行的任意column_index扩大后续列宽数组。
pub(super) fn native_markdown_table_column_count(table: &MarkdownTablePrototype) -> Option<usize> {
    let header = table.rows.first()?;
    let count = header
        .cell_index_range
        .end
        .checked_sub(header.cell_index_range.start)?;
    (header.is_header && count != 0 && header.cell_index_range.end <= table.cells.len())
        .then_some(count)
}

/// 语义 overscan 可带离屏表；只有真实源范围与本批半开窗口相交才需要表格布局。
pub(super) fn native_markdown_table_window_requires_layout(
    tables: &[MarkdownTablePrototype],
    window: &std::ops::Range<usize>,
) -> Option<bool> {
    if window.start > window.end {
        return None; // 反向窗口不能证明当前区域没有表格。
    }
    let mut intersects = false;
    for table in tables {
        if table.utf16_range.start >= table.utf16_range.end {
            return None; // 无效 prototype 范围不能被当作合法离屏表静默跳过。
        }
        // 仅判断成员关系；相交表的列、行、cell 及总预算仍由原准备路径完整验证。
        intersects |= window.start < window.end
            && table.utf16_range.start < window.end
            && window.start < table.utf16_range.end;
    }
    Some(intersects)
}

/// 在全批任何cell原生对象构造前检查可见/overscan总量，不创建预算Vec或正文副本。
pub(super) fn native_markdown_table_preparation_budget(
    tables: &[MarkdownTablePrototype],
    viewport: &std::ops::Range<usize>,
) -> Option<NativeMarkdownTablePreparationBudget> {
    if viewport.start > viewport.end {
        // 反向视口没有安全的可见行定义。
        return None;
    }
    let mut budget = NativeMarkdownTablePreparationBudget::default();
    for table in tables {
        // 语义overscan中完全离开视口的表格不会生成原生对象，也不占本批准备预算。
        if table.utf16_range.start >= viewport.end || table.utf16_range.end <= viewport.start {
            continue;
        }
        let selected = native_markdown_visible_table_row_range(&table.rows, viewport);
        if selected.is_empty() {
            // 无候选行的表格没有列宽、cell或delimiter投影。
            continue;
        }
        let columns = native_markdown_table_column_count(table)?;
        if columns > MAX_PREPARED_TABLE_COLUMNS {
            // 单表列数限制在分配natural_widths之前，空格也不能绕过。
            return None;
        }
        budget.column_slots = budget.column_slots.checked_add(columns)?;
        budget.row_slots = budget
            .row_slots
            .checked_add(selected.len())?
            .checked_add(1)?;
        let delimiter_units = table
            .delimiter_utf16_range
            .end
            .checked_sub(table.delimiter_utf16_range.start)?;
        budget.source_utf16_units = budget.source_utf16_units.checked_add(delimiter_units)?;
        for row_index in selected {
            // 连续源范围与cell索引同时验证，越界或反向prototype不能触发部分准备。
            let row = table.rows.get(row_index)?;
            let row_units = row.utf16_range.end.checked_sub(row.utf16_range.start)?;
            let count = row
                .cell_index_range
                .end
                .checked_sub(row.cell_index_range.start)?;
            let cells = table.cells.get(row.cell_index_range.clone())?;
            budget.cells = budget.cells.checked_add(count)?;
            budget.source_utf16_units = budget.source_utf16_units.checked_add(row_units)?;
            if budget.row_slots > MAX_PROJECTED_TABLE_ROWS
                || budget.cells > MAX_PREPARED_TABLE_CELLS
                || budget.source_utf16_units > MAX_PREPARED_TABLE_SOURCE_UTF16_UNITS
                || budget.column_slots > MAX_PREPARED_TABLE_COLUMN_SLOTS
            {
                // 任一总量超限使整批失败；不能先建立前1024格再把剩余格隐藏。
                return None;
            }
            let mut previous_cell_end = row.utf16_range.start;
            for cell in cells {
                // 按源顺序不重叠，row units才覆盖全部临时文本；重复长slice不能绕过总预算。
                if cell.column_index >= columns
                    || cell.utf16_range.start < previous_cell_end
                    || cell.utf16_range.start > cell.utf16_range.end
                    || cell.utf16_range.end > row.utf16_range.end
                {
                    // 空cell可共享同一边界，非空cell不得重复分配重叠源范围。
                    return None;
                }
                previous_cell_end = cell.utf16_range.end;
            }
        }
    }
    Some(budget)
}

#[cfg(test)]
mod preparation_budget_tests {
    use super::*;

    fn empty_cell_table(columns: usize, body_rows: usize) -> String {
        // 合成真实GFM：表头有文字，正文格全空，仅靠大量'|'就能制造对象数量压力。
        let mut source = format!("|{}\n|{}\n", "h|".repeat(columns), "---|".repeat(columns));
        for _ in 0..body_rows {
            // 空cell仍是parser正式cell，不能因为源码短或长度为零而不计预算。
            source.push('|');
            source.push_str(&" |".repeat(columns));
            source.push('\n');
        }
        source
    }

    fn parsed(source: &str) -> MarkdownViewportPresentation {
        // 使用真实core parser验证攻击形状可到达适配器；没有AppKit或native factory。
        let mut presentation = MarkdownViewportPresentation::default();
        presentation
            .rebuild(1, 0..source.encode_utf16().count(), source)
            .unwrap();
        assert!(!presentation.tables().is_empty());
        presentation
    }

    #[test]
    fn many_pipe_empty_cells_are_rejected_before_native_preparation() {
        let exact = empty_cell_table(64, 15);
        let exact_presentation = parsed(&exact);
        let accepted = native_markdown_table_preparation_budget(
            exact_presentation.tables(),
            &(0..exact.len()),
        )
        .unwrap();
        assert_eq!(accepted.cells, MAX_PREPARED_TABLE_CELLS);
        let excess = empty_cell_table(64, 16);
        let presentation = parsed(&excess);
        // 约几KiB就有1088格；仅256KiB envelope或256行检查都挡不住它。
        assert!(excess.len() < 8 * 1_024);
        assert_eq!(presentation.tables()[0].cells.len(), 1_088);
        assert!(
            native_markdown_table_preparation_budget(presentation.tables(), &(0..excess.len()))
                .is_none()
        );
    }

    #[test]
    fn native_budget_is_global_across_tables_in_one_viewport() {
        let one = empty_cell_table(64, 7);
        let one_presentation = parsed(&one);
        assert_eq!(
            native_markdown_table_preparation_budget(one_presentation.tables(), &(0..one.len()))
                .unwrap()
                .cells,
            512
        );
        let combined = format!("{one}\n{one}\n{one}");
        let presentation = parsed(&combined);
        assert_eq!(presentation.tables().len(), 3);
        // 每表单独合格不允许先创建三表对象后再检查总数。
        assert!(
            native_markdown_table_preparation_budget(presentation.tables(), &(0..combined.len()))
                .is_none()
        );
    }

    #[test]
    fn source_budget_counts_utf16_before_any_cell_strings() {
        let within = format!(
            "|h|\n|---|\n|{}|\n",
            "😀".repeat(MAX_PREPARED_TABLE_SOURCE_UTF16_UNITS / 2 - 32)
        );
        let accepted = parsed(&within);
        // 字节数可大于64KiB；本预算明确计UTF-16 units，而非偷换成字节或字符。
        assert!(within.len() > MAX_PREPARED_TABLE_SOURCE_UTF16_UNITS);
        assert!(
            native_markdown_table_preparation_budget(
                accepted.tables(),
                &(0..within.encode_utf16().count())
            )
            .is_some()
        );
        let excess = format!(
            "|h|\n|---|\n|{}|\n",
            "😀".repeat(MAX_PREPARED_TABLE_SOURCE_UTF16_UNITS / 2)
        );
        let presentation = parsed(&excess);
        assert!(excess.len() < 256 * 1_024);
        assert!(
            native_markdown_table_preparation_budget(
                presentation.tables(),
                &(0..excess.encode_utf16().count())
            )
            .is_none()
        );
    }

    #[test]
    fn row_and_column_limits_apply_before_metadata_arrays() {
        let exact_rows = empty_cell_table(1, MAX_PROJECTED_TABLE_ROWS - 2);
        let accepted = parsed(&exact_rows);
        assert_eq!(
            native_markdown_table_preparation_budget(accepted.tables(), &(0..exact_rows.len()))
                .unwrap()
                .row_slots,
            MAX_PROJECTED_TABLE_ROWS
        );
        let extra_row = empty_cell_table(1, MAX_PROJECTED_TABLE_ROWS - 1);
        let rows = parsed(&extra_row);
        assert!(
            native_markdown_table_preparation_budget(rows.tables(), &(0..extra_row.len()))
                .is_none()
        );
        let wide = empty_cell_table(MAX_PREPARED_TABLE_COLUMNS + 1, 1);
        let columns = parsed(&wide);
        // 514格低于cell总上限，仍不能让单表列宽数组无界增长。
        assert!(columns.tables()[0].cells.len() < MAX_PREPARED_TABLE_CELLS);
        assert!(
            native_markdown_table_preparation_budget(columns.tables(), &(0..wide.len())).is_none()
        );
    }

    #[test]
    fn overlapping_cell_sources_cannot_multiply_text_outside_the_budget() {
        let source = empty_cell_table(2, 1);
        let presentation = parsed(&source);
        let mut table = presentation.tables()[0].clone();
        let first = table.rows[0].cell_index_range.start;
        // 即使上游prototype出错，两格重复同一非空范围也不能让row计数低估复制量。
        table.cells[first + 1].utf16_range = table.cells[first].utf16_range.clone();
        assert!(native_markdown_table_preparation_budget(&[table], &(0..source.len())).is_none());
    }

    #[test]
    fn real_core_overscan_table_does_not_require_layout_for_following_quote_and_code() {
        // 重现真实 producer：前表有 240 个 body 行，后接 3002 字符引用与 18000 字符代码行。
        // working 只覆盖后两块，但 core 的有限 overscan 会把前表纳入同一语义快照。
        let table = empty_cell_table(4, 240);
        let prefix = format!("{table}\n");
        let tail = format!(
            "> {}\n\n```text\n{}\n```\n",
            "引".repeat(3_000),
            "码".repeat(18_000)
        );
        let source = format!("{prefix}{tail}");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("overscan-table.md");
        std::fs::write(&path, &source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 256 * 1_024).unwrap();
        let working = prefix.encode_utf16().count()..document.utf16_len();
        assert!(working.len() < MAX_PREPARED_TABLE_SOURCE_UTF16_UNITS);
        let slice = document
            .materialize_markdown_viewport(
                working.clone(),
                MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS,
            )
            .unwrap();
        assert_eq!(
            slice.semantic_status,
            MarkdownViewportSemanticStatus::Complete
        );
        let mut presentation = MarkdownViewportPresentation::default();
        presentation
            .rebuild(
                document.generation(),
                slice.utf16_range.clone(),
                &slice.text,
            )
            .unwrap();
        let snapshot = super::super::text_storage::NativeMarkdownTablePresentationSnapshot {
            document_generation: document.generation(),
            utf16_window: slice.utf16_range,
            // 本纯几何测试不借正文或发 Native 调用；空 Weak 不具有运行期源权限。
            identity: std::rc::Weak::new(),
            document_length: document.utf16_len(),
            selection: document.selection(),
            semantic_epoch: 0,
            semantics: presentation.semantic_snapshot(),
            verified_no_tables: None,
        };
        assert!(!snapshot.tables().is_empty());
        assert!(snapshot.utf16_window.start < working.start);
        assert!(
            snapshot
                .tables()
                .iter()
                .all(|table| table.utf16_range.end <= working.start)
        );
        // 旧 is_empty 判据会进入 builder，离屏表预算合法为零，随后空 Vec 却被误报预算失败。
        assert_eq!(
            native_markdown_table_preparation_budget(snapshot.tables(), &working),
            Some(NativeMarkdownTablePreparationBudget::default())
        );
        assert_eq!(
            native_markdown_table_window_requires_layout(snapshot.tables(), &working),
            Some(false)
        );
    }

    #[test]
    fn touching_ranges_are_disjoint_but_intersecting_invalid_tables_still_require_validation() {
        // prototype 来自真实 core parser；边界相接不意味着两个半开源区间相交。
        let source = format!("before\n\n{}\nafter\n", empty_cell_table(2, 1));
        let presentation = parsed(&source);
        let table = &presentation.tables()[0];
        let range = table.utf16_range.clone();
        assert_eq!(
            native_markdown_table_window_requires_layout(presentation.tables(), &(0..range.start)),
            Some(false)
        );
        assert_eq!(
            native_markdown_table_window_requires_layout(
                presentation.tables(),
                &(range.end..source.len())
            ),
            Some(false)
        );
        assert_eq!(
            native_markdown_table_window_requires_layout(
                presentation.tables(),
                &(range.start..range.start)
            ),
            Some(false)
        );
        assert_eq!(
            native_markdown_table_window_requires_layout(presentation.tables(), &range),
            Some(true)
        );

        let mut invalid = table.clone();
        let first = invalid.rows[0].cell_index_range.start;
        invalid.cells[first + 1].utf16_range = invalid.cells[first].utf16_range.clone();
        // 相交但 cell 源重叠的表不能退入无表成功分支，必须仍被原预算验证拒绝。
        assert_eq!(
            native_markdown_table_window_requires_layout(std::slice::from_ref(&invalid), &range),
            Some(true)
        );
        assert!(native_markdown_table_preparation_budget(&[invalid], &range).is_none());

        let excess = empty_cell_table(64, 16);
        let over_budget = parsed(&excess);
        let over_range = 0..excess.len();
        // 相交但对象预算超限也保留原失败；源窗口准入不授予跳过预算的权限。
        assert_eq!(
            native_markdown_table_window_requires_layout(over_budget.tables(), &over_range),
            Some(true)
        );
        assert!(
            native_markdown_table_preparation_budget(over_budget.tables(), &over_range).is_none()
        );
        let mut invalid_range = table.clone();
        invalid_range.utf16_range.end = invalid_range.utf16_range.start;
        assert_eq!(
            native_markdown_table_window_requires_layout(&[invalid_range], &range),
            None
        );
    }
}

/// 一行完整源范围与最终列宽下测得的高度，不保留单元格正文或原生对象。
#[derive(Clone, Debug, PartialEq)]
pub(super) struct NativeMarkdownTableRowHeight {
    pub(super) source_utf16_range: std::ops::Range<usize>,
    pub(super) height: f64,
}

/// 有限行高属于一个精确文档/代次/宽度；epoch 绑定属性与最终绘制几何。
pub(super) struct NativeMarkdownTableRowHeightProjection {
    pub(super) document_identity: WeakMarkdownDocument,
    pub(super) document_generation: u64,
    pub(super) width_key: i64,
    pub(super) epoch: u64,
    pub(super) rows: Vec<NativeMarkdownTableRowHeight>,
}

impl NativeMarkdownTableRowHeightProjection {
    /// 数值代次相同不等于同一正文，宽度和epoch也必须同时匹配。
    pub(super) fn matches(
        &self,
        identity: &WeakMarkdownDocument,
        generation: u64,
        width_key: i64,
        epoch: u64,
    ) -> bool {
        self.document_generation == generation
            && self.width_key == width_key
            && self.epoch == epoch
            && std::rc::Weak::ptr_eq(&self.document_identity, identity)
    }

    /// 检查整批输入后才允许发布；重叠范围或非有限高度不能变成局部成功。
    pub(super) fn rows_are_valid(
        rows: &[NativeMarkdownTableRowHeight],
        document_length: usize,
    ) -> bool {
        if rows.len() > MAX_PROJECTED_TABLE_ROWS {
            // 过多细行仍需保留原文；不能仅保留前若干行后隐藏剩余正文。
            return false;
        }
        let mut previous_end = 0;
        for row in rows {
            // 每行范围与高度均独立验证，禁止混入反向/重叠/越界或 NaN 投影。
            if row.source_utf16_range.start < previous_end
                || row.source_utf16_range.start >= row.source_utf16_range.end
                || row.source_utf16_range.end > document_length
                || !row.height.is_finite()
                || row.height < NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT
            {
                // 一项失败使整批失效，不允许部分表格覆盖原生 source。
                return false;
            }
            previous_end = row.source_utf16_range.end;
        }
        true
    }

    /// 坐标范围还必须落在canonical scalar边界，不能由合法长度掩盖半个surrogate。
    pub(super) fn rows_are_valid_for_document(
        rows: &[NativeMarkdownTableRowHeight],
        document: &MarkdownDocumentBuffer,
    ) -> bool {
        Self::rows_are_valid(rows, document.utf16_len())
            && rows.iter().all(|row| {
                // 字节与UTF-16坐标由同一piece tree验证，不建立辅助全文字符串。
                document
                    .byte_offset_for_utf16(row.source_utf16_range.start)
                    .is_ok()
                    && document
                        .byte_offset_for_utf16(row.source_utf16_range.end)
                        .is_ok()
            })
    }

    /// 新视口只收窄同一组已测行时，保留原有限投影，避免离屏行无意义地恢复literal。
    pub(super) fn covers_identical_rows(&self, rows: &[NativeMarkdownTableRowHeight]) -> bool {
        !rows.is_empty()
            && rows.iter().all(|row| {
                self.rows
                    .binary_search_by_key(&row.source_utf16_range.start, |old| {
                        old.source_utf16_range.start
                    })
                    .ok()
                    .is_some_and(|index| self.rows[index] == *row)
            }) // source与最终高度必须逐项完全相同；空输入仍是显式撤销，绝不能沿用旧占位。
    }

    /// 同一来源/宽度/epoch下只合并有限行高元数据；新增测量覆盖旧行，绝不保留正文或Native图。
    pub(super) fn merge_known_rows(
        &self,
        rows: Vec<NativeMarkdownTableRowHeight>,
    ) -> Vec<NativeMarkdownTableRowHeight> {
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
            return rows; // 空输入是撤销，占位不能因历史缓存而继续存在。
        };
        let requested = first.source_utf16_range.start..last.source_utf16_range.end;
        let mut candidates: Vec<_> = self
            .rows
            .iter()
            .filter(|old| {
                !rows.iter().any(|new| {
                    old.source_utf16_range.start < new.source_utf16_range.end
                        && new.source_utf16_range.start < old.source_utf16_range.end
                }) // 新测量与旧行有交集时，旧行整体失权，不能拼接两个高度事实。
            })
            .cloned()
            .map(|row| (row, false))
            .collect(); // 两个原有限输入最多512项；只有标量行高，不随整篇行数增长。
        candidates.extend(rows.into_iter().map(|row| (row, true))); // 旧行筛选的只读借用结束后才移入新测量。
        while candidates.len() > MAX_PROJECTED_TABLE_ROWS {
            let farthest = candidates
                .iter()
                .enumerate()
                .filter(|(_, (_, required))| !required)
                .max_by_key(|(_, (row, _))| {
                    requested.start.saturating_sub(row.source_utf16_range.end)
                        + row.source_utf16_range.start.saturating_sub(requested.end)
                })
                .map(|(index, _)| index)
                .expect("新测量已经通过256项预算，超额项必包含可撤销的旧元数据");
            candidates.swap_remove(farthest); // 只淘汰最远旧行；本批真实新测量一项也不能丢。
        }
        candidates.sort_unstable_by_key(|(row, _)| row.source_utf16_range.start);
        candidates.into_iter().map(|(row, _)| row).collect()
    }

    /// 返回当前行或相邻行之间的属性边界，避免有效范围跨越不同行高。
    pub(super) fn attributes_at(
        &self,
        index: usize,
        document_length: usize,
    ) -> (Option<f64>, std::ops::Range<usize>) {
        let next = self
            .rows
            .partition_point(|row| row.source_utf16_range.end <= index);
        if let Some(row) = self.rows.get(next)
            && row.source_utf16_range.contains(&index)
        {
            // 当前行必须把属性 effective range 限制在其真实源范围内。
            return (Some(row.height), row.source_utf16_range.clone());
        }
        // 投影之外仍用正常默认属性，但不能让该 run 覆盖相邻的已测高行。
        let start = next
            .checked_sub(1)
            .map_or(0, |previous| self.rows[previous].source_utf16_range.end);
        let end = self
            .rows
            .get(next)
            .map_or(document_length, |row| row.source_utf16_range.start);
        (None, start..end)
    }
}

/// 同一行以最高完整单元格为准；向上取整防止最后一行字形被小数误差裁掉。
pub(super) fn native_markdown_measured_table_row_height(
    cell_heights: impl IntoIterator<Item = f64>,
) -> Option<f64> {
    let mut height = NATIVE_MARKDOWN_TABLE_ROW_HEIGHT;
    for content_height in cell_heights {
        // 无效测量必须让调用方保留 source，不能通过 clamp 把失败伪装成38点。
        if !content_height.is_finite() || content_height < 0.0 {
            return None;
        }
        height =
            height.max((content_height + NATIVE_MARKDOWN_TABLE_CELL_VERTICAL_PADDING * 2.0).ceil());
    }
    height.is_finite().then_some(height)
}

/// 实际测量使用最终列宽减padding；零宽或NaN不具有覆盖source的权限。
pub(super) fn native_markdown_table_content_width(column_width: f64) -> Option<f64> {
    let width = column_width - NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING * 2.0;
    (width.is_finite() && width > 0.0).then_some(width)
}

#[cfg(test)]
mod row_height_tests {
    use super::*;

    fn row(range: std::ops::Range<usize>, height: f64) -> NativeMarkdownTableRowHeight {
        NativeMarkdownTableRowHeight {
            source_utf16_range: range,
            height,
        }
    }

    #[test]
    fn narrowing_the_viewport_keeps_identical_heights_but_never_suppresses_revocation() {
        let projection = NativeMarkdownTableRowHeightProjection {
            document_identity: WeakMarkdownDocument::new(),
            document_generation: 1,
            width_key: 100,
            epoch: 2,
            rows: vec![row(10..20, 38.0), row(30..40, 124.0), row(50..60, 38.0)],
        };
        assert!(projection.covers_identical_rows(&[row(30..40, 124.0)])); // 后继小视口不必撤旧屏外行。
        assert!(!projection.covers_identical_rows(&[])); // literal回退仍必须真实撤销全部占位。
        assert!(!projection.covers_identical_rows(&[row(30..40, 125.0)])); // 重测变化不能被旧高度吞掉。
        assert!(!projection.covers_identical_rows(&[row(30..39, 124.0)])); // 范围包含不是同一个完整源行。
        assert!(!projection.covers_identical_rows(&[row(70..80, 38.0)])); // 新进入的行仍须正常安装。
    }

    #[test]
    fn scrolling_retains_bounded_known_heights_and_new_measurements_win() {
        let projection = NativeMarkdownTableRowHeightProjection {
            document_identity: WeakMarkdownDocument::new(),
            document_generation: 1,
            width_key: 100,
            epoch: 2,
            rows: (0..256).map(|i| row(i * 10..i * 10 + 5, 38.0)).collect(),
        };
        let merged =
            projection.merge_known_rows(vec![row(2550..2555, 124.0), row(2560..2565, 38.0)]);
        assert_eq!(merged.len(), 256); // 保留原预算，不能随滚动累计整篇高度。
        assert_eq!(merged.first().unwrap().source_utf16_range, 10..15); // 远端旧行被淘汰。
        assert_eq!(merged[254], row(2550..2555, 124.0)); // 相同行的真实重测覆盖旧值。
        assert_eq!(merged[255], row(2560..2565, 38.0));
        assert!(NativeMarkdownTableRowHeightProjection::rows_are_valid(
            &merged, 2565
        ));
        assert!(projection.merge_known_rows(Vec::new()).is_empty()); // literal/关闭撤销不保留缓存。
    }

    #[test]
    fn tallest_complete_cell_sets_row_height_with_padding_and_upward_rounding() {
        // 短格不能裁最高格；小数高度向上而不是向下舍入，空格仍有可命中的最小行槽。
        assert_eq!(
            native_markdown_measured_table_row_height([14.0, 79.25, 31.0]),
            Some(96.0)
        );
        assert_eq!(native_markdown_measured_table_row_height([0.0]), Some(38.0));
        assert_eq!(native_markdown_measured_table_row_height([]), Some(38.0));
        assert_eq!(native_markdown_measured_table_row_height([f64::NAN]), None);
        assert_eq!(
            native_markdown_measured_table_row_height([f64::INFINITY]),
            None
        );
        assert_eq!(native_markdown_measured_table_row_height([-1.0]), None);
    }

    #[test]
    fn final_column_width_drives_measurement_instead_of_natural_width() {
        // 一列缩窄后有效宽度必须同步减小；此前固定280测高会漏掉窄列新增视觉行。
        assert_eq!(native_markdown_table_content_width(280.0), Some(260.0));
        assert_eq!(native_markdown_table_content_width(84.0), Some(64.0));
        assert_eq!(native_markdown_table_content_width(20.0), None);
        assert_eq!(native_markdown_table_content_width(f64::NAN), None);
        assert_eq!(native_markdown_table_content_width(f64::INFINITY), None);
    }

    #[test]
    fn row_and_gap_attribute_ranges_never_cross_a_different_height() {
        let projection = NativeMarkdownTableRowHeightProjection {
            document_identity: WeakMarkdownDocument::new(),
            document_generation: 1,
            width_key: 100,
            epoch: 2,
            rows: vec![row(10..20, 80.0), row(30..40, 120.0)],
        };
        // 行内与行间分别有正确effective边界，防止TextKit缓存一个高度覆盖后一行。
        assert_eq!(projection.attributes_at(0, 50), (None, 0..10));
        assert_eq!(projection.attributes_at(10, 50), (Some(80.0), 10..20));
        assert_eq!(projection.attributes_at(19, 50), (Some(80.0), 10..20));
        assert_eq!(projection.attributes_at(20, 50), (None, 20..30));
        assert_eq!(projection.attributes_at(30, 50), (Some(120.0), 30..40));
        assert_eq!(projection.attributes_at(49, 50), (None, 40..50));
    }

    #[test]
    fn malformed_or_overbudget_projection_is_rejected_as_a_whole() {
        // delimiter可占1点；正文行由独立max-height计算保证最小38点。
        assert!(NativeMarkdownTableRowHeightProjection::rows_are_valid(
            &[row(0..4, 38.0), row(4..8, 1.0)],
            8
        ));
        assert!(!NativeMarkdownTableRowHeightProjection::rows_are_valid(
            &[row(0..5, 38.0), row(4..8, 60.0)],
            8
        ));
        assert!(!NativeMarkdownTableRowHeightProjection::rows_are_valid(
            &[row(0..9, 38.0)],
            8
        ));
        assert!(!NativeMarkdownTableRowHeightProjection::rows_are_valid(
            &[row(4..4, 38.0)],
            8
        ));
        assert!(!NativeMarkdownTableRowHeightProjection::rows_are_valid(
            &[row(0..4, f64::NAN)],
            8
        ));
        let rows: Vec<_> = (0..=MAX_PROJECTED_TABLE_ROWS)
            .map(|index| row(index..index + 1, 38.0))
            .collect();
        assert!(!NativeMarkdownTableRowHeightProjection::rows_are_valid(
            &rows,
            rows.len()
        ));
    }

    #[test]
    fn projection_rejects_ranges_splitting_a_canonical_surrogate_pair() {
        // 仅使用合成文件与Rust文档，不构造AppKit或读取个人正文。
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unicode.md");
        std::fs::write(&path, "😀x\nnext\n").unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1_024).unwrap();
        assert!(
            NativeMarkdownTableRowHeightProjection::rows_are_valid_for_document(
                &[row(0..4, 80.0)],
                &document
            )
        );
        assert!(
            !NativeMarkdownTableRowHeightProjection::rows_are_valid_for_document(
                &[row(1..4, 80.0)],
                &document
            )
        );
        assert!(
            !NativeMarkdownTableRowHeightProjection::rows_are_valid_for_document(
                &[row(0..1, 80.0)],
                &document
            )
        );
    }
}

#[cfg(test)]
mod projection_identity_tests {
    use super::*;

    #[test]
    fn identity_generation_width_and_epoch_all_revoke_old_geometry() {
        // 同代次的两个真实Rust文档与过期epoch均不能复用同一组高度。
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.md");
        std::fs::write(&path, "row\n").unwrap();
        let first = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(&path, 1_024).unwrap(),
        ));
        let second = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(&path, 1_024).unwrap(),
        ));
        let identity = Rc::downgrade(&first);
        let projection = NativeMarkdownTableRowHeightProjection {
            document_identity: identity.clone(),
            document_generation: 0,
            width_key: 100,
            epoch: 5,
            rows: Vec::new(),
        };
        assert!(projection.matches(&identity, 0, 100, 5));
        assert!(!projection.matches(&Rc::downgrade(&second), 0, 100, 5));
        assert!(!projection.matches(&identity, 1, 100, 5));
        assert!(!projection.matches(&identity, 0, 101, 5));
        assert!(!projection.matches(&identity, 0, 100, 6));
        // 投影只有Weak，不能因元数据让正文和映射超过owner寿命。
        drop(first);
        assert!(projection.document_identity.upgrade().is_none());
    }
}

/// Selects one direction in the canonical structural history, independent of AppKit undo storage.
#[derive(Clone, Copy)]
pub(super) enum MarkdownHistoryDirection {
    Undo,
    Redo,
}

/// Identifies a semantic cell after reparsing; no cached byte offset survives a preceding cell edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeMarkdownTableCellCoordinate {
    pub(super) table_utf16_start: usize,
    pub(super) row_index: usize,
    pub(super) column_index: usize,
}

/// Owns only one visible cell's derived geometry and immutable attributed display text.
#[derive(Clone)]
pub(super) struct NativeMarkdownVisibleTableCellLayout {
    pub(super) coordinate: NativeMarkdownTableCellCoordinate,
    pub(super) source_utf16_range: std::ops::Range<usize>,
    pub(super) frame: NSRect,
    pub(super) text_frame: NSRect,
    pub(super) is_header: bool,
    pub(super) alignment: MarkdownTableAlignment,
    // 保留原唯一有限显示值；frame 从此值排版，实际 Native/语义验收继续读取同一属性值。
    pub(super) attributed_text: Retained<NSAttributedString>,
    // 原可见属性值由 CTFrame 持有；命中、绘制与行高消费同一份实际排版。
    pub(super) text_layout: Rc<super::table_editor::NativeMarkdownTableCellTextLayout>,
    pub(super) source_spans: Rc<[super::text_storage::NativeMarkdownTableCellSourceSpan]>,
}

impl NativeMarkdownVisibleTableCellLayout {
    /// 原 preview 的实际 glyph 坐标先映射到 display，再沿同流生成的有限 run 回到源码。
    pub(super) fn source_index_at_point(&self, point: NSPoint) -> Option<usize> {
        let display = self.text_layout.display_index_at_point(
            point,
            self.text_frame,
            self.attributed_text.length(),
        )?;
        native_markdown_table_cell_source_index(&self.source_spans, display)
            .or_else(|| {
                self.source_spans
                    .is_empty()
                    .then_some(self.source_utf16_range.start)
            })
            .filter(|source| {
                self.source_utf16_range.contains(source) || *source == self.source_utf16_range.end
            })
    }
}

/// 隐藏标记产生的断点向下一个可见 scalar 取亲和；EOF 保留末个真实可见字符的尾端。
fn native_markdown_table_cell_source_index(
    spans: &[super::text_storage::NativeMarkdownTableCellSourceSpan],
    display: usize,
) -> Option<usize> {
    for span in spans {
        if display < span.display_utf16_range.end {
            let offset = display.checked_sub(span.display_utf16_range.start)?;
            return span.source_utf16_range.start.checked_add(offset);
        }
    }
    spans
        .last()
        .filter(|span| display == span.display_utf16_range.end)
        .map(|span| span.source_utf16_range.end)
}

#[cfg(test)]
mod table_cell_source_pointer_tests {
    use super::*;

    #[test]
    /// 原 glyph 的 display caret 必须越过隐藏语法回到原 source；EOF 不吞掉末尾标记。
    fn display_caret_preserves_hidden_marker_and_escape_source_positions() {
        use super::super::text_storage::NativeMarkdownTableCellSourceSpan as Span;
        let spans = [
            Span {
                display_utf16_range: 0..4,
                source_utf16_range: 12..16,
            },
            Span {
                display_utf16_range: 4..5,
                source_utf16_range: 19..20,
            },
        ];
        // 第一个 run 可包含中文及两 UTF16 的 emoji；这里消费原 CTLine 已给出的 scalar caret。
        assert_eq!(native_markdown_table_cell_source_index(&spans, 0), Some(12));
        assert_eq!(native_markdown_table_cell_source_index(&spans, 2), Some(14));
        assert_eq!(native_markdown_table_cell_source_index(&spans, 4), Some(19));
        assert_eq!(native_markdown_table_cell_source_index(&spans, 5), Some(20));
        assert_eq!(native_markdown_table_cell_source_index(&spans, 6), None);
        assert_eq!(native_markdown_table_cell_source_index(&[], 0), None);
    }
}

/// Supplies background/header geometry for a row already selected by bounded viewport culling.
pub(super) struct NativeMarkdownVisibleTableRowLayout {
    pub(super) row_index: usize,
    // 行高投影与实际 TextKit fragment 必须指向同一 canonical 行。
    pub(super) source_utf16_range: std::ops::Range<usize>,
    pub(super) frame: NSRect,
    pub(super) is_header: bool,
}

/// Groups visible row/cell geometry under one table source identity and column coordinate system.
pub(super) struct NativeMarkdownVisibleTableLayout {
    pub(super) table_utf16_start: usize,
    // delimiter也只有在同一完整批次获准后才隐藏，准备失败时保留可读原文。
    pub(super) delimiter_utf16_range: std::ops::Range<usize>,
    pub(super) frame: NSRect,
    pub(super) rows: Vec<NativeMarkdownVisibleTableRowLayout>,
    pub(super) column_offsets: Vec<f64>,
    pub(super) cells: Vec<NativeMarkdownVisibleTableCellLayout>,
}
/// Stable identity of one table layout request.
///
/// TextKit may reenter drawRect: repeatedly. Unchanged generation, viewport and width
/// perform at most one layout, including empty/failure results, to avoid idle wakeups.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeMarkdownTableLayoutRequestKey {
    pub(super) document_generation: u64,
    pub(super) document_length: usize,
    pub(super) presentation_mode: MarkdownPresentationMode,
    pub(super) visible_origin_x: i64,
    pub(super) visible_origin_y: i64,
    pub(super) visible_width: i64,
    pub(super) visible_height: i64,
    pub(super) text_view_width: i64,
    pub(super) table_geometry_epoch: u64,
    // 相同屏幕坐标可能已映射到不同正文；缓存必须同时绑定实际source viewport。
    pub(super) native_viewport_start: usize,
    pub(super) native_viewport_end: usize,
}

/// 用已验证的cell真实矩形计算最小滚动；超高/超宽格显示其起点而非隐藏开头。
pub(super) fn native_markdown_minimal_cell_scroll_origin(
    cell: NSRect,
    visible: NSRect,
) -> Option<NSPoint> {
    fn axis(start: f64, size: f64, clip: f64, extent: f64) -> Option<f64> {
        if !start.is_finite()
            || !size.is_finite()
            || !clip.is_finite()
            || !extent.is_finite()
            || size < 0.0
            || extent <= 0.0
        {
            // 非有限或退化几何没有滚动权限。
            return None;
        }
        let end = start + size;
        if !end.is_finite() || !(clip + extent).is_finite() {
            return None;
        }
        Some(if start < clip || size > extent {
            // 格起点在上/左侧或格本身超过viewport，优先让起点可见。
            start
        } else if end > clip + extent {
            // 只有下/右侧越界，移动恰好露出末端的距离。
            end - extent
        } else {
            // 已完整可见不产生任何viewport变化。
            clip
        })
    }
    Some(NSPoint::new(
        axis(
            cell.origin.x,
            cell.size.width,
            visible.origin.x,
            visible.size.width,
        )?,
        axis(
            cell.origin.y,
            cell.size.height,
            visible.origin.y,
            visible.size.height,
        )?,
    ))
}

/// 一次同步动作的几何连续性；只含Weak与标量，不保活正文或原生段落。
pub(super) struct NativeMarkdownTableMutationAnchor {
    pub(super) identity: WeakMarkdownDocument,
    pub(super) generation: u64,
    pub(super) width: i64,
    pub(super) mode: MarkdownPresentationMode,
    pub(super) input_epoch: u64,
    pub(super) paragraph: std::ops::Range<usize>,
    pub(super) viewport: std::ops::Range<usize>,
    pub(super) screen_y: f64,
}

impl NativeMarkdownTableMutationAnchor {
    /// 仅同一文档、代次与显示合同拥有消费权限；Weak地址不能代替存活owner。
    pub(super) fn matches(
        &self,
        identity: &WeakMarkdownDocument,
        key: NativeMarkdownTableLayoutRequestKey,
        input_epoch: u64,
    ) -> bool {
        self.input_epoch == input_epoch
            && std::rc::Weak::ptr_eq(&self.identity, identity)
            && self.identity.upgrade().is_some()
            && self.generation == key.document_generation
            && self.width == key.text_view_width
            && self.mode == key.presentation_mode
    }

    /// 只映射存活段落的完整两端；被删除的锚点不猜成相邻文字。
    pub(super) fn advance(
        &mut self,
        replaced: &std::ops::Range<usize>,
        delta: isize,
        after_generation: u64,
    ) -> Option<()> {
        if self.generation.checked_add(1)? != after_generation {
            // 原生重入多次修改也不能把本动作旧坐标冒充新generation。
            return None;
        }
        let paragraph = native_markdown_map_surviving_range(&self.paragraph, replaced, delta)?;
        let viewport = native_markdown_map_surviving_range(&self.viewport, replaced, delta)?;
        self.paragraph = paragraph;
        self.viewport = viewport;
        self.generation = after_generation;
        Some(())
    }
}

/// 修改完全位于内部或外部才有明确映射；部分删除边界的范围失效。
pub(super) fn native_markdown_map_surviving_range(
    source: &std::ops::Range<usize>,
    replaced: &std::ops::Range<usize>,
    delta: isize,
) -> Option<std::ops::Range<usize>> {
    if source.start >= source.end
        || replaced.start > replaced.end
        || replaced
            .end
            .checked_sub(replaced.start)?
            .checked_add_signed(delta)
            .is_none()
    {
        // 反向、空锚点或负replacement长度均不能成为有效receipt。
        return None;
    }
    if source.end <= replaced.start {
        // 修改在此段之后；屏幕锚点source不变。
        return Some(source.clone());
    }
    if source.start >= replaced.end {
        // 修改在此段之前；两端用checked有符号位移。
        return Some(
            source.start.checked_add_signed(delta)?..source.end.checked_add_signed(delta)?,
        );
    }
    if source.start < replaced.start && source.end >= replaced.end {
        // cell内部修改保持paragraph起点，末端和viewport末端跟随真实delta。
        return Some(source.start..source.end.checked_add_signed(delta)?);
    }
    None
}

#[cfg(test)]
mod mutation_anchor_tests {
    use super::*;

    #[test]
    fn visible_cell_does_not_scroll_and_offscreen_cell_moves_only_necessary_distance() {
        let viewport = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(500.0, 600.0));
        let cell = |y, height| NSRect::new(NSPoint::new(20.0, y), NSSize::new(100.0, height));
        assert_eq!(
            native_markdown_minimal_cell_scroll_origin(cell(200.0, 124.0), viewport),
            Some(NSPoint::new(0.0, 100.0))
        );
        assert_eq!(
            native_markdown_minimal_cell_scroll_origin(cell(650.0, 124.0), viewport),
            Some(NSPoint::new(0.0, 174.0))
        );
        assert_eq!(
            native_markdown_minimal_cell_scroll_origin(cell(50.0, 124.0), viewport),
            Some(NSPoint::new(0.0, 50.0))
        );
        assert_eq!(
            native_markdown_minimal_cell_scroll_origin(cell(650.0, 1200.0), viewport),
            Some(NSPoint::new(0.0, 650.0))
        );
        assert_eq!(
            native_markdown_minimal_cell_scroll_origin(cell(f64::NAN, 100.0), viewport),
            None
        );
    }

    #[test]
    fn real_row148_shortening_preserves_row145_and_maps_viewport_end() {
        assert_eq!(
            native_markdown_map_surviving_range(&(13501..13655), &(13800..13900), -76),
            Some(13501..13655)
        );
        assert_eq!(
            native_markdown_map_surviving_range(&(13438..14495), &(13800..13900), -76),
            Some(13438..14419)
        );
        assert_eq!(
            native_markdown_map_surviving_range(&(13781..13935), &(13800..13900), -76),
            Some(13781..13859)
        );
        // Undo插回同一内部范围，原viewport范围精确恢复。
        assert_eq!(
            native_markdown_map_surviving_range(&(13438..14419), &(13800..13824), 76),
            Some(13438..14495)
        );
    }

    #[test]
    fn deleted_boundary_overflow_and_impossible_delta_fail_closed() {
        assert_eq!(
            native_markdown_map_surviving_range(&(10..20), &(5..15), -10),
            None
        );
        assert_eq!(
            native_markdown_map_surviving_range(&(10..20), &(10..20), -10),
            None
        );
        assert_eq!(
            native_markdown_map_surviving_range(&(usize::MAX - 3..usize::MAX), &(0..1), 10),
            None
        );
        assert_eq!(
            native_markdown_map_surviving_range(&(20..30), &(0..1), -2),
            None
        );
        assert_eq!(
            native_markdown_map_surviving_range(&(20..30), &(0..1), -1),
            Some(19..29)
        );
    }

    #[test]
    fn owner_generation_width_and_mode_each_revoke_mutation_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("anchor.md");
        std::fs::write(&path, "paragraph\n").unwrap();
        let first = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(&path, 1024).unwrap(),
        ));
        let second = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(&path, 1024).unwrap(),
        ));
        let identity = Rc::downgrade(&first);
        let mut anchor = NativeMarkdownTableMutationAnchor {
            identity: identity.clone(),
            generation: 0,
            width: 100,
            mode: MarkdownPresentationMode::InlinePreview,
            input_epoch: 7,
            paragraph: 0..10,
            viewport: 0..10,
            screen_y: 2.0,
        };
        let key = NativeMarkdownTableLayoutRequestKey {
            document_generation: 0,
            document_length: 10,
            presentation_mode: MarkdownPresentationMode::InlinePreview,
            visible_origin_x: 0,
            visible_origin_y: 0,
            visible_width: 100,
            visible_height: 100,
            text_view_width: 100,
            table_geometry_epoch: 0,
            native_viewport_start: 0,
            native_viewport_end: 10,
        };
        assert!(anchor.matches(&identity, key, 7));
        assert!(!anchor.matches(&identity, key, 8));
        assert!(!anchor.matches(&Rc::downgrade(&second), key, 7));
        assert!(!anchor.matches(
            &identity,
            NativeMarkdownTableLayoutRequestKey {
                document_generation: 1,
                ..key
            },
            7
        ));
        assert!(!anchor.matches(
            &identity,
            NativeMarkdownTableLayoutRequestKey {
                text_view_width: 101,
                ..key
            },
            7
        ));
        assert!(!anchor.matches(
            &identity,
            NativeMarkdownTableLayoutRequestKey {
                presentation_mode: MarkdownPresentationMode::Source,
                ..key
            },
            7
        ));
        assert!(anchor.advance(&(2..3), 1, 2).is_none());
        assert_eq!(anchor.generation, 0);
        assert!(anchor.advance(&(2..3), 1, 1).is_some());
        assert_eq!(anchor.paragraph, 0..11);
        assert!(anchor.advance(&(2..4), -1, 2).is_some());
        assert_eq!(anchor.paragraph, 0..10);
        drop(first);
        assert!(!anchor.matches(
            &identity,
            NativeMarkdownTableLayoutRequestKey {
                document_generation: 2,
                ..key
            },
            7
        ));
    }
}

/// 私有候选的有限失败分类；任何失败都不构成完整preview通过。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeMarkdownStableTableFailure {
    /// 新事件/文档/宽度已撤权，旧局部结果只能丢弃，不能恢复clip或发布literal覆盖新状态。
    InputIntentExpired,
    MissingGeometry,
    InvalidSourceAssociation,
    NonMonotonicFrames,
    UnstableFrames,
    AnchorUnavailable,
    AnchorUnstable,
    CoverageOutsideBudget,
    PreparationPassLimit,
}

/// 单次表格原生步骤前后都检查同一意图；返回值未获准时在无状态借用的本栈释放。
pub(super) fn native_markdown_table_intent_step<T>(
    is_current: &impl Fn() -> bool,
    operation: impl FnOnce() -> T,
) -> Result<T, NativeMarkdownStableTableFailure> {
    if !is_current() {
        // 前一步已发生新输入，不再执行任何旧布局或旧clip操作。
        return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
    }
    let value = operation();
    if !is_current() {
        // 原生调用可重入；旧值不传给下一步，也不为恢复旧位置重试。
        return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
    }
    Ok(value)
}

#[cfg(test)]
mod input_intent_tests {
    use super::*;

    #[test]
    fn cancelled_capture_cannot_be_relabelled_with_the_new_epoch() {
        let epoch = Cell::new(7);
        let expected = epoch.get();
        let result = native_markdown_table_intent_step(&|| epoch.get() == expected, || {
            // 模拟capture的公开getter重入新滚动；旧source值不能贴上epoch8继续使用。
            epoch.set(8);
            13501..13655
        });
        assert!(matches!(
            result,
            Err(NativeMarkdownStableTableFailure::InputIntentExpired)
        ));
    }

    #[test]
    fn input_during_layout_skips_old_restore_without_reverting_new_scroll() {
        let epoch = Cell::new(3);
        let expected = epoch.get();
        let clip = Cell::new(100.0);
        let old_restore_calls = Cell::new(0);
        let is_current = || epoch.get() == expected;
        let layout = native_markdown_table_intent_step(&is_current, || {
            // 模拟native layout内的新用户滚动；此位置属于新意图。
            epoch.set(4);
            clip.set(900.0);
        });
        assert!(layout.is_err());
        let restore = native_markdown_table_intent_step(&is_current, || {
            // 旧continuity若执行将覆盖新意图；门禁必须在调用前拒绝。
            old_restore_calls.set(old_restore_calls.get() + 1);
            clip.set(100.0);
        });
        assert!(restore.is_err());
        assert_eq!(old_restore_calls.get(), 0);
        assert_eq!(clip.get(), 900.0);
    }

    #[test]
    fn tab_scroll_or_reflect_reentry_cannot_begin_old_cell() {
        for cancel_at_scroll in [true, false] {
            // 两个真实native边界分别撤权；任一路径都不能激活旧局部cell。
            let epoch = Cell::new(11);
            let expected = epoch.get();
            let began = Cell::new(false);
            let is_current = || epoch.get() == expected;
            let result = (|| {
                native_markdown_table_intent_step(&is_current, || {
                    if cancel_at_scroll {
                        // clip滚动重入新输入，reflect也不再执行。
                        epoch.set(12);
                    }
                })?;
                native_markdown_table_intent_step(&is_current, || {
                    if !cancel_at_scroll {
                        // scrollbar同步重入新输入，begin也不再执行。
                        epoch.set(12);
                    }
                })?;
                native_markdown_table_intent_step(&is_current, || began.set(true))
            })();
            assert!(result.is_err());
            assert!(!began.get());
        }
    }

    #[test]
    fn unchanged_intent_preserves_native_step_order_and_result() {
        let stages = RefCell::new(Vec::new());
        let value = native_markdown_table_intent_step(&|| true, || {
            // 正常步骤只执行一次，不以修复为由吞掉真实原生操作。
            stages.borrow_mut().push("scroll");
            42
        })
        .unwrap();
        assert_eq!(value, 42);
        assert_eq!(&*stages.borrow(), &["scroll"]);
    }

    #[test]
    fn unpublished_field_candidate_drops_when_creation_loses_intent() {
        struct Candidate<'a>(&'a Cell<usize>);
        impl Drop for Candidate<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1); // 未发布owner只由本栈释放一次。
            }
        }
        let epoch = Cell::new(9);
        let drops = Cell::new(0);
        let result = native_markdown_table_intent_step(&|| epoch.get() == 9, || {
            // 模拟init重入新动作；返回的旧candidate不能发布到field槽。
            epoch.set(10);
            Candidate(&drops)
        });
        assert!(result.is_err());
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn setter_takeover_preserves_new_field_and_active_instead_of_rolling_back() {
        let epoch = Cell::new(21);
        let field = Cell::new("old");
        let active = Cell::new("none");
        let current = || epoch.get() == 21;
        let result = native_markdown_table_intent_step(&current, || {
            // 模拟setter回调内新request接管字段与active，外层不得再hide/clear/abort它。
            epoch.set(22);
            field.set("new draft");
            active.set("new cell");
        });
        assert!(result.is_err());
        let cleanup = native_markdown_table_intent_step(&current, || {
            field.set("");
            active.set("none");
        });
        assert!(cleanup.is_err());
        assert_eq!(field.get(), "new draft");
        assert_eq!(active.get(), "new cell");
    }

    #[test]
    fn revoked_show_does_not_run_old_focus_or_change_new_responder() {
        let epoch = Cell::new(31);
        let responder = Cell::new("new responder");
        let current = || epoch.get() == 31;
        let show = native_markdown_table_intent_step(&current, || {
            // 公开show调用可重入；只验证本项目不再发出旧selectText，不假设AppKit内部原子。
            epoch.set(32);
        });
        assert!(show.is_err());
        let focus = native_markdown_table_intent_step(&current, || responder.set("old field"));
        assert!(focus.is_err());
        assert_eq!(responder.get(), "new responder");
    }
}

/// 原生getter后的标量与Weak；每次只局部load认证活对象，不强持fragment/正文/属性。
#[derive(Clone, Debug)]
pub(super) struct NativeMarkdownResolvedTableRow {
    pub(super) source: std::ops::Range<usize>,
    pub(super) fragment_source: std::ops::Range<usize>,
    pub(super) fragment: Option<Weak<objc2_app_kit::NSTextLayoutFragment>>,
    pub(super) frame: NSRect,
}

/// 只描述可视/命中纵向band；不能反馈为Native占位高度，也不改变完整列宽。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct NativeMarkdownTableHeaderBand {
    pub(super) origin_y: f64,
    pub(super) height: f64,
}

/// 输入先经共享paragraph reader和两矩阵身份认证；这里只组合实际相接的隐藏delimiter。
pub(super) fn native_markdown_table_header_band(
    header: &NativeMarkdownResolvedTableRow,
    delimiter: &NativeMarkdownResolvedTableRow,
    first_body: Option<&NativeMarkdownResolvedTableRow>,
    delimiter_is_hidden: bool,
) -> Option<NativeMarkdownTableHeaderBand> {
    let bottom = |row: &NativeMarkdownResolvedTableRow| row.frame.origin.y + row.frame.size.height;
    if !delimiter_is_hidden
        || header.source.is_empty()
        || delimiter.source.is_empty()
        || header.fragment_source.end != delimiter.fragment_source.start
        || !header.frame.origin.y.is_finite()
        || !header.frame.size.height.is_finite()
        || header.frame.size.height <= 0.0
        || !delimiter.frame.origin.y.is_finite()
        || !delimiter.frame.size.height.is_finite()
        || (delimiter.frame.size.height - NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT).abs() > 0.01
        || !bottom(header).is_finite()
        || !bottom(delimiter).is_finite()
        || (bottom(header) - delimiter.frame.origin.y).abs() > 0.01
    {
        // 没有实际隐藏段、source不接合、估计大空洞或重叠均不能冒充1pt分隔线。
        return None;
    }
    if let Some(body) = first_body
        && (delimiter.fragment_source.end != body.fragment_source.start
            || !body.frame.origin.y.is_finite()
            || (bottom(delimiter) - body.frame.origin.y).abs() > 0.01)
    {
        // 首body存在时也须接合，不能用扩张header掩盖错段或空洞。
        return None;
    }
    Some(NativeMarkdownTableHeaderBand {
        origin_y: header.frame.origin.y,
        height: bottom(delimiter) - header.frame.origin.y,
    })
}

/// 内容始终来自原Native行槽；可视band吸收delimiter也不能改变字形/输入控件的纵向空间。
pub(super) fn native_markdown_table_native_content_y_height(frame: NSRect) -> Option<(f64, f64)> {
    let y = frame.origin.y + NATIVE_MARKDOWN_TABLE_CELL_VERTICAL_PADDING;
    let height = frame.size.height - NATIVE_MARKDOWN_TABLE_CELL_VERTICAL_PADDING * 2.0;
    (y.is_finite() && height.is_finite() && height > 0.0).then_some((y, height))
}

/// Native完整paragraph仅可比语义行多一个合法换行；delimiter与可见行共用同一source规则。
pub(super) fn native_markdown_table_paragraph_tail_is_valid(
    source: &std::ops::Range<usize>,
    native: &std::ops::Range<usize>,
    unit: impl Fn(usize) -> Option<u16>,
) -> bool {
    if source.is_empty() || native.start != source.start || native.end < source.end {
        return false; // 空行、错误起点或截断范围不属于本语义paragraph。
    }
    match native.end - source.end {
        0 => true,                                                        // EOF可没有终止换行。
        1 => matches!(unit(source.end), Some(10 | 13 | 0x2028 | 0x2029)), // 单个合法换行。
        2 => unit(source.end) == Some(13) && unit(source.end + 1) == Some(10), // CRLF不能当两个段落。
        _ => false, // 额外文字或多个段落不能被隐藏在尾部容差中。
    }
}

/// 完整 delimiter 的每个 run 都必须由原不可变发布证明隐藏，不能只查两端或后继可变缓存。
pub(super) fn native_markdown_table_delimiter_semantics_are_hidden(
    presentation: &MarkdownViewportPresentationSnapshot,
    generation: u64,
    range: &std::ops::Range<usize>,
) -> bool {
    if presentation.generation() != generation || range.start >= range.end {
        return false; // 旧代次、反向或空范围均没有真实可隐藏字符。
    }
    let mut cursor = range.start;
    while cursor < range.end {
        let Some(query) = presentation.query(cursor) else {
            return false; // 任一中间覆盖缺失都不能用两端隐藏的事实补齐。
        };
        if !query.utf16_range.contains(&cursor)
            || !query
                .styles
                .contains(MarkdownPresentationStyles::TABLE_DELIMITER)
            || !query
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        {
            return false; // 中间默认 gap、已显露标记或不推进的异常 run 取消整个 band。
        }
        cursor = query.utf16_range.end.min(range.end);
    }
    true
}

#[cfg(test)]
mod delimiter_band_tests {
    use super::*;

    /// 测试也先取得真实完整解析快照；fallback 与 Source 不能制造隐藏权限。
    fn current_delimiter_is_hidden(
        presentation: &MarkdownViewportPresentation,
        generation: u64,
        range: &std::ops::Range<usize>,
    ) -> bool {
        presentation
            .semantic_snapshot()
            .as_ref()
            .is_some_and(|snapshot| {
                native_markdown_table_delimiter_semantics_are_hidden(snapshot, generation, range)
            })
    }

    fn paragraph(
        source: std::ops::Range<usize>,
        end: usize,
        y: f64,
        height: f64,
    ) -> NativeMarkdownResolvedTableRow {
        NativeMarkdownResolvedTableRow {
            fragment_source: source.start..end,
            source,
            fragment: None,
            frame: NSRect::new(NSPoint::new(36.0, y), NSSize::new(638.0, height)),
        }
    }

    fn rows() -> [NativeMarkdownResolvedTableRow; 3] {
        [
            paragraph(9..103, 104, 91.64, 50.0),
            paragraph(104..149, 150, 141.64, 1.0),
            paragraph(150..229, 230, 142.64, 52.0),
        ]
    }

    #[test]
    fn actual_one_point_delimiter_closes_only_header_visual_band() {
        let [header, delimiter, body] = rows();
        let band =
            native_markdown_table_header_band(&header, &delimiter, Some(&body), true).unwrap();
        assert!((band.origin_y - 91.64).abs() < 0.001);
        assert!((band.height - 51.0).abs() < 0.001);
        assert!((band.origin_y + band.height - body.frame.origin.y).abs() < 0.001);
        // 原Native行高和内容区不变，完整列宽也不是tight glyph width。
        assert_eq!(header.frame.size.height, 50.0);
        let (y, height) = native_markdown_table_native_content_y_height(header.frame).unwrap();
        assert!((y - 99.64).abs() < 0.001);
        assert_eq!(height, 34.0);
        assert_eq!(header.frame.size.width, 638.0);
    }

    #[test]
    fn repeated_batches_do_not_feed_visual_height_back_into_native_height() {
        let [header, delimiter, body] = rows();
        for _ in 0..4 {
            // 每批只读同一Native50+delimiter1，不把上批band51作为下批Native输入。
            let band =
                native_markdown_table_header_band(&header, &delimiter, Some(&body), true).unwrap();
            assert!((band.height - 51.0).abs() < 0.001);
            assert_eq!(header.frame.size.height, 50.0);
        }
    }

    #[test]
    fn header_only_band_ends_at_real_delimiter_without_requiring_another_body() {
        let [header, delimiter, _] = rows();
        let band = native_markdown_table_header_band(&header, &delimiter, None, true).unwrap();
        assert!(
            (band.origin_y + band.height
                - (delimiter.frame.origin.y + delimiter.frame.size.height))
                .abs()
                < 0.001
        );
    }

    #[test]
    fn visible_or_wrong_source_delimiter_cannot_be_covered() {
        let [header, mut delimiter, body] = rows();
        assert!(
            native_markdown_table_header_band(&header, &delimiter, Some(&body), false).is_none()
        );
        delimiter.fragment_source.start += 1;
        // 即使像素位置恰好相接，别的source也没有隐藏权限。
        assert!(
            native_markdown_table_header_band(&header, &delimiter, Some(&body), true).is_none()
        );
    }

    #[test]
    fn two_point_gap_overlap_and_wrong_body_source_fail_instead_of_shifting_rows() {
        let [header, mut delimiter, mut body] = rows();
        delimiter.frame.size.height = 2.0;
        body.frame.origin.y += 1.0;
        assert!(
            native_markdown_table_header_band(&header, &delimiter, Some(&body), true).is_none()
        );
        delimiter.frame.size.height = 1.0;
        delimiter.frame.origin.y -= 1.0;
        assert!(
            native_markdown_table_header_band(&header, &delimiter, Some(&body), true).is_none()
        );
        delimiter.frame.origin.y += 1.0;
        body.frame.origin.y -= 1.0;
        body.fragment_source.start += 1;
        assert!(
            native_markdown_table_header_band(&header, &delimiter, Some(&body), true).is_none()
        );
    }

    #[test]
    fn shared_paragraph_tail_accepts_lf_crlf_and_rejects_extra_text() {
        for ending in ["\n", "\r\n", "\r", "\u{2028}", "\u{2029}"] {
            // 原始UTF16坐标含emoji，尾规则不依赖UTF8字节长度。
            let text = format!("😀| a |{ending}");
            let units: Vec<_> = text.encode_utf16().collect();
            let end = units.len() - ending.encode_utf16().count();
            assert!(native_markdown_table_paragraph_tail_is_valid(
                &(0..end),
                &(0..units.len()),
                |i| units.get(i).copied()
            ));
        }
        let units: Vec<_> = "| a |xx".encode_utf16().collect();
        assert!(!native_markdown_table_paragraph_tail_is_valid(
            &(0..5),
            &(0..7),
            |i| units.get(i).copied()
        ));
        assert!(!native_markdown_table_paragraph_tail_is_valid(
            &(0..5),
            &(1..7),
            |i| units.get(i).copied()
        ));
    }

    #[test]
    fn only_current_preview_delimiter_semantics_allow_the_band() {
        let source = "before\n\n| A | B |\n| --- | --- |\n| x | y |\n";
        let start = source[..source.find("| ---").unwrap()]
            .encode_utf16()
            .count();
        let end = start + "| --- | --- |".encode_utf16().count();
        let mut presentation = MarkdownViewportPresentation::default();
        let length = source.encode_utf16().count();
        presentation.rebuild(4, 0..length, source).unwrap();
        assert!(current_delimiter_is_hidden(&presentation, 4, &(start..end)));
        assert!(!current_delimiter_is_hidden(
            &presentation,
            5,
            &(start..end)
        ));
        // 原core合同规定表格语法在活动选择时仍隐藏；本候选不擅改该功能。
        presentation.set_active_selection(start..start);
        presentation.rebuild(4, 0..length, source).unwrap();
        assert!(current_delimiter_is_hidden(&presentation, 4, &(start..end)));
        presentation
            .record_source_fallback_window(4, 0..length)
            .unwrap();
        assert!(!current_delimiter_is_hidden(
            &presentation,
            4,
            &(start..end)
        ));
        presentation.set_mode(MarkdownPresentationMode::Source);
        presentation.rebuild(4, 0..length, source).unwrap();
        assert!(!current_delimiter_is_hidden(
            &presentation,
            4,
            &(start..end)
        ));
    }
}

/// 同一批必须按source严格有序且不重叠，不能仅凭每行height足够接受拼接frame。
pub(super) fn native_markdown_table_matrix_is_ordered(
    rows: &[NativeMarkdownResolvedTableRow],
) -> bool {
    let mut previous_end = 0;
    let mut previous_bottom = f64::NEG_INFINITY;
    for row in rows {
        if row.source.start >= row.source.end
            || row.source.start < previous_end
            || row.fragment_source.start != row.source.start
            || row.fragment_source.end < row.source.end
            || row.fragment_source.end - row.source.end > 2
            || !row.frame.origin.x.is_finite()
            || !row.frame.size.width.is_finite()
            || row.frame.size.width < 0.0
            || !row.frame.origin.y.is_finite()
            || !row.frame.size.height.is_finite()
            || row.frame.size.height <= 0.0
            || row.frame.origin.y + 0.01 < previous_bottom
        {
            // 0.01点只容忍浮点求和误差，不接受一个物理像素的实际行重叠。
            return false;
        }
        previous_end = row.source.end;
        previous_bottom = row.frame.origin.y + row.frame.size.height;
    }
    true
}

/// 不扩大既有准备预算；连续ensure与两次union都必须在相同source单位上限内。
pub(super) fn native_markdown_bounded_table_layout_range(
    first: &std::ops::Range<usize>,
    second: &std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    if first.start > first.end || second.start > second.end {
        return None;
    }
    let range = first.start.min(second.start)..first.end.max(second.end);
    (range.end.checked_sub(range.start)? <= MAX_PREPARED_TABLE_SOURCE_UTF16_UNITS).then_some(range)
}

/// 完成投影后的可视band必须相接；delimiter已经纳入header，不再为header放宽间隙。
pub(super) fn native_markdown_table_rows_have_contiguous_frames(
    rows: &[NativeMarkdownVisibleTableRowLayout],
) -> bool {
    rows.windows(2).all(|pair| {
        let previous = &pair[0];
        let next = &pair[1];
        if previous.row_index.checked_add(1) != Some(next.row_index) {
            // preparation必须保持连续source行，不能跳过中间内容后拼接两块矩阵。
            return false;
        }
        let gap = next.frame.origin.y - (previous.frame.origin.y + previous.frame.size.height);
        gap.is_finite() && gap.abs() <= 0.01
    })
}

/// 有序连续row索引必须覆盖整个新可见区，防止旧批次尾部空白仍被发布。
pub(super) fn native_markdown_table_rows_cover(
    rows: &[NativeMarkdownVisibleTableRowLayout],
    first: usize,
    end: usize,
) -> bool {
    if first > end {
        // 反向范围不是空可见区。
        return false;
    }
    let count = end - first;
    let at = rows.partition_point(|row| row.row_index < first);
    let Some(last) = at.checked_add(count) else {
        return false;
    };
    rows.get(at..last).is_some_and(|selected| {
        // 不能仅比较首尾；中间缺行仍会造成可见空缺。
        selected
            .iter()
            .enumerate()
            .all(|(offset, row)| row.row_index == first + offset)
    })
}

#[cfg(test)]
mod stable_matrix_tests {
    use super::*;

    fn row(start: usize, y: f64, height: f64) -> NativeMarkdownResolvedTableRow {
        NativeMarkdownResolvedTableRow {
            source: start..start + 8,
            fragment_source: start..start + 9,
            fragment: None,
            frame: NSRect::new(NSPoint::new(0.0, y), NSSize::new(100.0, height)),
        }
    }

    #[test]
    fn actual_198_point_rebase_overlap_is_rejected() {
        // Root真实batch3里旧高度检查接受了这两个有序source的逆序frame。
        assert!(!native_markdown_table_matrix_is_ordered(&[
            row(12_038, 4_093.593_209_358_128, 38.0),
            row(12_101, 3_933.520_920_201_501_7, 124.0),
        ]));
    }

    #[test]
    fn adjacent_fractional_rows_and_one_separator_are_valid() {
        assert!(native_markdown_table_matrix_is_ordered(&[
            row(0, 1.25, 38.0),
            row(10, 39.25, 124.0),
        ]));
        assert!(!native_markdown_table_matrix_is_ordered(&[
            row(0, 1.25, 38.0),
            row(10, 39.0, 124.0),
        ]));
        let mut invalid = row(0, 0.0, 38.0);
        invalid.fragment_source.end = 12;
        assert!(!native_markdown_table_matrix_is_ordered(&[invalid]));
    }

    #[test]
    fn visible_union_cannot_silently_expand_into_whole_document() {
        assert_eq!(
            native_markdown_bounded_table_layout_range(&(100..200), &(150..300)),
            Some(100..300)
        );
        assert_eq!(
            native_markdown_bounded_table_layout_range(&(0..100), &(1_000_000..1_000_100)),
            None
        );
        assert_eq!(
            native_markdown_bounded_table_layout_range(
                &std::ops::Range { start: 20, end: 10 },
                &(0..2)
            ),
            None
        );
    }

    #[test]
    fn monotonic_rows_with_a_large_blank_are_not_a_complete_table() {
        let mut rows: Vec<_> = (1..4)
            .map(|index| NativeMarkdownVisibleTableRowLayout {
                row_index: index,
                source_utf16_range: index * 10..index * 10 + 8,
                frame: NSRect::new(
                    NSPoint::new(0.0, (index - 1) as f64 * 38.0),
                    NSSize::new(100.0, 38.0),
                ),
                is_header: false,
            })
            .collect();
        assert!(native_markdown_table_rows_have_contiguous_frames(&rows));
        // 与重影相反的正向198点空洞同样破坏完整可见性，不能因无重叠而放行。
        rows[2].frame.origin.y += 198.072_289_156_627;
        assert!(!native_markdown_table_rows_have_contiguous_frames(&rows));
        rows[2].frame.origin.y -= 198.072_289_156_627;
        rows[0].row_index = 0;
        rows[1].row_index = 1;
        rows[2].row_index = 2;
        rows[1].frame.origin.y += NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT;
        rows[2].frame.origin.y += NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT;
        // 原Native行槽之间的delimiter不能直接被可视矩阵放过。
        assert!(!native_markdown_table_rows_have_contiguous_frames(&rows));
        rows[0].frame.size.height += NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT;
        // 只有已组合的header band才能与首body共享同一边。
        assert!(native_markdown_table_rows_have_contiguous_frames(&rows));
    }

    #[test]
    fn new_native_viewport_tail_must_be_prepared_before_publication() {
        let rows: Vec<_> = (112..138)
            .map(|index| NativeMarkdownVisibleTableRowLayout {
                row_index: index,
                source_utf16_range: index * 10..index * 10 + 8,
                frame: NSRect::ZERO,
                is_header: false,
            })
            .collect();
        // Root真实旧批112..137不能覆盖新viewport130..143的末六行。
        assert!(native_markdown_table_rows_cover(&rows, 120, 138));
        assert!(!native_markdown_table_rows_cover(&rows, 130, 144));
        assert!(!native_markdown_table_rows_cover(&rows, 140, 130));
        let mut gap = rows;
        gap.remove(15);
        assert!(!native_markdown_table_rows_cover(&gap, 120, 138));
    }
}

/// 保存一个精确代次/视口的不可变投影；主线程 Rc 只在同步载荷租约内共享。
///
/// 不实现 Clone，防止绘制或测量重新深复制 tables/rows/cells 及逐格原生属性引用。
/// 唯一 cache slot 在失效或 accepted release 时取走；局部快照不能排队或跨会话保活。
pub(super) struct NativeMarkdownVisibleTableLayoutCache {
    // Weak 保留分配身份而不保留正文；同 generation 的另一笔记不得复用本投影。
    pub(super) document_identity: WeakMarkdownDocument,
    pub(super) document_generation: u64,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) utf16_window: std::ops::Range<usize>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) viewport_utf16_window: std::ops::Range<usize>,
    pub(super) text_view_width: f64,
    pub(super) tables: Vec<NativeMarkdownVisibleTableLayout>,
    pub(super) request_key: NativeMarkdownTableLayoutRequestKey,
}

/// Stores the active single-cell draft and source range until explicit commit or cancellation.
#[derive(Clone)]
pub(super) struct NativeMarkdownTableCellEditorState {
    // 只有真实RequiredRow的Ready可发布；一次提交后原窗口失效，不用相同整数范围补签。
    pub(super) authority: Option<Rc<NativeMarkdownTableCellAuthority>>,
    pub(super) coordinate: NativeMarkdownTableCellCoordinate,
    pub(super) source_utf16_range: std::ops::Range<usize>,
    pub(super) frame: NSRect,
    pub(super) original_source: String,
    // 只记录真实字段操作已经证明的局部方向；有序 NSRange 或 affinity 不能单独建立 anchor。
    pub(super) field_selection: Option<MarkdownTextSelection>,
    // 仅首次 preview 点击尚未 mouseUp 时持有；新输入/Up/取消就释放真实事件。
    pub(super) pointer: Option<Rc<super::table_editor::NativeMarkdownTableCellPointer>>,
}

/// 原字段的实际范围只续接已证明方向；未知非空范围不猜测其主端点。
pub(super) fn native_markdown_table_field_selection(
    previous: Option<MarkdownTextSelection>,
    range: NSRange,
    explicit: Option<MarkdownTextSelection>,
    extends_previous: bool,
) -> Option<MarkdownTextSelection> {
    let end = range.location.checked_add(range.length)?;
    if range.location == NSNotFound as usize {
        return None; // 系统尚无实际范围，不能把 NSNotFound 当作字段 caret。
    }
    let matches = |selection: MarkdownTextSelection| {
        selection.anchor_utf16.min(selection.head_utf16) == range.location
            && selection.anchor_utf16.max(selection.head_utf16) == end
    };
    if let Some(explicit) = explicit {
        return matches(explicit).then_some(explicit); // 只接受本次真实 Native 操作的完整后果。
    }
    if range.length == 0 {
        return Some(MarkdownTextSelection::caret(range.location)); // 单 caret 自身已证明方向。
    }
    let previous = previous?;
    if matches(previous) {
        return Some(previous); // 范围完全未变，不因纯焦点或普通通知丢失已知方向。
    }
    if !extends_previous {
        return None; // 不同的非空范围需要本次真实扩选事实，不能靠端点偶合续权。
    }
    let head = if previous.anchor_utf16 == range.location {
        end
    } else if previous.anchor_utf16 == end {
        range.location
    } else {
        return None; // 旧 anchor 已不在新范围边缘，实际动作不是已证明的连续 Shift。
    };
    Some(MarkdownTextSelection {
        anchor_utf16: previous.anchor_utf16,
        head_utf16: head,
    })
}

#[cfg(test)]
mod field_selection_tests {
    use super::*;

    #[test]
    fn continuous_shift_preserves_the_proven_reverse_anchor() {
        let original = MarkdownTextSelection {
            anchor_utf16: 9,
            head_utf16: 4,
        };
        assert_eq!(
            native_markdown_table_field_selection(Some(original), NSRange::new(2, 7), None, true),
            Some(MarkdownTextSelection {
                anchor_utf16: 9,
                head_utf16: 2
            })
        );
        assert_eq!(
            native_markdown_table_field_selection(Some(original), NSRange::new(9, 3), None, true),
            Some(MarkdownTextSelection {
                anchor_utf16: 9,
                head_utf16: 12
            })
        );
    }

    #[test]
    fn ordered_range_does_not_invent_a_direction_or_reuse_a_changed_anchor() {
        let original = MarkdownTextSelection {
            anchor_utf16: 9,
            head_utf16: 4,
        };
        assert_eq!(
            native_markdown_table_field_selection(None, NSRange::new(2, 7), None, true),
            None
        );
        assert_eq!(
            native_markdown_table_field_selection(Some(original), NSRange::new(1, 4), None, true),
            None
        );
        assert_eq!(
            native_markdown_table_field_selection(Some(original), NSRange::new(2, 7), None, false),
            None
        );
        assert_eq!(
            native_markdown_table_field_selection(Some(original), NSRange::new(6, 0), None, false),
            Some(MarkdownTextSelection::caret(6))
        );
    }

    #[test]
    fn explicit_native_installation_must_match_both_real_endpoints() {
        let selection = MarkdownTextSelection {
            anchor_utf16: 9,
            head_utf16: 2,
        };
        assert_eq!(
            native_markdown_table_field_selection(None, NSRange::new(2, 7), Some(selection), false),
            Some(selection)
        );
        assert_eq!(
            native_markdown_table_field_selection(None, NSRange::new(2, 6), Some(selection), false),
            None
        );
    }
}

/// Encodes spreadsheet-style movement without coupling semantic coordinates to native controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeMarkdownTableNavigation {
    Next,
    Previous,
    NextRow,
}

/// Normalizes viewport coordinates into a stable, finite cache-key metric.
pub(super) fn native_markdown_layout_metric(value: f64) -> i64 {
    if value.is_finite() {
        // Quantize AppKit's point coordinates to sixteenths. This filters tiny floating-point
        // layout jitter while preserving meaningful resize and scroll invalidations.
        (value * 16.0).round() as i64
    } else {
        // Nonfinite transient native geometry must not create unstable cache identities.
        0
    }
}

/// Finds visible rows by ordered UTF-16 intervals and adds at most one overscan row per edge.
pub(super) fn native_markdown_visible_table_row_range(
    rows: &[omni_markdown_core::MarkdownTableRowPrototype],
    viewport_utf16_window: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    if rows.is_empty() {
        // Empty semantic tables cannot contribute visible geometry or an overscan row.
        return 0..0;
    }

    // Two partition searches preserve logarithmic row selection even in very large tables.
    let viewport_end = viewport_utf16_window
        .end
        .max(viewport_utf16_window.start.saturating_add(1));
    let first_visible =
        rows.partition_point(|row| row.utf16_range.end <= viewport_utf16_window.start);
    let visible_end = rows.partition_point(|row| row.utf16_range.start < viewport_end);
    if first_visible < visible_end {
        // Include visible rows and one adjacent row so small scroll changes do not expose a blank edge.
        return first_visible.saturating_sub(1)..visible_end.saturating_add(1).min(rows.len());
    }

    // A gap or caret-only viewport uses the closest semantic row with the same fixed overscan budget.
    let nearest = first_visible.min(rows.len().saturating_sub(1));
    nearest.saturating_sub(1)..nearest.saturating_add(2).min(rows.len())
}

/// Resolves Tab, Shift-Tab or Enter against current parsed cells, preserving ragged-table boundaries.
#[cfg(test)]
pub(super) fn native_markdown_table_navigation_target(
    table: &MarkdownTablePrototype,
    current: NativeMarkdownTableCellCoordinate,
    navigation: NativeMarkdownTableNavigation,
) -> Option<NativeMarkdownTableCellCoordinate> {
    if table.utf16_range.start != current.table_utf16_start {
        // A coordinate belonging to an older/different table cannot move this table's editing focus.
        return None;
    }
    // Missing rows/cells are stale coordinates; return no target rather than inventing a new cell.
    let row = table.rows.get(current.row_index)?;
    let current_cell_index = row.cell_index_range.clone().find(|cell_index| {
        // Resolve the semantic column inside this row instead of assuming a rectangular flat index.
        table
            .cells
            .get(*cell_index)
            .is_some_and(|cell| cell.column_index == current.column_index)
    })?;

    let target_cell_index = match navigation {
        NativeMarkdownTableNavigation::Next => {
            // Tab advances horizontally, then wraps only to an existing next row.
            if current_cell_index + 1 < row.cell_index_range.end {
                // The next cell remains within the current row's authoritative parsed interval.
                current_cell_index + 1
            } else {
                // At the row edge, the next parsed row may be absent or empty.
                let next_row = table.rows.get(current.row_index.saturating_add(1))?;
                next_row.cell_index_range.clone().next()?
            }
        }
        NativeMarkdownTableNavigation::Previous => {
            // Shift-Tab reverses horizontal traversal and wraps to the preceding row's last cell.
            if current_cell_index > row.cell_index_range.start {
                // A preceding cell exists in the current row.
                current_cell_index - 1
            } else {
                // Checked subtraction makes the first cell of the first row a terminal boundary.
                let previous_row = table.rows.get(current.row_index.checked_sub(1)?)?;
                previous_row.cell_index_range.clone().next_back()?
            }
        }
        NativeMarkdownTableNavigation::NextRow => {
            // Enter preserves the semantic column; a ragged next row can legitimately have no target.
            let next_row = table.rows.get(current.row_index.saturating_add(1))?;
            next_row.cell_index_range.clone().find(|cell_index| {
                // Search only existing next-row cells rather than creating implicit padding columns.
                table
                    .cells
                    .get(*cell_index)
                    .is_some_and(|cell| cell.column_index == current.column_index)
            })?
        }
    };
    // Reconstruct the destination from authoritative parsed data after resolving its flat cell index.
    let target = table.cells.get(target_cell_index)?;
    let row_index = table
        .rows
        .iter()
        .position(|row| row.cell_index_range.contains(&target_cell_index))?;
    Some(NativeMarkdownTableCellCoordinate {
        table_utf16_start: table.utf16_range.start,
        row_index,
        column_index: target.column_index,
    })
}

/// Fits natural column widths to the viewport while enforcing useful bounded widths where space permits.
pub(super) fn native_markdown_table_column_widths(
    natural_widths: &[f64],
    available_width: f64,
) -> Vec<f64> {
    if natural_widths.is_empty() || available_width <= 0.0 {
        // A zero-column or collapsed viewport has no meaningful horizontal layout to allocate.
        return Vec::new();
    }
    // Relax the normal minimum for narrow viewports before clamping each measured natural width.
    let column_count = natural_widths.len() as f64;
    let dynamic_minimum =
        NATIVE_MARKDOWN_TABLE_MINIMUM_COLUMN_WIDTH.min((available_width / column_count).max(42.0));
    let mut widths: Vec<f64> = natural_widths
        .iter()
        .map(|width| width.clamp(dynamic_minimum, NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH))
        .collect();
    let mut total: f64 = widths.iter().sum();
    if total < available_width {
        // Distribute spare width fairly among columns that have not reached the maximum.
        let mut remaining = available_width - total;
        // Each iteration consumes spare width or stops when every column is capped; half a point
        // prevents floating-point residue from turning viewport layout into unbounded repeated work.
        while remaining > 0.5 {
            let expandable = widths
                .iter()
                .filter(|width| **width < NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH)
                .count();
            if expandable == 0 {
                // Every column reached the cap; leaving trailing space is preferable to oversized cells.
                break;
            }
            let share = remaining / expandable as f64;
            let mut consumed = 0.0;
            // Apply the same share with individual caps, then redistribute any unconsumed remainder.
            for width in &mut widths {
                let growth = share.min(NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH - *width);
                *width += growth;
                consumed += growth;
            }
            if consumed <= 0.5 {
                // No meaningful expansion remains, so another pass would only add numerical noise.
                break;
            }
            remaining -= consumed;
        }
        return widths;
    }
    if total > available_width {
        // Remove overflow proportionally to each column's capacity above the dynamic minimum.
        let shrinkable: f64 = widths
            .iter()
            .map(|width| (*width - dynamic_minimum).max(0.0))
            .sum();
        let overflow = total - available_width;
        if shrinkable > 0.0 {
            // At least one column can shrink without first crossing the normal dynamic minimum.
            for width in &mut widths {
                // A column contributes only its own nonnegative share of the aggregate shrink capacity.
                let capacity = (*width - dynamic_minimum).max(0.0);
                *width -= overflow * (capacity / shrinkable);
            }
        }
        total = widths.iter().sum();
        if total > available_width + 0.5 {
            // An exceptionally narrow viewport takes equal widths instead of overflowing horizontally.
            widths.fill(available_width / column_count);
        }
    }
    widths
}

/// 普通单格提交消费唯一序列化扫描；没有选区时仍保持原字符串入口不失败。
pub(super) fn native_markdown_sanitize_table_cell_input(source: &str) -> String {
    sanitize_table_cell_source(source, None).0
}

/// 行内格式沿相同序列化输出映射真实 scalar 端点；非法 UTF16 中点不能向邻字取整。
pub(super) fn native_markdown_sanitize_table_cell_input_with_selection(
    source: &str,
    selection: std::ops::Range<usize>,
) -> Result<(String, std::ops::Range<usize>), String> {
    if selection.start > selection.end {
        return Err("格内选择范围顺序无效，草稿将保持打开".into());
    }
    let (text, mapped) = sanitize_table_cell_source(source, Some(selection));
    mapped
        .map(|range| (text, range))
        .ok_or_else(|| "格内选择不在有效文字边界，草稿将保持打开".into())
}

/// 单次实际发出源字节的 producer 同步记录可选端点；不存在第二份转义、换行或 trim 规则。
fn sanitize_table_cell_source(
    source: &str,
    selection: Option<std::ops::Range<usize>>,
) -> (String, Option<std::ops::Range<usize>>) {
    let mut sanitized = String::with_capacity(source.len());
    let mut consecutive_backslashes = 0_usize;
    let mut source_units = 0;
    let mut output_units = 0;
    let mut start = None;
    let mut end = None;
    for scalar in source.chars() {
        // 端点只能在原 scalar 边界领取本次真实输出位置；代理项内部位置自然保持未知。
        if let Some(selection) = &selection {
            if source_units == selection.start {
                start = Some(output_units);
            }
            if source_units == selection.end {
                end = Some(output_units);
            }
        }
        match scalar {
            '\r' | '\n' => {
                // 一格不拆成新表格行；相邻 CR/LF 或已有空格只保留一个词间隔。
                if !sanitized.ends_with(' ') {
                    sanitized.push(' ');
                    output_units += 1; // 只记实际写出的空格，CRLF 第二个 scalar 不推进映射。
                }
                consecutive_backslashes = 0;
            }
            '|' if consecutive_backslashes.is_multiple_of(2) => {
                // 偶数反斜线仍使 pipe 有结构意义，实际加一条 escape 后才是格内文字。
                sanitized.push('\\');
                sanitized.push('|');
                output_units += 2;
                consecutive_backslashes = 0;
            }
            '\\' => {
                // 原反斜线逐字保留；紧邻 pipe 的奇偶性只属于这一条真实源扫描。
                sanitized.push('\\');
                output_units += 1;
                consecutive_backslashes = consecutive_backslashes.saturating_add(1);
            }
            _ => {
                // 普通 Unicode 与已转义 pipe 原字节保留；同一个 len_utf16 决定输出端点。
                sanitized.push(scalar);
                output_units += scalar.len_utf16();
                consecutive_backslashes = 0;
            }
        }
        source_units += scalar.len_utf16();
    }
    if let Some(selection) = &selection {
        if source_units == selection.start {
            start = Some(output_units); // 包括真实空字符串和文尾 caret。
        }
        if source_units == selection.end {
            end = Some(output_units);
        }
    }
    // 从实际输出切片取得 trim 的唯一边界；空白类型不在映射层重新解释一遍。
    let leading_bytes = sanitized.len() - sanitized.trim_start().len();
    let leading_units = sanitized[..leading_bytes].encode_utf16().count();
    let content = sanitized.trim();
    let content_bytes = content.len();
    let content_units = content.encode_utf16().count();
    let map = |position: usize| position.saturating_sub(leading_units).min(content_units);
    let mapped = start.zip(end).map(|(start, end)| map(start)..map(end));
    // trim 返回的 UTF8 边界合法；在唯一输出内移除前后空白，不再复制一份规范化单格。
    drop(sanitized.drain(..leading_bytes));
    sanitized.truncate(content_bytes);
    (sanitized, mapped)
}

/// Emits one small three-column Markdown table, separating surrounding blocks only when necessary.
pub(super) fn native_markdown_default_table_source(
    needs_prefix: bool,
    needs_suffix: bool,
) -> String {
    const TABLE_SOURCE: &str =
        "| 列 1 | 列 2 | 列 3 |\n| :--- | :---: | ---: |\n|  |  |  |\n|  |  |  |";
    let mut markdown = String::with_capacity(
        TABLE_SOURCE.len() + usize::from(needs_prefix) * 2 + usize::from(needs_suffix) * 2,
    );
    if needs_prefix {
        // Existing nonempty text before the insertion needs a blank block boundary.
        markdown.push_str("\n\n");
    }
    markdown.push_str(TABLE_SOURCE);
    if needs_suffix {
        // Existing text after the insertion must not become part of the table's final row.
        markdown.push_str("\n\n");
    }
    markdown
}

#[cfg(test)]
mod native_markdown_table_tests {
    use super::{
        MarkdownPresentationMode, MarkdownPresentationStyles, MarkdownTableAlignment,
        MarkdownTablePrototype, MarkdownViewportPresentation, NativeMarkdownTableCellCoordinate,
        NativeMarkdownTableNavigation, native_markdown_default_table_source,
        native_markdown_sanitize_table_cell_input,
        native_markdown_table_cell_display_text_from_source, native_markdown_table_column_widths,
        native_markdown_table_navigation_target, native_markdown_visible_table_row_range,
    };
    use omni_markdown_core::{MarkdownTableCellPrototype, MarkdownTableRowPrototype};

    /// Build an ordered rectangular semantic fixture without parsing or native object allocation.
    pub(super) fn uniform_table_prototype(
        row_count: usize,
        column_count: usize,
    ) -> MarkdownTablePrototype {
        let mut rows = Vec::with_capacity(row_count);
        let mut cells = Vec::with_capacity(row_count.saturating_mul(column_count));
        // Build a deterministic rectangular semantic fixture; every row owns a disjoint cell interval.
        for row_index in 0..row_count {
            let row_start = row_index * 10;
            let cell_start = cells.len();
            // Scalar-sized synthetic cells keep the fixture focused on navigation and row culling.
            for column_index in 0..column_count {
                cells.push(MarkdownTableCellPrototype {
                    utf16_range: row_start + column_index..row_start + column_index + 1,
                    column_index,
                    alignment: MarkdownTableAlignment::Left,
                });
            }
            rows.push(MarkdownTableRowPrototype {
                utf16_range: row_start..row_start + 10,
                cell_index_range: cell_start..cells.len(),
                is_header: row_index == 0,
            });
        }
        MarkdownTablePrototype {
            utf16_range: 0..row_count * 10,
            header_utf16_range: 0..10,
            delimiter_utf16_range: 10..11,
            rows,
            cells,
        }
    }

    /// Derive one actual parser table while retaining only its owned finite prototype for assertions.
    pub(super) fn parsed_table(source: &str) -> MarkdownTablePrototype {
        let mut presentation = MarkdownViewportPresentation::default();
        presentation.set_mode(MarkdownPresentationMode::InlinePreview);
        presentation.set_active_selection(source.encode_utf16().count()..0);
        presentation
            .rebuild(1, 0..source.encode_utf16().count(), source)
            .expect("build fixture table semantics");
        presentation
            .tables()
            .first()
            .expect("the parsed fixture table")
            .clone()
    }

    #[test]
    /// Include exactly one adjacent row at either visible edge rather than materializing the table body.
    pub(super) fn visible_table_rows_include_exactly_one_overscan_row_per_side() {
        let table = uniform_table_prototype(10, 6);

        assert_eq!(
            native_markdown_visible_table_row_range(&table.rows, &(35..55)),
            2..7
        );
    }

    #[test]
    /// Keep cell materialization proportional to the viewport even when the semantic table has ten thousand rows.
    pub(super) fn large_table_materializes_only_visible_rows_and_bounded_overscan_cells() {
        let table = uniform_table_prototype(10_000, 6);
        let rows = native_markdown_visible_table_row_range(&table.rows, &(50_003..50_004));
        let visible_cell_count = rows
            .clone()
            .map(|row_index| table.rows[row_index].cell_index_range.len())
            .sum::<usize>();

        assert_eq!(rows, 4_999..5_002);
        assert_eq!(visible_cell_count, 18);
        assert!(visible_cell_count < table.cells.len());
    }

    #[test]
    /// Resolve horizontal wrapping, reverse wrapping and vertical column preservation at real row boundaries.
    pub(super) fn table_navigation_matches_spreadsheet_tab_backtab_and_enter_behavior() {
        let table = uniform_table_prototype(3, 6);
        let coordinate = |row_index, column_index| NativeMarkdownTableCellCoordinate {
            table_utf16_start: 0,
            row_index,
            column_index,
        };

        assert_eq!(
            native_markdown_table_navigation_target(
                &table,
                coordinate(0, 5),
                NativeMarkdownTableNavigation::Next,
            ),
            Some(coordinate(1, 0))
        );
        assert_eq!(
            native_markdown_table_navigation_target(
                &table,
                coordinate(1, 0),
                NativeMarkdownTableNavigation::Previous,
            ),
            Some(coordinate(0, 5))
        );
        assert_eq!(
            native_markdown_table_navigation_target(
                &table,
                coordinate(0, 3),
                NativeMarkdownTableNavigation::NextRow,
            ),
            Some(coordinate(1, 3))
        );
        assert_eq!(
            native_markdown_table_navigation_target(
                &table,
                coordinate(2, 5),
                NativeMarkdownTableNavigation::Next,
            ),
            None
        );
    }

    #[test]
    /// Preserve literal escaped pipes while preventing a cell draft from introducing another Markdown row/column.
    pub(super) fn table_cell_input_stays_single_line_and_escapes_structural_pipes() {
        assert_eq!(
            native_markdown_sanitize_table_cell_input(" a|b\nc\\|d "),
            "a\\|b c\\|d"
        );
    }

    #[test]
    /// Keep the finite source template parseable and insert blank block boundaries only where requested.
    pub(super) fn default_table_source_has_three_columns_and_optional_block_spacing() {
        let source = native_markdown_default_table_source(false, false);
        assert_eq!(source.lines().count(), 4);
        assert_eq!(parsed_table(&source).cells.len(), 9);
        assert!(native_markdown_default_table_source(true, false).starts_with("\n\n|"));
        assert!(native_markdown_default_table_source(false, true).ends_with("|\n\n"));
    }

    #[test]
    /// Translate hidden code delimiters and escaped pipes without shifting Unicode style ranges incorrectly.
    pub(super) fn table_cell_display_removes_commonmark_and_gfm_pipe_escapes() {
        let source = "`a\\|b` c\\|d 🟢";
        let display =
            native_markdown_table_cell_display_text_from_source(source, 0, |offset| match offset {
                // The two code delimiters disappear from derived cell display text.
                0 | 5 => MarkdownPresentationStyles::HIDDEN_MARKER,
                // Interior escaped pipe content keeps its inline-code style.
                1..=4 => MarkdownPresentationStyles::INLINE_CODE,
                // Remaining literal content inherits only table membership.
                _ => MarkdownPresentationStyles::TABLE,
            });

        assert_eq!(display.text, "a|b c|d 🟢");
        assert_eq!(display.text.encode_utf16().count(), 10);
        assert_eq!(display.style_spans[0].utf16_range, 0..3);
        assert!(
            display.style_spans[0]
                .styles
                .contains(MarkdownPresentationStyles::INLINE_CODE)
        );
    }

    #[test]
    /// Navigate by semantic row/column after an earlier cell changes every subsequent source offset.
    pub(super) fn semantic_cell_target_survives_cell_length_changes_without_index_caching() {
        let original = parsed_table("| A | B |\n| --- | --- |\n| x | y |");
        let edited = parsed_table("| A | B |\n| --- | --- |\n| much longer | y |");
        let current = NativeMarkdownTableCellCoordinate {
            table_utf16_start: edited.utf16_range.start,
            row_index: 1,
            column_index: 0,
        };

        assert!(edited.cells[3].utf16_range.start > original.cells[3].utf16_range.start);
        assert_eq!(
            native_markdown_table_navigation_target(
                &edited,
                current,
                NativeMarkdownTableNavigation::Next,
            ),
            Some(NativeMarkdownTableCellCoordinate {
                table_utf16_start: edited.utf16_range.start,
                row_index: 1,
                column_index: 1,
            })
        );
    }

    #[test]
    /// Fit normal viewports without oversized columns and distribute exceptionally narrow widths without overflow.
    pub(super) fn table_column_widths_fill_available_space_without_unbounded_columns() {
        let widths = native_markdown_table_column_widths(&[50.0, 200.0, 400.0], 600.0);
        assert!((widths.iter().sum::<f64>() - 600.0).abs() < 0.5);
        assert!(widths.iter().all(|width| *width > 0.0 && *width <= 280.0));

        let constrained = native_markdown_table_column_widths(&[200.0; 6], 240.0);
        assert_eq!(constrained, vec![40.0; 6]);
    }
}
