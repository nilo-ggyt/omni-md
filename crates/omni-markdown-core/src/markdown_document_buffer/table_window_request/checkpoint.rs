//! 真实行边界上的单表回放；只保结构checkpoint，不重建全局前沿或保存源正文。

use super::*;
use pulldown_cmark::block_frontier::{TableContinuationState, TableStep};

#[derive(Clone, Debug)]
/// checkpoint 来自同一已接受行边界；物理行号仅由 opaque checkpoint 保存并恢复。
/// Core 只另记真实 fact 的后继 ordinal/byte/UTF16，不从窗口局部下标或 EOF 行尾推算坐标。
pub(super) struct Boundary {
    pub(super) checkpoint: TableCheckpoint,
    pub(super) next_row_ordinal: usize,
    pub(super) next_byte: usize,
    pub(super) next_utf16: usize,
}

impl Boundary {
    /// H/D由真实Global接受后保存首条body入口，不再为公式状态重复扫描header。
    pub(super) fn after_delimiter(
        checkpoint: TableCheckpoint,
        header: &GlobalTableFact,
        delimiter_utf16_end: usize,
    ) -> Self {
        Self {
            checkpoint,
            next_row_ordinal: 1,
            next_byte: header.separator_byte_end,
            next_utf16: delimiter_utf16_end,
        }
    }

    /// Row fact和checkpoint来自同一次结构接受；后继坐标不依赖额外正文重放。
    pub(super) fn after_row(checkpoint: TableCheckpoint, row: TableRowFact) -> Self {
        Self {
            checkpoint,
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

/// 临时续体只回放一张已验证表；结束后不接管或删除批次仍持有的主 Global。
pub(super) struct ReplayRequest {
    source: MarkdownTableSourceIdentity,
    anchors: ReplayAnchors,
    target: usize,
    boundary: Boundary,
    previous: Option<Boundary>,
    table: Option<TableContinuationState>,
    // 只保最后完成行与它的真实前驱边界，供不存在目标时签真实terminal窗口。
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
            // H/D已有独立真实结构事实；H-only不需要让续体读取第一条body。
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
            // 失败时释放续体与末行metadata；主Global由调用方持有，不从源起点重建。
            self.failed = Some(error);
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
            let ticket = self.window(None);
            return Ok((
                semantic::SemanticWork {
                    structural_transitions: 64,
                    ..semantic::SemanticWork::default()
                },
                Some(ReplayOutcome::Window(Box::new(ticket))),
            ));
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
                // 原TableContinuation已完成本行与容器词法；直接消费同一checkpoint，不重读正文。
                if fact.row_ordinal == self.target {
                    self.anchors.chunk = self.boundary.clone();
                    self.anchors.halo = self.previous.take();
                    self.boundary = Boundary::after_row(checkpoint, fact);
                    self.consumed = true;
                    self.table = None;
                    return Ok((
                        work,
                        Some(ReplayOutcome::Window(Box::new(self.window(Some(fact))))),
                    ));
                }
                // 非目标行只替换固定metadata，真实前行/前驱边界仍可供末行窗口复用。
                self.last_completed = Some((fact, self.boundary.clone(), self.previous.clone()));
                self.previous = Some(std::mem::replace(
                    &mut self.boundary,
                    Boundary::after_row(checkpoint, fact),
                ));
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
                    self.anchors.chunk = before;
                    self.anchors.halo = halo;
                    self.window(Some(row))
                } else if end.last_body_row_ordinal().is_none() {
                    // H/D-only仍保留真实H事实，不把空body投影冒充虚构row1。
                    let mut window = self.window(None);
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

    fn window(&self, row: Option<TableRowFact>) -> MarkdownTableStructuralWindow {
        let mut body = Vec::with_capacity(usize::from(row.is_some()));
        if let Some(row) = row {
            // 只物化目标这一条真实 body；H/D 作为独立语法上下文，绝不补齐中间省略行。
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
            include_header: self.target == 0,
            next_required_row: None,
            table_end: None,
            replay: Some(self.anchors.clone()),
        }
    }
}
