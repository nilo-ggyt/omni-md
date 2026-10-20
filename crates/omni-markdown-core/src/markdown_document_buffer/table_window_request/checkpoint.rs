//! 真实行边界上的单表回放；只保结构与数学状态，不重建全局前沿或保存源正文。

use super::*;
use pulldown_cmark::block_frontier::{TableContinuationState, TableStep};

#[derive(Clone, Debug)]
/// checkpoint 与数学状态来自同一已完成行边界；物理行号仅由 opaque checkpoint 保存并恢复。
/// Core 只另记真实 fact 的后继 ordinal/byte/UTF16，不从窗口局部下标或 EOF 行尾推算坐标。
pub(super) struct Boundary {
    pub(super) checkpoint: TableCheckpoint,
    pub(super) math: MathContext,
    pub(super) next_row_ordinal: usize,
    pub(super) next_byte: usize,
    pub(super) next_utf16: usize,
}

impl Boundary {
    /// H/D 已由真实 Global 接受，且 H 的 HeaderActive 扫描完成后，才能保存首条 body 的入口。
    pub(super) fn after_delimiter(
        checkpoint: TableCheckpoint,
        header: &GlobalTableFact,
        delimiter_utf16_end: usize,
        math: MathContext,
    ) -> Self {
        Self {
            checkpoint,
            math,
            next_row_ordinal: 1,
            next_byte: header.separator_byte_end,
            next_utf16: delimiter_utf16_end,
        }
    }

    /// Row fact 后的 checkpoint 已接受本行；math 必须同样扫完本行，不能提前绑定前一行状态。
    pub(super) fn after_row(
        checkpoint: TableCheckpoint,
        row: TableRowFact,
        math: MathContext,
    ) -> Self {
        Self {
            checkpoint,
            math,
            next_row_ordinal: row.row_ordinal + 1,
            next_byte: row.byte_end,
            next_utf16: row.utf16_end,
        }
    }
}

#[derive(Clone, Debug)]
/// 三个有限锚点共享原 spine 页和 fuel；不保存文档 root、Native 对象、正文或逐行目录。
pub(super) struct ReplayAnchors {
    pub(super) header: GlobalTableFact,
    pub(super) header_utf16: Range<usize>,
    pub(super) delimiter_utf16: Range<usize>,
    pub(super) header_math: MathContext,
    pub(super) base: Boundary,
    pub(super) chunk: Boundary,
    pub(super) halo: Option<Boundary>,
}

#[derive(Debug)]
pub(super) enum ReplayOutcome {
    // 父批次已预扣 RequiredState 与固定 4096 字节工作区；此分配只移交一个有限 ticket，不复制完整表。
    Window(Box<MarkdownTableStructuralWindow>),
    End {
        end: VerifiedTableEnd,
        terminal: Box<MarkdownTableStructuralWindow>,
    },
}

struct PendingRow {
    fact: TableRowFact,
    checkpoint: TableCheckpoint,
    scanner: MathContextScan,
}

/// 临时续体只回放一张已验证表；结束后不接管或删除批次仍持有的主 Global。
pub(super) struct ReplayRequest {
    source: MarkdownTableSourceIdentity,
    anchors: ReplayAnchors,
    target: usize,
    boundary: Boundary,
    previous: Option<Boundary>,
    table: Option<TableContinuationState>,
    pending: Option<PendingRow>,
    // 只保最后完成行的metadata和before seed，供不存在目标时签真实terminal窗口。
    last_completed: Option<(TableRowFact, Boundary, Option<Boundary>)>,
    consumed: bool,
    failed: Option<MarkdownTableRequestError>,
}

impl ReplayRequest {
    pub(super) fn new(
        source: MarkdownTableSourceIdentity,
        anchors: ReplayAnchors,
        target: usize,
    ) -> Result<Self, MarkdownTableRequestError> {
        validate_columns(anchors.header.columns)?;
        if anchors.header.header_byte_start >= anchors.header.header_byte_end
            || anchors.header.header_byte_end != anchors.header.separator_byte_start
            || anchors.header.separator_byte_start >= anchors.header.separator_byte_end
            || anchors.header.separator_byte_end > source.byte_len()
            || anchors.header_utf16.start >= anchors.header_utf16.end
            || anchors.header_utf16.end != anchors.delimiter_utf16.start
            || anchors.delimiter_utf16.start >= anchors.delimiter_utf16.end
            || anchors.base.next_row_ordinal != 1
            || anchors.base.next_byte != anchors.header.separator_byte_end
            || anchors.base.next_utf16 != anchors.delimiter_utf16.end
        {
            // 这些字段只由真实结构路径构造；不一致时撤销整次回放，不修补出一个猜测种子。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut selected = None;
        for candidate in std::iter::once(&anchors.base)
            .chain(std::iter::once(&anchors.chunk))
            .chain(anchors.halo.iter())
        {
            if candidate.next_row_ordinal == 0
                || candidate.next_byte > source.byte_len()
                || candidate.next_byte < anchors.base.next_byte
                || candidate.next_utf16 < anchors.base.next_utf16
            {
                // 固定三个锚点均须属同表已接受前缀；目标未命中的锚点也不能悄悄携带倒退坐标。
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if candidate.next_row_ordinal <= target
                && selected.as_ref().is_none_or(|previous: &&Boundary| {
                    previous.next_row_ordinal < candidate.next_row_ordinal
                })
            {
                // 只选目标之前最近的真实入口；没有按 ordinal 构造新的 parser seed。
                selected = Some(candidate);
            }
        }
        let boundary = if target == 0 {
            // Header 已有独立真实 math seed；H-only 不需要让续体读取第一条 body。
            anchors.base.clone()
        } else {
            selected
                .ok_or(MarkdownTableRequestError::CoverageRequiresCheckpoint)?
                .clone()
        };
        let previous = std::iter::once(&anchors.base)
            .chain(std::iter::once(&anchors.chunk))
            .chain(anchors.halo.iter())
            .find(|candidate| {
                candidate.next_row_ordinal.checked_add(1) == Some(boundary.next_row_ordinal)
            })
            .cloned();
        let table =
            (target != 0).then(|| TableContinuationState::resume(boundary.checkpoint.clone()));
        Ok(Self {
            source,
            anchors,
            target,
            boundary,
            previous,
            table,
            pending: None,
            last_completed: None,
            consumed: false,
            failed: None,
        })
    }

    /// 批次在外层处理取消并累计原总 fuel；本层每次仍核当前 root，并只执行一个有界子阶段。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(semantic::SemanticWork, Option<ReplayOutcome>), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            // 首错保持撤权，不能凭同一旧 checkpoint 在下一回调自动重试并补签结果。
            return Err(error);
        }
        let result = if !self.source.is_current(document) {
            Err(MarkdownTableRequestError::SourceSuperseded)
        } else if !(3072..=65536).contains(&budget) || self.consumed {
            Err(MarkdownTableRequestError::InvalidRequest)
        } else {
            self.advance_inner(document, budget)
        };
        if let Err(error) = result {
            // 临时行扫描和续体一并释放；主 Global 由调用方持有，不会在失败后从源起点重建。
            self.failed = Some(error);
            self.pending = None;
            self.last_completed = None;
            self.table = None;
        }
        result
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(semantic::SemanticWork, Option<ReplayOutcome>), MarkdownTableRequestError> {
        if self.target == 0 {
            // H-only 只移交既有真实 metadata；不运行 body 分类，也不把 context-only H 混入 body。
            self.consumed = true;
            let ticket = self.window(None, self.anchors.header_math);
            return Ok((
                semantic::SemanticWork {
                    structural_transitions: 64,
                    ..semantic::SemanticWork::default()
                },
                Some(ReplayOutcome::Window(Box::new(ticket))),
            ));
        }
        if self.pending.is_some() {
            // 上一步只接受了结构行；本步必须完成原源 math 扫描，才能移动可公开边界。
            return self.advance_math(document, budget);
        }
        let tree = tree_height(document);
        let child = child_budget(budget, tree)?;
        let table = self
            .table
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let frame = document
            .table_source_frame(table.next_byte(), child)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = table
            .advance_bounded(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                child,
                child,
            )
            .map_err(|error| {
                MarkdownTableRequestError::Global(GlobalFrontierError::Frontier(error))
            });
        // 回收成功后才消费step，不能让原parser提前终止遮住真实映射错误。
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = step?;
        let measured = table.last_work();
        let work = semantic::SemanticWork {
            source_bytes: measured.byte_reads,
            source_tree_steps: tree,
            structural_transitions: measured.transitions + tree + 64,
            ..semantic::SemanticWork::default()
        };
        match step {
            TableStep::Row { fact, .. } => {
                if fact.header_byte_start != self.anchors.header.header_byte_start
                    || fact.row_ordinal != self.boundary.next_row_ordinal
                    || fact.byte_start != self.boundary.next_byte
                    || fact.utf16_start != self.boundary.next_utf16
                {
                    // 不接受另一个表或非连续回放的 Row；这些坐标不能由相同 ordinal 替代。
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                let checkpoint = table
                    .checkpoint()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                let start = fact
                    .byte_start
                    .checked_add(fact.container_prefix_bytes)
                    .filter(|start| *start <= fact.byte_end)
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                let scanner = MathContextScan::new(
                    self.source.generation(),
                    self.source.byte_len(),
                    start..fact.byte_end,
                    self.boundary.math,
                )
                .map_err(MarkdownTableRequestError::Math)?;
                self.pending = Some(PendingRow {
                    fact,
                    checkpoint,
                    scanner,
                });
                Ok((work, None))
            }
            TableStep::Ended { .. } => {
                let end = table
                    .verified_end()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                if end.source_revision() != self.source.generation()
                    || end.header_byte_start() != self.anchors.header.header_byte_start
                    || end.byte_end() != self.boundary.next_byte
                    || end.utf16_end() != self.boundary.next_utf16
                {
                    // 只交付此续体的真实表尾；未命中目标不生成空窗口，也不推断目标已激活。
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                // 超界目标本身不能授权append。额外保留真实末行票，随后仍走同批语义/配额。
                let mut terminal = if let Some((row, before, halo)) = self.last_completed.take() {
                    self.anchors.chunk = before.clone();
                    self.anchors.halo = halo;
                    self.window(Some(row), before.math)
                } else if end.last_body_row_ordinal().is_none() {
                    // H/D-only也有真实H事实和header seed，不把空body投影冒充虚构row1。
                    let mut window = self.window(None, self.anchors.header_math);
                    window.include_header = true;
                    window
                } else {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                };
                terminal.table_end = Some(end.clone());
                self.consumed = true;
                self.table = None;
                Ok((
                    work,
                    Some(ReplayOutcome::End {
                        end,
                        terminal: Box::new(terminal),
                    }),
                ))
            }
            TableStep::Progress { .. } | TableStep::NeedInput { .. } => {
                // 短 frame 与未完成分类均只保续体；不能在这里重授 checkpoint 或结束权限。
                Ok((work, None))
            }
        }
    }

    fn advance_math(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(semantic::SemanticWork, Option<ReplayOutcome>), MarkdownTableRequestError> {
        let tree = tree_height(document);
        let child = child_budget(budget, tree)?;
        let row = self
            .pending
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let frame = document
            .table_source_frame(row.scanner.next_byte(), child)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = row
            .scanner
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                MathSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
                child,
            )
            .map_err(MarkdownTableRequestError::Math);
        // 回收成功后才消费step，不能让原parser提前终止遮住真实映射错误。
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = step?;
        let (measured, complete) = match step {
            MathContextStep::Progress { work, .. } | MathContextStep::NeedInput { work, .. } => {
                (work, None)
            }
            MathContextStep::Complete { context, work } => (work, Some(context)),
        };
        let work = semantic::SemanticWork {
            source_bytes: measured.byte_reads,
            source_tree_steps: tree,
            structural_transitions: measured.transitions + tree + 64,
            ..semantic::SemanticWork::default()
        };
        let Some(context) = complete else {
            // math 状态还未覆盖完整物理行；保留原 before-row 边界，不能提交部分结果。
            return Ok((work, None));
        };
        let row = self
            .pending
            .take()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if row.fact.row_ordinal == self.target {
            // 目标 seed 是扫描本行之前的真实状态；after-row 仅供 scope 和后继续行使用。
            self.anchors.chunk = self.boundary.clone();
            self.anchors.halo = self.previous.take();
            let before = self.boundary.math;
            self.boundary = Boundary::after_row(row.checkpoint, row.fact, context);
            self.consumed = true;
            self.table = None;
            return Ok((
                work,
                Some(ReplayOutcome::Window(Box::new(
                    self.window(Some(row.fact), before),
                ))),
            ));
        }
        // 非目标行也真实扫描且收费；仅覆盖最后一行metadata，不随已扫行数增长。
        self.last_completed = Some((row.fact, self.boundary.clone(), self.previous.clone()));
        self.previous = Some(std::mem::replace(
            &mut self.boundary,
            Boundary::after_row(row.checkpoint, row.fact, context),
        ));
        Ok((work, None))
    }

    fn window(
        &self,
        row: Option<TableRowFact>,
        before: MathContext,
    ) -> MarkdownTableStructuralWindow {
        let mut math_rows = Vec::with_capacity(if row.is_some() { 2 } else { 1 });
        math_rows.push(MarkdownTableRowMathContext {
            row_ordinal: 0,
            context: self.anchors.header_math,
        });
        let mut body = Vec::with_capacity(usize::from(row.is_some()));
        if let Some(row) = row {
            // 只物化目标这一条真实 body；H/D 作为独立语法上下文，绝不补齐中间省略行。
            math_rows.push(MarkdownTableRowMathContext {
                row_ordinal: row.row_ordinal,
                context: before,
            });
            body.push(row);
        }
        MarkdownTableStructuralWindow {
            source: self.source.clone(),
            header: self.anchors.header,
            checkpoint: self.boundary.checkpoint.clone(),
            header_utf16: self.anchors.header_utf16.clone(),
            delimiter_utf16: self.anchors.delimiter_utf16.clone(),
            body,
            inline_rows: row
                .map(|_| {
                    self.boundary
                        .checkpoint
                        .last_inline_row()
                        .expect("目标原行已经真实接受，after-row checkpoint 保留同一行来源")
                })
                .into_iter()
                .collect(),
            math_rows,
            include_header: self.target == 0,
            next_required_row: None,
            table_end: None,
            replay: Some(self.anchors.clone()),
        }
    }
}
