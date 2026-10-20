//! 已有真实上下文的有限表格投影；不从拼接成功反推省略源行的成员资格。
//!
//! 本模块内部adapter只解决语法投影和坐标恢复；外部消费者只能从opaque已验证票据
//! 读取metadata。成员、容器、全局引用与underline均由文档producer确认，候选构造不外露。

use super::*;

pub(crate) mod context;
#[cfg(feature = "s2-table-window-prototype")]
pub mod navigation;
#[cfg(feature = "s2-table-window-prototype")]
pub(crate) mod references;

/// 所有上限约束当前批次及离屏header上下文，不约束整表的总行数。
pub const TABLE_WINDOW_MAX_ROWS: usize = 256;
pub const TABLE_WINDOW_MAX_COLUMNS: usize = 256;
pub const TABLE_WINDOW_MAX_CELLS: usize = 1_024;
pub const TABLE_WINDOW_MAX_UTF16: usize = 64 * 1_024;
pub const TABLE_WINDOW_MAX_BYTES: usize = 256 * 1_024;
pub const TABLE_WINDOW_MAX_REFERENCE_BYTES: usize = 16 * 1_024;

#[derive(Clone, Debug)]
/// 同一viewport各窗口共用的逻辑metadata与run准入；不含正文或源强owner。
pub(crate) struct MarkdownTableWindowQuota(std::rc::Rc<QuotaState>);
#[derive(Debug)]
struct QuotaState {
    runs: std::cell::Cell<usize>,
    prefixes: std::cell::Cell<usize>,
    metadata: std::cell::Cell<usize>,
    max_runs: usize,
    max_metadata: usize,
    underline_reserved: std::cell::Cell<bool>,
    cell_inline_reserved: std::cell::Cell<bool>,
}
impl MarkdownTableWindowQuota {
    pub(crate) fn new(max_runs: usize, max_metadata: usize) -> Self {
        Self(std::rc::Rc::new(QuotaState {
            runs: 0.into(),
            prefixes: 0.into(),
            metadata: 0.into(),
            max_runs,
            max_metadata,
            underline_reserved: false.into(),
            cell_inline_reserved: false.into(),
        }))
    }
    /// 同一 SourceRequest 只允许一个活动 CellInline；其固定作业与4KiB读窗只占一次活库存。
    /// 外带每格的发布、样式、block 和真实 Vec 仍另行逐份准入，不能借串行工作区漏算共活输出。
    pub(crate) fn reserve_cell_inline_workspace(
        &self,
        bytes: usize,
    ) -> Result<(), MarkdownTableWindowError> {
        if !self.0.cell_inline_reserved.get() {
            self.admit_metadata(bytes)?;
            self.0.cell_inline_reserved.set(true);
        }
        Ok(())
    }
    pub(crate) fn reserve_underline_workspace(&self) -> Result<(), MarkdownTableWindowError> {
        if !self.0.underline_reserved.get() {
            // 批次串行投影时只活一份原underline库存；在创建它之前预留其已有最大pair容量。
            self.admit_metadata(
                (MAX_MARKDOWN_PRESENTATION_DECORATIONS / 3)
                    * std::mem::size_of::<MarkdownTableUnderline>()
                    + crate::markdown_document_buffer::MarkdownDocumentBuffer::underline_code_workspace_bytes(),
            )?;
            self.0.underline_reserved.set(true);
        }
        Ok(())
    }
    pub(crate) fn metadata_used(&self) -> usize {
        self.0.metadata.get()
    }
    pub(crate) fn remaining_runs(&self) -> usize {
        self.0.max_runs - self.0.runs.get()
    }
    pub(crate) fn admit_retained(
        &self,
        runs: usize,
        metadata_bytes: usize,
    ) -> Result<(), MarkdownTableWindowError> {
        // 既有 Ready 的不可变载荷仍与新后缀同活，必须预扣其 run 与 metadata，不能只算新后缀。
        // 先完整计算两项累计值；溢出时不改额度，旧 scratch 已释放，因此不恢复 prefix 计数。
        let next_runs = self
            .0
            .runs
            .get()
            .checked_add(runs)
            .ok_or(MarkdownTableWindowError::SemanticLimit)?;
        let next_metadata = self
            .0
            .metadata
            .get()
            .checked_add(metadata_bytes)
            .ok_or(MarkdownTableWindowError::SemanticLimit)?;
        // 两项都准入后才同时提交；任一超限都保留原额度，避免失败后留下部分预扣。
        if next_runs > self.0.max_runs || next_metadata > self.0.max_metadata {
            return Err(MarkdownTableWindowError::SemanticLimit);
        }
        self.0.runs.set(next_runs);
        self.0.metadata.set(next_metadata);
        Ok(())
    }
    pub(crate) fn admit_metadata(&self, bytes: usize) -> Result<(), MarkdownTableWindowError> {
        let next = self
            .0
            .metadata
            .get()
            .checked_add(bytes)
            .ok_or(MarkdownTableWindowError::SemanticLimit)?;
        if next > self.0.max_metadata {
            return Err(MarkdownTableWindowError::SemanticLimit);
        }
        self.0.metadata.set(next);
        Ok(())
    }
    pub(crate) fn admit_prefix(&self) -> Result<(), MarkdownTableWindowError> {
        if self.0.prefixes.get() == self.0.max_runs {
            return Err(MarkdownTableWindowError::SemanticLimit);
        }
        self.admit_metadata(std::mem::size_of::<MarkdownPresentationRun>())?;
        self.0.prefixes.set(self.0.prefixes.get() + 1);
        Ok(())
    }
    fn admit_output(&self) -> Result<(), MarkdownTableWindowError> {
        if self.remaining_runs() == 0 {
            return Err(MarkdownTableWindowError::SemanticLimit);
        }
        self.admit_metadata(std::mem::size_of::<MarkdownPresentationRun>())?;
        self.0.runs.set(self.0.runs.get() + 1);
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct RunAdmission<'a> {
    maximum: usize,
    quota: Option<&'a MarkdownTableWindowQuota>,
    output: bool,
}
impl RunAdmission<'_> {
    fn with_maximum(self, maximum: usize) -> Self {
        Self { maximum, ..self }
    }
    fn output(self) -> Self {
        Self {
            output: true,
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// 当前代次真实完整源行；prefix只可由已验证的container context给出，不能trim猜测。
pub(crate) struct MarkdownTableSourceLine<'a> {
    pub text: &'a str,
    pub byte_start: usize,
    pub utf16_start: usize,
    pub source_line: usize,
    pub row_ordinal: usize,
    pub container_prefix_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 原始源中已经配对的UTF16 underline结构；省略行里的标签不能由投影重新配对。
pub(crate) struct MarkdownTableUnderline {
    pub open: Range<usize>,
    pub content: Range<usize>,
    pub close: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
/// 全局上下文准备状态与文本窗口分离；未知不是“没有definition/underline”。
pub(crate) enum MarkdownTableGlobalContext<'a> {
    Pending,
    #[cfg(test)]
    Ready {
        reference_definitions: &'a str,
        underline: &'a [MarkdownTableUnderline],
    },
    #[cfg(feature = "s2-table-window-prototype")]
    ReadyResolved {
        references: &'a references::MarkdownTableReferenceQueries,
        underline: &'a [MarkdownTableUnderline],
    },
}

#[derive(Debug)]
/// 仅借用本批真实行。header/delimiter必须相邻；body可从任意已证实的绝对ordinal开始。
pub(crate) struct MarkdownTableWindowInput<'a> {
    pub generation: u64,
    pub context_revision: u64,
    pub header: MarkdownTableSourceLine<'a>,
    pub delimiter: MarkdownTableSourceLine<'a>,
    pub body: &'a [MarkdownTableSourceLine<'a>],
    pub include_header: bool,
    pub inherited_styles: MarkdownPresentationStyles,
    pub container_prefix_runs: &'a [MarkdownPresentationRun],
    pub global: MarkdownTableGlobalContext<'a>,
    pub active_selection: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 缺格只有插入锚点，不能被当作有字符的cell或直接零长替换后宣称补齐pipe。
pub enum MarkdownTableWindowCellSource {
    Present(Range<usize>),
    ImplicitEmpty { insertion_anchor_utf16: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// cell局部序号仅用于本批flat Vec；源row ordinal在row记录中独立保留。
pub struct MarkdownTableWindowCell {
    pub source: MarkdownTableWindowCellSource,
    pub column_index: usize,
    pub alignment: MarkdownTableAlignment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 原始完整行的稳定身份；window Vec位置不具有编辑或导航身份。
pub struct MarkdownTableWindowRow {
    pub row_ordinal: usize,
    pub source_line: usize,
    pub byte_range: Range<usize>,
    pub utf16_range: Range<usize>,
    pub cell_index_range: Range<usize>,
    pub is_header: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 离屏H只用于列与字体上下文；不产生可绘制row或H到中段的连续coverage。
pub struct MarkdownTableWindowHeaderContext {
    pub row: MarkdownTableWindowRow,
    /// H已经发布时为空，调用方复用主cells，避免准备同一组header两次。
    pub cells: Vec<MarkdownTableWindowCell>,
    /// H已经发布时为空，属性消费者复用主runs；其余情况只保存H有限源样式。
    pub runs: Vec<MarkdownPresentationRun>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 投影候选只保纯metadata。它不证明省略行的成员，文档producer验证前不得发布。
pub struct MarkdownProjectedTableWindow {
    pub generation: u64,
    pub context_revision: u64,
    pub header_byte_range: Range<usize>,
    pub delimiter_byte_range: Range<usize>,
    pub header_utf16_range: Range<usize>,
    pub delimiter_utf16_range: Range<usize>,
    pub column_count: usize,
    pub header_context: MarkdownTableWindowHeaderContext,
    pub rows: Vec<MarkdownTableWindowRow>,
    pub cells: Vec<MarkdownTableWindowCell>,
    pub runs: Vec<MarkdownPresentationRun>,
    pub projection_bytes: usize,
    pub projection_utf16: usize,
    pub origins: Vec<MarkdownTableOrigin>,
}

impl MarkdownProjectedTableWindow {
    pub fn header_cells(&self) -> &[MarkdownTableWindowCell] {
        if self.header_context.cells.is_empty() {
            // H在主rows中时其cell range直接指向同一个主Vec，不复制metadata或Native对象。
            &self.cells[self.header_context.row.cell_index_range.clone()]
        } else {
            &self.header_context.cells
        }
    }

    /// row ordinal是整表身份，不能把中段window Vec位置当成同一数值直接索引。
    pub fn row(&self, row_ordinal: usize) -> Option<&MarkdownTableWindowRow> {
        let index = self
            .rows
            .partition_point(|row| row.row_ordinal < row_ordinal);
        self.rows
            .get(index)
            .filter(|row| row.row_ordinal == row_ordinal)
    }

    /// 列数来自真实header上下文，即使header不在本窗口也无需补造离屏row或重新扫描整表。
    pub fn cell(
        &self,
        row_ordinal: usize,
        column_index: usize,
    ) -> Option<&MarkdownTableWindowCell> {
        if column_index >= self.column_count {
            // 外部陈旧导航坐标不能跨列读取邻接row的flat cells。
            return None;
        }
        let row = self.row(row_ordinal)?;
        let cells = self.cells.get(row.cell_index_range.clone())?;
        cells
            .get(column_index)
            .filter(|cell| cell.column_index == column_index)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 每段映射同时保留byte与UTF16坐标；不保文本，也不合成省略区间的连续range。
pub struct MarkdownTableOrigin {
    pub projected_bytes: Range<usize>,
    pub projected_utf16: Range<usize>,
    pub source_bytes: Range<usize>,
    pub source_utf16: Range<usize>,
    pub context_only: bool,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// 有限候选的失败不会触发全文/整表重试，也不授权部分overlay隐藏源文。
pub enum MarkdownTableWindowError {
    #[error("表格上下文尚未准备完成")]
    ContextPending,
    #[error("表格当前窗口行数超过预算")]
    RowLimit,
    #[error("表格列数超过当前窗口预算")]
    ColumnLimit,
    #[error("表格当前窗口单元格数量超过预算")]
    CellLimit,
    #[error("表格当前源窗口超过预算")]
    SourceLimit,
    #[error("表格引用上下文超过预算")]
    ReferenceLimit,
    #[error("表格真实源坐标或行身份无效")]
    InvalidSource,
    #[error("表格有限投影与已提供上下文不一致")]
    ParserMismatch,
    #[error("表格投影坐标越过了真实源片段")]
    ProjectionBoundary,
    #[error("表格underline上下文无效")]
    InvalidUnderline,
    #[error("表格有限样式数量超过预算")]
    SemanticLimit,
}

#[derive(Clone, Debug)]
/// 每段只映射自己的原始内容；context header可以解析但不对外发布绘制行。
struct OriginSegment<'a> {
    projected: Range<usize>,
    projected_bytes: Range<usize>,
    source: MarkdownTableSourceLine<'a>,
    content_utf16_start: usize,
    publish: bool,
}

/// 对已经由文档层证明的表格源行建立有限候选。
///
/// 输入必须是同代次、同一真实表上下文。此函数检查当前行/列与坐标，但不扫描省略区间；
/// 调用方不能因为返回成功就认为省略行必属该表。global Ready也必须来自当前代次。
/// footnote沿既有产品合同保持literal；普通reference使用固定Parser自己的label查找。
///
/// # Errors
/// 任何预算、源行、Parser或上下文不一致都拒绝整批；不产生部分结果或全文fallback。
#[cfg(test)]
pub(crate) fn project_table_window(
    input: &MarkdownTableWindowInput<'_>,
) -> Result<MarkdownProjectedTableWindow, MarkdownTableWindowError> {
    project_table_window_with_quota(input, None)
}

/// batch共用剩余额度；旧单窗调用仍使用原8192，语法/来源映射保持同一个实现。
pub(crate) fn project_table_window_with_quota(
    input: &MarkdownTableWindowInput<'_>,
    quota: Option<&MarkdownTableWindowQuota>,
) -> Result<MarkdownProjectedTableWindow, MarkdownTableWindowError> {
    let admission = RunAdmission {
        maximum: quota.map_or(MAX_MARKDOWN_PRESENTATION_DECORATIONS, |quota| {
            quota.remaining_runs()
        }),
        quota,
        output: false,
    };
    let (reference_context, underline) = match input.global {
        #[cfg(test)]
        MarkdownTableGlobalContext::Ready {
            reference_definitions,
            underline,
        } => {
            if reference_definitions.len() > TABLE_WINDOW_MAX_REFERENCE_BYTES {
                // 兼容纯adapter测试的定义文本仍受独立预算，不允许转成全局目录。
                return Err(MarkdownTableWindowError::ReferenceLimit);
            }
            (
                (!reference_definitions.is_empty()).then_some(
                    MarkdownPresentationReferenceContext::Definitions(reference_definitions),
                ),
                underline,
            )
        }
        #[cfg(feature = "s2-table-window-prototype")]
        MarkdownTableGlobalContext::ReadyResolved {
            references,
            underline,
        } => {
            if !references.is_ready() {
                // 部分已命中不能提前发布；未决definition仍可能改变当前投影语义。
                return Err(MarkdownTableWindowError::ContextPending);
            }
            (
                Some(MarkdownPresentationReferenceContext::Resolved(references)),
                underline,
            )
        }
        MarkdownTableGlobalContext::Pending => {
            // 未完成全局metadata时不能把未知引用或跨行underline误当作literal完成态。
            return Err(MarkdownTableWindowError::ContextPending);
        }
    };
    if underline.len() > MAX_MARKDOWN_PRESENTATION_DECORATIONS / 3
        || input.container_prefix_runs.len() > MAX_MARKDOWN_PRESENTATION_DECORATIONS
    {
        // producer只能交付与当前窗口相交的有限paired spans，不得传入全源tag目录。
        return Err(MarkdownTableWindowError::SemanticLimit);
    }
    let (projection, origins, utf16, columns) = prepare_table_projection(input)?;
    let projected_selection = project_selection(&origins, &input.active_selection);
    let built = build_markdown_presentation_with_reference_context(
        &projection,
        0,
        &projected_selection,
        input.header.byte_start == 0,
        reference_context,
        false,
    )
    .map_err(|_| MarkdownTableWindowError::SemanticLimit)?;
    let [table] = built.tables.as_slice() else {
        // 一批只允许已提供header的单张table；投影中有新块/第二表不能拼进旧身份。
        return Err(MarkdownTableWindowError::ParserMismatch);
    };
    if table.rows.len() != input.body.len() + 1
        || table
            .rows
            .first()
            .is_none_or(|row| row.cell_index_range.len() != columns)
    {
        // 语法终止、列数不符或Parser自身fuel拒绝都使整批失去完整性。
        return Err(MarkdownTableWindowError::ParserMismatch);
    }
    let mut rows = Vec::new();
    let mut cells = Vec::new();
    for (parsed, origin) in table.rows.iter().zip(
        origins
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != 1)
            .map(|(_, origin)| origin),
    ) {
        // 原始ordinal来自输入证明，不从projection row位置加base伪造。
        if !origin.publish {
            continue; // 离屏header只为语法/alignment提供上下文，不生成待绘制cell。
        }
        let first_cell = cells.len();
        let line_end = source_content_range(origin.source)?.1.end;
        for cell in &table.cells[parsed.cell_index_range.clone()] {
            // 每个cell应完整属于同一真实行；zero-length补格另留明确插入锚点。
            let actual = map_range(origin, &cell.utf16_range)?;
            let source = if actual.is_empty() && actual.start >= line_end {
                // Parser在行尾补的空cell没有真实字符，不能使用它的虚拟EOF位置替换正文。
                MarkdownTableWindowCellSource::ImplicitEmpty {
                    insertion_anchor_utf16: line_end,
                }
            } else {
                // 即使显式空格trim后范围为空，它仍有本行真实分隔结构可供编辑producer核验。
                MarkdownTableWindowCellSource::Present(actual)
            };
            cells.push(MarkdownTableWindowCell {
                source,
                column_index: cell.column_index,
                alignment: cell.alignment,
            });
        }
        let (byte_range, utf16_range) = source_content_range(origin.source)?;
        rows.push(MarkdownTableWindowRow {
            row_ordinal: origin.source.row_ordinal,
            source_line: origin.source.source_line,
            byte_range,
            utf16_range,
            cell_index_range: first_cell..cells.len(),
            is_header: parsed.is_header,
        });
    }
    let header_context = build_header_context(
        input,
        &origins[0],
        table,
        &built.runs,
        &rows,
        underline,
        admission,
    )?;
    let maximum = admission.maximum.saturating_sub(header_context.runs.len());
    let admission = admission.with_maximum(maximum);
    let mut spans = Vec::new();
    for run in built.runs {
        // parser只了解投影坐标；每个run分别与真实origin求交，不能整段加offset跨过省略区间。
        for origin in origins.iter().filter(|origin| origin.publish) {
            // 跨header/body gap的Table范围拆成实际源交集，绝不发布虚拟连续TableEnd。
            let overlap = run.utf16_range.start.max(origin.projected.start)
                ..run.utf16_range.end.min(origin.projected.end);
            if overlap.start < overlap.end {
                admit_window_run(
                    &mut spans,
                    MarkdownPresentationRun {
                        utf16_range: map_range(origin, &overlap)?,
                        styles: run.styles.union(input.inherited_styles),
                    },
                    admission,
                )?;
            }
        }
    }
    for row in &rows {
        // 裸投影不保留quote/list外层语法，真实producer提供的继承样式按各完整源行补回。
        if !input.inherited_styles.is_empty() {
            // 容器样式来自真实context，不通过裸投影重新猜测list/quote前缀。
            admit_window_run(
                &mut spans,
                MarkdownPresentationRun {
                    utf16_range: row.utf16_range.clone(),
                    styles: input.inherited_styles,
                },
                admission,
            )?;
        }
    }
    let coverage = origins
        .iter()
        .filter(|origin| origin.publish)
        .map(|origin| {
            // 原始完整行包括已证明container前缀和真实EOL，underline不能在换行处丢掉原属性。
            origin.source.utf16_start
                ..origin.source.utf16_start + origin.source.text.encode_utf16().count()
        })
        .collect::<Vec<_>>();
    for run in input.container_prefix_runs {
        // 同一prefix元数据可能跨越许多当前行；逐交集准入，不能先展开乘积再检查总数。
        for range in &coverage {
            let clipped =
                run.utf16_range.start.max(range.start)..run.utf16_range.end.min(range.end);
            if clipped.start < clipped.end {
                // 只有真实相交部分进入共同预算；拒绝时不保留任何部分投影结果。
                admit_window_run(
                    &mut spans,
                    MarkdownPresentationRun {
                        utf16_range: clipped,
                        styles: run.styles,
                    },
                    admission,
                )?;
            }
        }
    }
    add_source_underline(
        &coverage,
        underline,
        &input.active_selection,
        &mut spans,
        admission,
    )?;
    let runs = disjoint_window_runs(spans, admission)?;
    Ok(MarkdownProjectedTableWindow {
        generation: input.generation,
        context_revision: input.context_revision,
        header_byte_range: source_content_range(input.header)?.0,
        delimiter_byte_range: source_content_range(input.delimiter)?.0,
        header_utf16_range: source_content_range(input.header)?.1,
        delimiter_utf16_range: source_content_range(input.delimiter)?.1,
        column_count: columns,
        header_context,
        rows,
        cells,
        runs,
        projection_bytes: projection.len(),
        projection_utf16: utf16,
        origins: origins
            .iter()
            .map(|origin| MarkdownTableOrigin {
                // 输出值映射供document producer复核；临时source借用在返回时全部结束。
                projected_bytes: origin.projected_bytes.clone(),
                projected_utf16: origin.projected.clone(),
                source_bytes: origin.source.byte_start + origin.source.container_prefix_bytes
                    ..origin.source.byte_start + origin.source.text.len(),
                source_utf16: origin.content_utf16_start
                    ..origin.content_utf16_start + origin.projected.len(),
                context_only: !origin.publish,
            })
            .collect(),
    })
}

/// 当前H上下文仍来自同一次有限Parser结果；不重解析header或复制整表。
fn build_header_context(
    input: &MarkdownTableWindowInput<'_>,
    origin: &OriginSegment<'_>,
    table: &MarkdownTablePrototype,
    parsed_runs: &[MarkdownPresentationRun],
    published_rows: &[MarkdownTableWindowRow],
    underline: &[MarkdownTableUnderline],
    admission: RunAdmission<'_>,
) -> Result<MarkdownTableWindowHeaderContext, MarkdownTableWindowError> {
    if input.include_header {
        let row = published_rows
            .first()
            .filter(|row| row.is_header)
            .cloned()
            .ok_or(MarkdownTableWindowError::ParserMismatch)?;
        return Ok(MarkdownTableWindowHeaderContext {
            row,
            cells: Vec::new(),
            runs: Vec::new(),
        });
    }
    let parsed = table
        .rows
        .first()
        .ok_or(MarkdownTableWindowError::ParserMismatch)?;
    let (byte_range, utf16_range) = source_content_range(input.header)?;
    let mut cells = Vec::with_capacity(parsed.cell_index_range.len());
    for cell in &table.cells[parsed.cell_index_range.clone()] {
        let range = map_range(origin, &cell.utf16_range)?;
        let source = if range.is_empty() && range.start >= utf16_range.end {
            MarkdownTableWindowCellSource::ImplicitEmpty {
                insertion_anchor_utf16: utf16_range.end,
            }
        } else {
            MarkdownTableWindowCellSource::Present(range)
        };
        cells.push(MarkdownTableWindowCell {
            source,
            column_index: cell.column_index,
            alignment: cell.alignment,
        });
    }
    let mut spans = Vec::new();
    for run in parsed_runs {
        let overlap = run.utf16_range.start.max(origin.projected.start)
            ..run.utf16_range.end.min(origin.projected.end);
        if overlap.start < overlap.end {
            admit_window_run(
                &mut spans,
                MarkdownPresentationRun {
                    utf16_range: map_range(origin, &overlap)?,
                    styles: run.styles.union(input.inherited_styles),
                },
                admission,
            )?;
        }
    }
    let coverage = input.header.utf16_start
        ..input.header.utf16_start + input.header.text.encode_utf16().count();
    for run in input.container_prefix_runs {
        let range =
            run.utf16_range.start.max(coverage.start)..run.utf16_range.end.min(coverage.end);
        if range.start < range.end {
            admit_window_run(
                &mut spans,
                MarkdownPresentationRun {
                    utf16_range: range,
                    styles: run.styles,
                },
                admission,
            )?;
        }
    }
    add_source_underline(
        std::slice::from_ref(&coverage),
        underline,
        &input.active_selection,
        &mut spans,
        admission,
    )?;
    let runs = disjoint_window_runs(spans, admission)?;
    Ok(MarkdownTableWindowHeaderContext {
        row: MarkdownTableWindowRow {
            row_ordinal: 0,
            source_line: input.header.source_line,
            byte_range,
            utf16_range,
            cell_index_range: 0..cells.len(),
            is_header: true,
        },
        cells,
        runs,
    })
}

/// 共享有限源准备先核预算再复制，引用查询和最终属性不能各自猜容器/行边界。
fn prepare_table_projection<'a>(
    input: &MarkdownTableWindowInput<'a>,
) -> Result<(String, Vec<OriginSegment<'a>>, usize, usize), MarkdownTableWindowError> {
    validate_window_source(input)?;
    let delimiter = &input.delimiter.text[input.delimiter.container_prefix_bytes..];
    let delimiter = delimiter.trim().trim_matches('|');
    let columns = delimiter.split('|').count();
    if columns == 0 || columns > TABLE_WINDOW_MAX_COLUMNS {
        // delimiter的结构列数仅作分配前保守预算；是否真是table仍由固定Parser核验。
        return Err(MarkdownTableWindowError::ColumnLimit);
    }
    if columns.saturating_mul(input.body.len() + 1) > TABLE_WINDOW_MAX_CELLS {
        // 包括context-only header和所有可补空cell，先于Parser节点构造限制稀疏表乘法。
        return Err(MarkdownTableWindowError::CellLimit);
    }
    let mut projection = String::new();
    let mut origins = Vec::with_capacity(input.body.len() + 2);
    let mut utf16 = 0;
    append_origin(
        &mut projection,
        &mut origins,
        &mut utf16,
        input.header,
        input.include_header,
    )?;
    append_origin(
        &mut projection,
        &mut origins,
        &mut utf16,
        input.delimiter,
        input.include_header,
    )?;
    for row in input.body {
        // 只复制当前完整源行；省略部分不进入projection、AST或UTF16映射数组。
        append_origin(&mut projection, &mut origins, &mut utf16, *row, true)?;
    }
    Ok((projection, origins, utf16, columns))
}

#[cfg(feature = "s2-table-window-prototype")]
/// 仅提取当前已证明窗口的实际未解析引用；未知全局定义不妨碍先建立有限查询集。
///
/// # Errors
/// 真实行坐标、投影源或查询标签超过当前批次预算时拒绝，不扫描源文件定义区。
pub(crate) fn table_window_reference_queries(
    input: &MarkdownTableWindowInput<'_>,
) -> Result<references::MarkdownTableReferenceQueries, MarkdownTableWindowError> {
    let (projection, _, _, _) = prepare_table_projection(input)?;
    references::MarkdownTableReferenceQueries::from_projection(
        &projection,
        input.header.byte_start == 0,
    )
}

/// batch只向同一个尚未封口库存登记label；不为每表重新授予key/value额度。
#[cfg(feature = "s2-table-window-prototype")]
pub(crate) fn collect_table_window_reference_queries(
    input: &MarkdownTableWindowInput<'_>,
    queries: &mut references::MarkdownTableReferenceQueries,
) -> Result<usize, MarkdownTableWindowError> {
    let (projection, _, _, _) = prepare_table_projection(input)?;
    queries.collect_projection(&projection, input.header.byte_start == 0)?;
    Ok(projection.len())
}

/// 检查声明的完整行、顺序和预算；成员证明仍由document producer负责。
fn validate_window_source(
    input: &MarkdownTableWindowInput<'_>,
) -> Result<(), MarkdownTableWindowError> {
    if input.body.len() >= TABLE_WINDOW_MAX_ROWS {
        return Err(MarkdownTableWindowError::RowLimit); // header语法节点同样计入当前批次。
    }
    let bytes = input.body.iter().fold(
        input
            .header
            .text
            .len()
            .saturating_add(input.delimiter.text.len()),
        |count, line| count.saturating_add(line.text.len()),
    );
    if bytes > TABLE_WINDOW_MAX_BYTES {
        // 原始byte预算先于UTF16/字符/语法扫描；超大借用字符串不能绕过有限入口做全行解析。
        return Err(MarkdownTableWindowError::SourceLimit);
    }
    let header = input.header;
    let delimiter = input.delimiter;
    validate_line(header)?;
    validate_line(delimiter)?;
    if header.row_ordinal != 0
        || delimiter.source_line
            != header
                .source_line
                .checked_add(1)
                .ok_or(MarkdownTableWindowError::InvalidSource)?
        || delimiter.byte_start
            != header
                .byte_start
                .checked_add(header.text.len())
                .ok_or(MarkdownTableWindowError::InvalidSource)?
        || delimiter.utf16_start
            != header
                .utf16_start
                .checked_add(header.text.encode_utf16().count())
                .ok_or(MarkdownTableWindowError::InvalidSource)?
    {
        return Err(MarkdownTableWindowError::InvalidSource); // header与真实delimiter不能跨未证明gap。
    }
    let mut utf16 = header
        .text
        .encode_utf16()
        .count()
        .saturating_add(delimiter.text.encode_utf16().count());
    let mut previous: Option<MarkdownTableSourceLine<'_>> = None;
    for row in input.body {
        // 每行先独立核验完整源边界，再验证相对header的绝对行身份；投影局部序号没有此权限。
        validate_line(*row)?;
        if row.row_ordinal == 0
            || header
                .source_line
                .checked_add(row.row_ordinal)
                .and_then(|line| line.checked_add(1))
                != Some(row.source_line)
            || row.byte_start < delimiter.byte_start.saturating_add(delimiter.text.len())
            || row.utf16_start
                < delimiter
                    .utf16_start
                    .saturating_add(delimiter.text.encode_utf16().count())
        {
            // 不连续或倒退的坐标不能通过后续拼接变成合法成员，整批拒绝而不裁掉坏行。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        if let Some(previous) = previous
            && (previous.row_ordinal.checked_add(1) != Some(row.row_ordinal)
                || previous.source_line.checked_add(1) != Some(row.source_line)
                || previous.byte_start.checked_add(previous.text.len()) != Some(row.byte_start)
                || previous
                    .utf16_start
                    .checked_add(previous.text.encode_utf16().count())
                    != Some(row.utf16_start))
        {
            // 本批仅允许header与首body之间一个明确gap，批次内部不能再省略未知源行。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        utf16 = utf16.saturating_add(row.text.encode_utf16().count());
        previous = Some(*row);
    }
    if utf16 > TABLE_WINDOW_MAX_UTF16 {
        return Err(MarkdownTableWindowError::SourceLimit); // 先计真实源再分配投影，不能裁掉巨行后半cell。
    }
    Ok(())
}

/// EOL只允许最后一个真实行尾；不在此层吞掉额外空行或容器prefix。
fn validate_line(line: MarkdownTableSourceLine<'_>) -> Result<(), MarkdownTableWindowError> {
    if line.byte_start.checked_add(line.text.len()).is_none()
        || line
            .utf16_start
            .checked_add(line.text.encode_utf16().count())
            .is_none()
    {
        // 所有后续局部坐标加法都必须位于已验证的真实行终点内，外来巨大offset不能触发溢出。
        return Err(MarkdownTableWindowError::InvalidSource);
    }
    let content = line.text.trim_end_matches(['\r', '\n']);
    if line.container_prefix_bytes > content.len()
        || !line.text.is_char_boundary(line.container_prefix_bytes)
        || content.contains(['\r', '\n'])
        || line.text.len() - content.len() > 2
        || (line.text.len() - content.len() == 2 && !line.text.ends_with("\r\n"))
    {
        return Err(MarkdownTableWindowError::InvalidSource); // 半行、多个EOL或非法UTF8 prefix均不能成为真实行。
    }
    Ok(())
}

/// 返回原始行内容范围，保留真实container前缀而不把投影起点当paragraph起点。
fn source_content_range(
    line: MarkdownTableSourceLine<'_>,
) -> Result<(Range<usize>, Range<usize>), MarkdownTableWindowError> {
    let content = line.text.trim_end_matches(['\r', '\n']);
    let byte_end = line
        .byte_start
        .checked_add(content.len())
        .ok_or(MarkdownTableWindowError::InvalidSource)?;
    let utf16_end = line
        .utf16_start
        .checked_add(content.encode_utf16().count())
        .ok_or(MarkdownTableWindowError::InvalidSource)?;
    Ok((line.byte_start..byte_end, line.utf16_start..utf16_end))
}

/// 投影只删除已证明container前缀；任何人工separator都不能混入真实origin。
fn append_origin<'a>(
    projection: &mut String,
    origins: &mut Vec<OriginSegment<'a>>,
    utf16: &mut usize,
    line: MarkdownTableSourceLine<'a>,
    publish: bool,
) -> Result<(), MarkdownTableWindowError> {
    let prefix = &line.text[..line.container_prefix_bytes];
    let text = &line.text[line.container_prefix_bytes..];
    let count = text.encode_utf16().count();
    let end = utf16
        .checked_add(count)
        .ok_or(MarkdownTableWindowError::InvalidSource)?;
    let byte_start = projection.len();
    origins.push(OriginSegment {
        projected: *utf16..end,
        projected_bytes: byte_start..byte_start + text.len(),
        source: line,
        content_utf16_start: line
            .utf16_start
            .checked_add(prefix.encode_utf16().count())
            .ok_or(MarkdownTableWindowError::InvalidSource)?,
        publish,
    });
    projection.push_str(text);
    *utf16 = end;
    Ok(())
}

/// 一个已解析范围只能映回同一真实片段，不能跨过未出现的源字符。
fn map_range(
    origin: &OriginSegment<'_>,
    projected: &Range<usize>,
) -> Result<Range<usize>, MarkdownTableWindowError> {
    if projected.start > projected.end
        || projected.start < origin.projected.start
        || projected.end > origin.projected.end
    {
        return Err(MarkdownTableWindowError::ProjectionBoundary); // 不以clamp掩盖parser或origin错配。
    }
    Ok(
        origin.content_utf16_start + projected.start - origin.projected.start
            ..origin.content_utf16_start + projected.end - origin.projected.start,
    )
}

/// 仅真实相交的行参与selection；gap中的caret不能激活投影虚拟首行。
fn project_selection(origins: &[OriginSegment<'_>], selection: &Range<usize>) -> Range<usize> {
    let mut projected: Option<Range<usize>> = None;
    for origin in origins {
        let source =
            origin.content_utf16_start..origin.content_utf16_start + origin.projected.len();
        let overlap = selection.start.max(source.start)..selection.end.min(source.end);
        if overlap.start <= overlap.end
            && overlap.start >= source.start
            && overlap.end <= source.end
        {
            let current = origin.projected.start + overlap.start - source.start
                ..origin.projected.start + overlap.end - source.start;
            projected = Some(projected.map_or(current.clone(), |old| {
                old.start.min(current.start)..old.end.max(current.end)
            }));
        }
    }
    projected.unwrap_or(usize::MAX..usize::MAX) // 不相交时，没有任何当前投影owner获得活动权限。
}

/// 原源paired span按当前行切开，既不重新配对也不发布穿过gap的大range。
fn add_source_underline(
    coverage: &[Range<usize>],
    structures: &[MarkdownTableUnderline],
    selection: &Range<usize>,
    spans: &mut Vec<MarkdownPresentationRun>,
    admission: RunAdmission<'_>,
) -> Result<(), MarkdownTableWindowError> {
    for structure in structures {
        // 原始paired span必须包含恰好相邻的开标签、内容、闭标签；无效元数据不能隐藏正文。
        if structure.open.len() != 3
            || structure.close.len() != 4
            || structure.content.start > structure.content.end
            || structure.open.end != structure.content.start
            || structure.content.end != structure.close.start
        {
            return Err(MarkdownTableWindowError::InvalidUnderline);
        }
        let owner = structure.open.start..structure.close.end;
        let marker = if markdown_selection_intersects(selection, &owner) {
            MarkdownPresentationStyles::MARKER
        } else {
            MarkdownPresentationStyles::MARKER.union(MarkdownPresentationStyles::HIDDEN_MARKER)
        };
        for coverage in coverage {
            // 跨省略段的pair仅在每条实际行内叠加，不能一次发布贯穿gap的大range。
            for (range, styles) in [
                (&structure.content, MarkdownPresentationStyles::UNDERLINE),
                (&structure.open, marker),
                (&structure.close, marker),
            ] {
                let clipped = range.start.max(coverage.start)..range.end.min(coverage.end);
                if clipped.start < clipped.end {
                    // underline与parser/container/inherited使用同一分配前入口，避免元数据乘行数先扩张。
                    admit_window_run(
                        spans,
                        MarkdownPresentationRun {
                            utf16_range: clipped,
                            styles,
                        },
                        admission,
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// 每个来源和最终分割结果共享分配前预算；Vec任何时点最多8192个run。
fn admit_window_run(
    runs: &mut Vec<MarkdownPresentationRun>,
    run: MarkdownPresentationRun,
    admission: RunAdmission<'_>,
) -> Result<(), MarkdownTableWindowError> {
    if run.utf16_range.is_empty() || run.styles.is_empty() {
        // 无字符/无样式不产生语义端点，也不消耗可发布run容量。
        return Ok(());
    }
    if runs.len() >= admission.maximum {
        // 检查发生在push之前；相交元数据最多展开8192项，第8193项立即拒绝整个候选。
        return Err(MarkdownTableWindowError::SemanticLimit);
    }
    if let Some(quota) = admission.quota {
        if admission.output {
            quota.admit_output()?;
        } else {
            quota.admit_metadata(std::mem::size_of::<MarkdownPresentationRun>())?;
        }
    }
    runs.push(run);
    Ok(())
}

/// 有限区间端点扫描生成不相交runs；不分配按整个源offset索引的数组。
fn disjoint_window_runs(
    spans: Vec<MarkdownPresentationRun>,
    admission: RunAdmission<'_>,
) -> Result<Vec<MarkdownPresentationRun>, MarkdownTableWindowError> {
    if spans.len() > admission.maximum {
        return Err(MarkdownTableWindowError::SemanticLimit); // 所有额外underline/容器span也计入同一批预算。
    }
    let mut transitions = Vec::new();
    for span in spans {
        // 每个有效style产生一对有限端点；排序扫描代替逐区间反复遍历全部span的平方成本。
        for style_bit in span.styles.set_bits() {
            if let Some(quota) = admission.quota {
                // 端点也先准入；不会在构造第二窗完整span后才发现批次metadata超限。
                quota.admit_metadata(2 * std::mem::size_of::<MarkdownStyleTransition>())?;
            }
            transitions.push(MarkdownStyleTransition {
                utf16_offset: span.utf16_range.start,
                style_bit,
                delta: 1,
            });
            transitions.push(MarkdownStyleTransition {
                utf16_offset: span.utf16_range.end,
                style_bit,
                delta: -1,
            });
        }
    }
    transitions.sort_unstable_by_key(|transition| transition.utf16_offset);
    let mut runs: Vec<MarkdownPresentationRun> = Vec::new();
    let mut counts = [0_u16; u32::BITS as usize];
    let mut index = 0;
    let mut start = transitions
        .first()
        .map_or(0, |transition| transition.utf16_offset);
    while index < transitions.len() {
        let end = transitions[index].utf16_offset;
        let styles = MarkdownPresentationStyles::from_presence_counts(&counts);
        if start < end && !styles.is_empty() {
            if let Some(last) = runs.last_mut()
                && last.styles == styles
                && last.utf16_range.end == start
            {
                last.utf16_range.end = end; // 仅真实相邻的同样式区间合并，gap不获得假覆盖。
            } else {
                // 不同样式端点可能把输入span分割成更多run，输出也必须在分配前受相同上限约束。
                admit_window_run(
                    &mut runs,
                    MarkdownPresentationRun {
                        utf16_range: start..end,
                        styles,
                    },
                    admission.output(),
                )?;
            }
        }
        while index < transitions.len() && transitions[index].utf16_offset == end {
            let transition = transitions[index];
            let count = &mut counts[transition.style_bit];
            if transition.delta > 0 {
                *count += 1; // 当前预算小于u16范围，有限重叠不会溢出。
            } else {
                *count -= 1; // 每个span都有同代次配对端点，原计数不能下溢。
            }
            index += 1;
        }
        start = end;
    }
    Ok(runs)
}

#[cfg(test)]
mod tests;
