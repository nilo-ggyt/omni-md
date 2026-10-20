//! 原结构票据上的单格编辑准备；只扫描真实4KiB源片，不构造整行投影或inline Ready。

use super::super::table_cell_edit::{
    MarkdownTableCellEditPlan, TableCellEditContext, missing_cell_separators,
};
use super::*;
use pulldown_cmark::block_frontier::{TableCellSemanticScanner, TableDelimiterScanner};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Ready 只说明原格结构端点已认证；编辑计划和只读来源择一消费，不表达预览样式或 Native 挂载。
pub enum MarkdownTableCellEditStep {
    Progress { work: SemanticWork },
    Ready { work: SemanticWork },
    NotTable { work: SemanticWork },
}

/// 所有权只含原结构续体、有限坐标和一个词法游标；不持Doc、映射或正文。
pub struct MarkdownTableCellEditRequest {
    source: MarkdownTableSourceIdentity,
    structure: Option<MarkdownTableWindowStructureRequest>,
    ticket: Option<MarkdownTableStructuralWindow>,
    context: Option<TableCellEditContext>,
    scanner: Option<TableCellSemanticScanner>,
    // 同一真实 separator 只流式提取当前列；不为对齐创建整行字符串或完整 alignment Vec。
    delimiter: Option<TableDelimiterScanner>,
    delimiter_next: usize,
    delimiter_end: usize,
    alignment: Option<crate::MarkdownTableAlignment>,
    next: usize,
    content_start: usize,
    utf16: usize,
    trimmed_utf16: Range<usize>,
    selected: Option<Range<usize>>,
    row: usize,
    column: usize,
    ready: Option<VerifiedMarkdownTableCellSource>,
    finished: bool,
    failed: Option<MarkdownTableRequestError>,
}

impl MarkdownDocumentBuffer {
    /// 按真实Header和绝对行列准备原格；请求坐标不能绕开结构producer的成员与End认证。
    pub fn table_cell_edit_request(
        &self,
        header_byte_start: usize,
        row_ordinal: usize,
        column: usize,
    ) -> Result<MarkdownTableCellEditRequest, MarkdownTableRequestError> {
        self.original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        let structure = self.table_window_structure_request(
            MarkdownTableWindowTarget::RequiredRow {
                header_byte_start,
                row_ordinal,
            },
            0..0,
        )?;
        Ok(MarkdownTableCellEditRequest {
            source: structure.source.clone(),
            structure: Some(structure),
            ticket: None,
            context: None,
            scanner: None,
            delimiter: None,
            delimiter_next: 0,
            delimiter_end: 0,
            alignment: None,
            next: 0,
            content_start: 0,
            utf16: 0,
            trimmed_utf16: 0..0,
            selected: None,
            row: row_ordinal,
            column,
            ready: None,
            finished: false,
            failed: None,
        })
    }
}

impl MarkdownTableCellEditRequest {
    /// 只移交原 RequiredRow 回放票，不建立结构 Global；所有后续扫描仍走同一个原实现。
    pub(super) fn from_ticket(
        ticket: MarkdownTableStructuralWindow,
        row: usize,
        column: usize,
    ) -> Self {
        Self {
            source: ticket.source.clone(),
            structure: None,
            ticket: Some(ticket),
            context: None,
            scanner: None,
            delimiter: None,
            delimiter_next: 0,
            delimiter_end: 0,
            alignment: None,
            next: 0,
            content_start: 0,
            utf16: 0,
            trimmed_utf16: 0..0,
            selected: None,
            row,
            column,
            ready: None,
            finished: false,
            failed: None,
        }
    }
    /// 每次结构或原格扫描均至多借4096字节；取消/失效丢弃候选，不发布部分编辑权限。
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableCellEditStep, MarkdownTableRequestError> {
        let result = (|| {
            document
                .original_backing_health()
                .map_err(MarkdownTableRequestError::Source)?;
            if let Some(error) = self.failed {
                return Err(error);
            }
            if cancelled {
                return Err(MarkdownTableRequestError::Cancelled);
            }
            if !self.source.is_current(document) {
                return Err(MarkdownTableRequestError::SourceSuperseded);
            }
            if !(4096..=65536).contains(&budget) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.advance_inner(document, budget.min(4096))
        })();
        match result {
            Ok(step) => Ok(step),
            Err(cause) => {
                // 子阶段的坐标错误不能覆盖实际源错误；释放全部无正文续体后才保存首因。
                let error = document
                    .original_backing_health()
                    .err()
                    .map(MarkdownTableRequestError::Source)
                    .unwrap_or(cause);
                self.failed = Some(error);
                self.structure = None;
                self.ticket = None;
                self.context = None;
                self.scanner = None;
                self.delimiter = None;
                self.selected = None;
                self.ready = None;
                Err(error)
            }
        }
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableCellEditStep, MarkdownTableRequestError> {
        if self.finished {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if self.ready.is_some() {
            return Ok(MarkdownTableCellEditStep::Ready {
                work: SemanticWork::default(),
            });
        }
        if let Some(structure) = self.structure.as_mut() {
            let step = structure.advance(document, false, budget)?;
            let measured = match step {
                MarkdownTableStructureStep::Progress { work }
                | MarkdownTableStructureStep::StructureReady { work }
                | MarkdownTableStructureStep::NotTable { work }
                | MarkdownTableStructureStep::TableEndReached { work } => work,
            };
            let work = SemanticWork {
                source_bytes: measured.byte_reads,
                source_tree_steps: measured.source_tree_steps,
                structural_transitions: measured.transitions,
                ..SemanticWork::default()
            };
            match step {
                MarkdownTableStructureStep::Progress { .. } => {}
                MarkdownTableStructureStep::StructureReady { .. } => {
                    self.ticket = structure.ready.take();
                    self.structure = None;
                }
                MarkdownTableStructureStep::NotTable { .. }
                | MarkdownTableStructureStep::TableEndReached { .. } => {
                    // 所需格不存在时结束这个请求；真实表尾不能凭缺格坐标变成目标行。
                    self.structure = None;
                    self.finished = true;
                    return Ok(MarkdownTableCellEditStep::NotTable { work });
                }
            }
            return Ok(MarkdownTableCellEditStep::Progress { work });
        }
        if self.context.is_none() {
            let ticket = self
                .ticket
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            let header = ticket.header;
            if self.column >= header.columns {
                return Err(MarkdownTableRequestError::Window(
                    MarkdownTableWindowError::ColumnLimit,
                ));
            }
            let (start, end, prefix, utf16) = if self.row == 0 {
                (
                    header.header_byte_start,
                    header.header_byte_end,
                    header.header_prefix_bytes,
                    ticket.header_utf16.clone(),
                )
            } else {
                let row = ticket
                    .body
                    .iter()
                    .find(|row| row.row_ordinal == self.row)
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                (
                    row.byte_start,
                    row.byte_end,
                    row.container_prefix_bytes,
                    row.utf16_start..row.utf16_end,
                )
            };
            let (header_end, header_reads) =
                line_content_end(document, header.header_byte_start..header.header_byte_end)?;
            let (delimiter_end, delimiter_reads) = line_content_end(
                document,
                header.separator_byte_start..header.separator_byte_end,
            )?;
            let (content_end, row_reads) = line_content_end(document, start..end)?;
            self.next = start;
            self.content_start = start + prefix;
            self.utf16 = utf16.start;
            let inline_row = if self.row == 0 {
                ticket.checkpoint.header_inline_row()
            } else {
                ticket
                    .inline_rows
                    .iter()
                    .find(|row| row.row_ordinal() == self.row)
                    .cloned()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?
            };
            let math = ticket
                .math_rows
                .iter()
                .find(|row| row.row_ordinal == self.row)
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
                .context;
            self.scanner = Some(
                inline_row
                    .cell_scanner(self.column, math)
                    .map_err(|cause| MarkdownTableRequestError::Global(cause.into()))?,
            );
            self.context = Some(TableCellEditContext {
                header: header.header_byte_start..header_end,
                delimiter: header.separator_byte_start..delimiter_end,
                row_bytes: start..content_end,
                row_utf16: utf16.start..utf16.end - (end - content_end),
                row_ordinal: self.row,
                column: self.column,
                columns: header.columns,
                end: ticket.table_end.clone(),
            });
            // 原 Global 已认证 separator 的真实容器前缀；只跳过它，不在 Native 重识别 quote/list。
            self.delimiter_next = header.separator_byte_start + header.separator_prefix_bytes;
            self.delimiter_end = delimiter_end;
            self.delimiter = Some(TableDelimiterScanner::new());
            self.ticket = None; // 原身份和所需End已移入同一计划上下文，不保整份窗口metadata。
            return Ok(MarkdownTableCellEditStep::Progress {
                work: SemanticWork {
                    source_bytes: header_reads + delimiter_reads + row_reads,
                    structural_transitions: 64,
                    ..SemanticWork::default()
                },
            });
        }
        if let Some(delimiter) = self.delimiter.as_mut() {
            // 与原 LineReader 共用唯一 delimiter 转换，每轮仍按真实 byte/树定位/状态工作收费。
            // 整行有效和列数已由同一 Global/结构票认证；目标列闭合即可停，不重扫该列之后的分隔行。
            let context = self.context.as_ref().unwrap();
            if self.delimiter_next < self.delimiter_end {
                let tree = tree_height(document);
                let count =
                    ((budget - tree - 64) / 4).min(self.delimiter_end - self.delimiter_next);
                let frame = document
                    .table_source_frame(self.delimiter_next, count)
                    .map_err(MarkdownTableRequestError::Source)?;
                let available = frame.bytes.len().min(count);
                if available == 0 {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                let mut used = 0;
                let mut completed = false;
                for byte in &frame.bytes[..available] {
                    used += 1;
                    if let Some((column, alignment)) = delimiter.push(*byte)
                        && column == context.column
                    {
                        self.alignment = Some(table_alignment(alignment));
                        completed = true;
                        break;
                    }
                }
                // 实际只消费到目标 pipe 的部分才计源读取；未扫描的同 frame 后缀不冒充工作。
                // 归还失败保留原 typed errno，不能以已取得 alignment 的局部成功覆盖它。
                self.delimiter_next += used;
                frame.finish().map_err(MarkdownTableRequestError::Source)?;
                if completed {
                    self.delimiter = None;
                }
                return Ok(MarkdownTableCellEditStep::Progress {
                    work: SemanticWork {
                        source_bytes: used,
                        source_tree_steps: tree,
                        structural_transitions: used * 3 + 64,
                        ..SemanticWork::default()
                    },
                });
            }
            if let Some((column, alignment)) = delimiter.final_column()
                && column == context.column
            {
                self.alignment = Some(table_alignment(alignment));
            }
            // 没有尾 pipe 的真实末列只能在 content_end 收口；未知/缺失值不能默认左对齐。
            if delimiter.columns() != Some(context.columns) || self.alignment.is_none() {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.delimiter = None;
            return Ok(MarkdownTableCellEditStep::Progress {
                work: SemanticWork {
                    structural_transitions: 64,
                    ..SemanticWork::default()
                },
            });
        }
        let context = self.context.as_ref().unwrap();
        let scanner = self.scanner.as_mut().unwrap();
        let tree = tree_height(document);
        if self.next < context.row_bytes.end {
            // 逐字节转换与原树定位共同收费；不以4KiB借用宣称隐藏扫描了整格。
            let count = ((budget - tree - 64) / 6).min(context.row_bytes.end - self.next);
            let frame = document
                .table_source_frame(self.next, count)
                .map_err(MarkdownTableRequestError::Source)?;
            let used = frame.bytes.len().min(count);
            if used == 0 {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            for (offset, byte) in frame.bytes[..used].iter().enumerate() {
                let at = self.next + offset;
                let before = self.utf16;
                // 原immutable/piece源已保证合法UTF8；按真实leading byte计量，continuation跨frame不重复。
                self.utf16 += match byte {
                    0..=0x7f | 0xc2..=0xef => 1,
                    0xf0..=0xf4 => 2,
                    _ => 0,
                };
                if at < self.content_start {
                    continue;
                } // 容器字节参与原坐标，但不进入格词法。
                if at == self.content_start {
                    self.trimmed_utf16 = before..before;
                }
                let prior_trimmed = self.trimmed_utf16.clone();
                if scanner.push(*byte).is_some() && scanner.cell_count() == self.column + 1 {
                    self.selected = Some(prior_trimmed); // 关闭pipe之前的真实trim端点，不把新leading pipe写入原格。
                }
                let trimmed = scanner.trimmed_range();
                if trimmed.start == at {
                    self.trimmed_utf16.start = before;
                } else if trimmed.start == at + 1 {
                    self.trimmed_utf16.start = self.utf16;
                }
                if trimmed.end == at {
                    self.trimmed_utf16.end = before;
                } else if trimmed.end == at + 1 {
                    self.trimmed_utf16.end = self.utf16;
                }
            }
            self.next += used;
            frame.finish().map_err(MarkdownTableRequestError::Source)?;
            return Ok(MarkdownTableCellEditStep::Progress {
                work: SemanticWork {
                    source_bytes: used,
                    source_tree_steps: tree,
                    structural_transitions: used * 5 + 64, // 原格词法/UTF16之外，同源 math 的两个逐字节转换也真实收费。
                    ..SemanticWork::default()
                },
            });
        }
        if scanner.finish().is_some() && scanner.cell_count() == self.column + 1 {
            self.selected = Some(self.trimmed_utf16.clone());
        }
        let (range, separators, implicit_empty) = match self.selected.take() {
            Some(range) => (range, 0, false),
            None => {
                // 原词法确认目标属于连续缺格；只按真实已有尾pipe补足所需分隔符。
                let separators = missing_cell_separators(
                    self.column,
                    scanner.cell_count(),
                    scanner.trailing_pipe(),
                )
                .map_err(MarkdownTableRequestError::Window)?;
                (
                    context.row_utf16.end..context.row_utf16.end,
                    separators,
                    true,
                )
            }
        };
        // 编辑和只读布局消费同一原结构端点；此处不建立 replacement，更不签 inline 样式。
        self.ready = Some(VerifiedMarkdownTableCellSource {
            source: self.source.clone(),
            context: self.context.take().unwrap(),
            range,
            implicit_empty,
            separators,
            alignment: self.alignment.expect("原 separator 完整结束后才签发格票"),
            // 原 Scanner 已证实的补列事实与端点同票移交，显示消费不能再把它丢成一对裸整数。
            // seed 仅含纯值与原 Arc/Atomic scope 页，允许原 layout Source 的真实 worker 跨线程释放。
            inline_seed: scanner.take_semantic_seed().map(std::sync::Arc::new),
        });
        self.scanner = None;
        Ok(MarkdownTableCellEditStep::Ready {
            work: SemanticWork {
                structural_transitions: 64,
                ..SemanticWork::default()
            },
        })
    }

    /// 同一原端点只可交给一个消费者；编辑沿原 replacement 规则，不把 raw 来源解释为预览 Ready。
    pub fn take_plan_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<MarkdownTableCellEditPlan>, MarkdownTableRequestError> {
        self.take_raw_cell_for(document)?
            .map(|cell| document.plan_verified_table_cell_draft(cell))
            .transpose()
    }

    /// 巨格同样只移交原 Scanner 证实的 raw 格；不占草稿槽，不复制源行，也没有 inline 权限。
    pub fn take_cell_source_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<VerifiedMarkdownTableCellSource>, MarkdownTableRequestError> {
        self.take_raw_cell_for(document)
    }

    /// 共享原请求的一次消费权；健康门先于首因与代次判断，不能从旧整数坐标签发另一个格。
    fn take_raw_cell_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<VerifiedMarkdownTableCellSource>, MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        let cell = self.ready.take();
        if cell.is_some() {
            self.finished = true;
        }
        Ok(cell)
    }
}

/// 原 Parser 的默认方向与显式左对齐具有相同布局语义；这里仅映 enum，不解释字节。
fn table_alignment(alignment: pulldown_cmark::Alignment) -> crate::MarkdownTableAlignment {
    match alignment {
        pulldown_cmark::Alignment::None | pulldown_cmark::Alignment::Left => {
            crate::MarkdownTableAlignment::Left
        }
        pulldown_cmark::Alignment::Center => crate::MarkdownTableAlignment::Center,
        pulldown_cmark::Alignment::Right => crate::MarkdownTableAlignment::Right,
    }
}

/// 只借真实末尾一到两字节确认EOL；不要求末尾UTF8片段自身成str，也不复制整行。
fn line_content_end(
    document: &MarkdownDocumentBuffer,
    range: Range<usize>,
) -> Result<(usize, usize), MarkdownTableRequestError> {
    let mut end = range.end;
    let mut reads = 0;
    if range.is_empty() {
        return Ok((end, reads));
    }
    let frame = document
        .table_source_frame(end - 1, 1)
        .map_err(MarkdownTableRequestError::Source)?;
    let last = frame.bytes[0];
    reads += 1;
    frame.finish().map_err(MarkdownTableRequestError::Source)?;
    if matches!(last, b'\r' | b'\n') {
        end -= 1;
    }
    if last == b'\n' && end > range.start {
        let frame = document
            .table_source_frame(end - 1, 1)
            .map_err(MarkdownTableRequestError::Source)?;
        if frame.bytes[0] == b'\r' {
            end -= 1;
        }
        reads += 1;
        frame.finish().map_err(MarkdownTableRequestError::Source)?;
    }
    Ok((end, reads))
}
