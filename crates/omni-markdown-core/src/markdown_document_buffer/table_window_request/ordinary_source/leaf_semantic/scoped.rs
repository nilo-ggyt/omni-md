//! 原 Leaf capture 与逐行 Origin 的有限消费；只保存当前视口的前缀样式，不保存代码正文。

use super::super::semantic::{PrefixDecoration, SpanProof};
use super::*;
use pulldown_cmark::global_frontier::{
    BlockOriginCursor, BlockOriginLimits, BlockOriginStep, CapturedScopeCursor, CapturedScopeRole,
    CapturedScopeStep,
};

/// 同一 Leaf 的两类真实只读证明按次序消费；源寿命仍完全属于父 SourceRequest。
/// scope 完成即归还目录，Origin 只前进到本窗口末行；没有全文行表、字符串或第二 Global。
pub(in super::super) struct ScopedLeaf {
    scope: Option<CapturedScopeCursor>,
    origins: Option<BlockOriginCursor>,
    prefix: Option<PrefixDecoration>,
    pending: Option<SpanProof>,
    coordinates: Option<Coordinates>,
    coverage: Range<usize>,
    selection_bytes: Range<usize>,
    containers: ContainerPositions,
    prefixes: Vec<MarkdownPresentationRun>,
    // 只保存实际祖先 opening 的字面区间；它们不带正文，不参与容器样式继承。
    // 短 Paragraph 路径直接借原 context 中同一事实，不再复制这组 metadata。
    footnote_openings: Vec<Range<usize>>,
    line_reaches_end: bool,
    unsupported: bool,
    done: bool,
    inline_index: usize,
}

/// 每种视觉继承只保最早真实祖先 anchor；坐标转换前后互斥，不复制一张角色或来源目录。
#[derive(Clone, Copy)]
enum ContainerPositions {
    Bytes([Option<usize>; 3]),
    Utf16([Option<usize>; 3]),
}

const CONTAINER_STYLES: [MarkdownPresentationStyles; 3] = [
    MarkdownPresentationStyles::BLOCK_QUOTE,
    MarkdownPresentationStyles::UNORDERED_LIST,
    MarkdownPresentationStyles::ORDERED_LIST,
];

impl ScopedLeaf {
    pub(super) fn new(
        leaf: &VerifiedGlobalLeafBlock,
        coverage: Range<usize>,
        selection_bytes: Range<usize>,
        limits: (usize, usize),
        quota: &MarkdownTableWindowQuota,
    ) -> Result<Box<Self>, MarkdownTableRequestError> {
        quota
            .admit_metadata(std::mem::size_of::<Self>())
            .map_err(MarkdownTableRequestError::Window)?;
        let scope = leaf
            .scope_cursor()
            .map_err(MarkdownTableRequestError::Global)?;
        let origins = leaf
            .line_origins(BlockOriginLimits {
                max_byte_work: limits.0,
                max_transitions: limits.1,
            })
            .map_err(MarkdownTableRequestError::Global)?;
        Ok(Box::new(Self {
            scope: Some(scope),
            origins: Some(origins),
            prefix: None,
            pending: None,
            coordinates: None,
            coverage,
            selection_bytes,
            containers: ContainerPositions::Bytes([None; 3]),
            prefixes: Vec::new(),
            footnote_openings: Vec::new(),
            line_reaches_end: false,
            unsupported: false,
            done: false,
            inline_index: 0,
        }))
    }

    pub(super) fn unsupported(&self) -> bool {
        self.unsupported
    }

    /// 短 scoped 段落复用同一前缀样式消费者；scope 和行状态只借原 context 的已完成事实。
    /// 原 context 由 BoundedInline 独占持有，本对象不克隆其 seed、spine 或行目录。
    pub(in super::super) fn for_inline(
        context: &pulldown_cmark::global_frontier::InlineParagraphContext,
        coverage: Range<usize>,
        selection_bytes: Range<usize>,
        quota: &MarkdownTableWindowQuota,
    ) -> Result<Box<Self>, MarkdownTableRequestError> {
        let starts = context
            .container_starts()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        quota
            .admit_metadata(std::mem::size_of::<Self>())
            .map_err(MarkdownTableRequestError::Window)?;
        Ok(Box::new(Self {
            scope: None,
            origins: None,
            prefix: None,
            pending: None,
            coordinates: None,
            coverage,
            selection_bytes,
            containers: ContainerPositions::Bytes(starts),
            prefixes: Vec::new(),
            footnote_openings: Vec::new(),
            line_reaches_end: false,
            unsupported: false,
            done: false,
            inline_index: 0,
        }))
    }

    pub(in super::super) fn done(&self) -> bool {
        self.done
    }

    pub(in super::super) fn advance_inline(
        &mut self,
        document: &MarkdownDocumentBuffer,
        context: &pulldown_cmark::global_frontier::InlineParagraphContext,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<(SemanticWork, bool), MarkdownTableRequestError> {
        self.advance_with_context(document, quota, budget, Some(context))
    }

    /// 每量子只推进一个原角色、Origin 或前缀片；所有实际分配与读取交回父请求累计计量。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<(SemanticWork, bool), MarkdownTableRequestError> {
        self.advance_with_context(document, quota, budget, None)
    }

    fn advance_with_context(
        &mut self,
        document: &MarkdownDocumentBuffer,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
        context: Option<&pulldown_cmark::global_frontier::InlineParagraphContext>,
    ) -> Result<(SemanticWork, bool), MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..Default::default()
        };
        if self.done {
            return Ok((work, true));
        }
        if let Some(scope) = self.scope.as_mut() {
            let before = scope.allocated_workspace_bytes();
            let step = scope
                .advance(
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    budget - CONTROL - std::mem::size_of::<Range<usize>>(),
                )
                .map_err(MarkdownTableRequestError::Global)?;
            let allocated = scope.allocated_workspace_bytes() - before;
            quota
                .admit_metadata(allocated)
                .map_err(MarkdownTableRequestError::Window)?;
            let measured = match step {
                CapturedScopeStep::Progress { work }
                | CapturedScopeStep::Entry { work, .. }
                | CapturedScopeStep::Complete { work } => work,
            };
            work.structural_transitions += measured.transitions;
            work.metadata_bytes = allocated;
            match step {
                CapturedScopeStep::Entry {
                    role,
                    source_byte_start,
                    ..
                } => {
                    let index = match role {
                        CapturedScopeRole::Quote => Some(0),
                        CapturedScopeRole::List { marker, .. } => match marker {
                            b'-' | b'+' | b'*' => Some(1),
                            b'.' | b')' => Some(2),
                            _ => return Err(MarkdownTableRequestError::InvalidRequest),
                        },
                        CapturedScopeRole::Item { .. } => None,
                        CapturedScopeRole::Footnote {
                            body_start: Some(end),
                        } => {
                            // producer 已证明 label、colon 和其后空白；数字/嵌套标点仍保持字面。
                            // 保实际完整端点，让 prefix 游标能越过可见区之前的同一个 opening。
                            let bytes = std::mem::size_of::<Range<usize>>();
                            quota
                                .admit_metadata(bytes)
                                .map_err(MarkdownTableRequestError::Window)?;
                            self.footnote_openings.reserve_exact(1);
                            self.footnote_openings.push(source_byte_start..end);
                            work.metadata_bytes += bytes;
                            work.structural_transitions += bytes;
                            None
                        }
                        _ => {
                            self.unsupported = true;
                            None
                        }
                    };
                    if let Some(index) = index {
                        let ContainerPositions::Bytes(starts) = &mut self.containers else {
                            unreachable!("原 scope 尚未完成坐标转换");
                        };
                        starts[index] = Some(starts[index].map_or(source_byte_start, |previous| {
                            previous.min(source_byte_start)
                        }));
                    } // 子 List 不能污染其 opening 之前的外层 Quote prefix，真实 source anchor 必须保留。
                }
                CapturedScopeStep::Complete { .. } => {
                    self.scope = None;
                    if self.unsupported {
                        self.origins = None;
                        self.done = true;
                    } // 仍缺 opening 的角色或未接定义继续未知，不能当作普通列表。
                }
                _ => {}
            }
            return Ok((work, self.done));
        }
        if let ContainerPositions::Bytes(starts) = self.containers {
            let mut at = [starts[0], starts[1], starts[2], None].map(|start| {
                start
                    .unwrap_or(self.coverage.end)
                    .clamp(self.coverage.start, self.coverage.end)
            });
            at.sort_unstable(); // 原四边界游标只沿源前进；固定排序不重排容器或制造新的 source anchor。
            if let Some(coordinates) = self.coordinates.as_mut() {
                let (measured, values) = coordinates.advance(document, budget - CONTROL)?;
                work = measured;
                work.structural_transitions += CONTROL;
                if let Some(values) = values {
                    self.containers = ContainerPositions::Utf16(std::array::from_fn(|index| {
                        starts[index].map(|start| {
                            let byte = start.clamp(self.coverage.start, self.coverage.end);
                            values[at.binary_search(&byte).unwrap()]
                        })
                    }));
                    self.coordinates = None;
                }
            } else {
                let (coordinates, measured) = Coordinates::new(document, at, budget - CONTROL)?;
                self.coordinates = Some(coordinates);
                work = measured;
                work.structural_transitions += CONTROL;
            }
            return Ok((work, false));
        } // 只转换固定三个真实起点；不按祖先层数分配，也不为每条 prefix 重扫这些端点。
        if let Some(proof) = self.pending.as_ref() {
            if let Some(coordinates) = self.coordinates.as_mut() {
                let (measured, values) = coordinates.advance(document, budget - CONTROL)?;
                work = measured;
                work.structural_transitions += CONTROL;
                if let Some(values) = values {
                    quota
                        .admit_metadata(std::mem::size_of::<MarkdownPresentationRun>())
                        .map_err(MarkdownTableRequestError::Window)?;
                    self.prefixes.reserve_exact(1);
                    self.prefixes.push(MarkdownPresentationRun {
                        utf16_range: values[0]..values[3],
                        styles: proof.styles,
                    });
                    work.metadata_bytes += std::mem::size_of::<MarkdownPresentationRun>();
                    self.pending = None;
                    self.coordinates = None;
                }
                return Ok((work, false));
            }
            // 只转换可见交集；离屏 opener、整条代码行或中间正文不会被复制进坐标任务。
            let start = proof.whole.start.max(self.coverage.start);
            let end = proof.whole.end.min(self.coverage.end);
            let (coordinates, measured) =
                Coordinates::new(document, [start, start, end, end], budget - CONTROL)?;
            self.coordinates = Some(coordinates);
            work = measured;
            work.structural_transitions += CONTROL;
            return Ok((work, false));
        }
        if let Some(prefix) = self.prefix.as_mut() {
            let openings = match context {
                Some(context) => context
                    .footnote_openings()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                None => &self.footnote_openings,
            };
            let (measured, proof, done) = prefix.advance(document, budget - CONTROL, openings)?;
            work = measured;
            work.structural_transitions += CONTROL;
            self.pending = proof.filter(|proof| {
                proof.whole.start < self.coverage.end && self.coverage.start < proof.whole.end
            });
            if done {
                self.prefix = None;
            }
            return Ok((work, false));
        }
        if self.line_reaches_end {
            let had_origin_cursor = self.origins.take().is_some();
            self.done = true;
            work.structural_transitions += usize::from(had_origin_cursor) * 1024;
            // 仅原 Leaf cursor 真的持目录时收归还费；短段落 context 的释放归其唯一 BoundedInline owner。
            return Ok((work, true));
        }
        if let Some(context) = context {
            let Some(line) = context.line_at(self.inline_index) else {
                self.done = true;
                return Ok((work, true));
            };
            self.inline_index += 1;
            self.line_reaches_end = line.physical_range().end >= self.coverage.end;
            let prefix = line.prefix_range();
            if prefix.start < self.coverage.end && self.coverage.start < prefix.end {
                self.prefix = Some(PrefixDecoration::for_paragraph(
                    line.clone(),
                    &self.selection_bytes,
                ));
            }
            work.structural_transitions += std::mem::size_of_val(line);
            return Ok((work, false));
        } // 原 Leaf 继续驱动原 Origin cursor；短段落只借其唯一 context 的已证行。
        let cursor = self.origins.as_mut().unwrap();
        let tree = tree_height(document);
        let child = budget - CONTROL - tree;
        let frame = document
            .table_source_frame(cursor.next_byte(), child)
            .map_err(MarkdownTableRequestError::Source)?;
        let before = cursor.allocated_workspace_bytes();
        let step = cursor.advance(
            SourceFrame {
                source_revision: frame.generation,
                byte_start: frame.byte_start,
                bytes: &frame.bytes,
                real_eof: frame.real_eof,
            },
            GlobalSourceAuthority {
                current_revision: document.generation(),
                cancelled: false,
            },
            child,
        );
        let allocated = cursor.allocated_workspace_bytes() - before;
        frame.finish().map_err(MarkdownTableRequestError::Source)?;
        let step = step.map_err(MarkdownTableRequestError::Global)?;
        quota
            .admit_metadata(allocated)
            .map_err(MarkdownTableRequestError::Window)?;
        let measured = match &step {
            BlockOriginStep::Progress { work, .. }
            | BlockOriginStep::NeedInput { work, .. }
            | BlockOriginStep::Line { work, .. }
            | BlockOriginStep::Complete { work } => *work,
        };
        work.source_bytes = measured.byte_reads;
        work.source_tree_steps = tree;
        work.structural_transitions += measured.transitions + tree;
        work.metadata_bytes = allocated;
        match step {
            BlockOriginStep::Line { line, .. } => {
                self.line_reaches_end =
                    line.physical_range().end >= self.coverage.end || line.is_terminal();
                let prefix = line.prefix_range();
                if prefix.start < self.coverage.end && self.coverage.start < prefix.end {
                    self.prefix = Some(PrefixDecoration::for_leaf(line, &self.selection_bytes));
                }
            }
            BlockOriginStep::Complete { .. } => {
                self.origins = None;
                self.done = true;
            }
            _ => {}
        }
        Ok((work, self.done))
    }

    /// 原前缀段有序且互不重叠；每次只给当前 run 的下一真实切点，继承样式不会抹去代码/标题。
    pub(in super::super) fn query(&self, index: usize, end: usize) -> MarkdownPresentationQuery {
        let ContainerPositions::Utf16(starts) = self.containers else {
            unreachable!("只有原 scope 与 prefix 完成后的 query 才能发布");
        };
        let mut styles = MarkdownPresentationStyles::default();
        let mut end = end;
        for (start, style) in starts.into_iter().zip(CONTAINER_STYLES) {
            if let Some(start) = start {
                if start <= index {
                    styles = styles.union(style);
                } else {
                    end = end.min(start);
                }
            }
        }
        let at = self
            .prefixes
            .partition_point(|run| run.utf16_range.end <= index);
        match self.prefixes.get(at) {
            Some(run) if run.utf16_range.contains(&index) => MarkdownPresentationQuery {
                utf16_range: index..end.min(run.utf16_range.end),
                styles: styles.union(run.styles),
            },
            next => MarkdownPresentationQuery {
                utf16_range: index..next.map_or(end, |run| end.min(run.utf16_range.start)),
                styles,
            },
        }
    }
}
