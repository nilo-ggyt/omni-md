//! 一个有限窗口的真实源配对事务；结构前探临时使用原Global，完成前恢复原来源位置。

use super::inline_code::InlinePermission;
use super::*;
use crate::markdown_document_buffer::table_reference_request;
use crate::markdown_document_buffer::table_window_request::MarkdownTableRequestError as Error;
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use crate::markdown_viewport_presentation::table_window::context::{
    MarkdownTableContextFrame, MarkdownTableUnderlineContext,
};
use crate::markdown_viewport_presentation::table_window::references::MarkdownTableReferenceQueries;
use pulldown_cmark::block_frontier::SourceFrame;
use pulldown_cmark::global_frontier::{
    GlobalFact, GlobalFrontier, GlobalSourceAuthority, GlobalStep, LeafBlockKind,
    VerifiedGlobalReference,
};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MarkdownTableUnderlineRequestStep {
    /// 原配对与UTF16游标的实际读取；单独保留可验证每pair没有重扫长前缀。
    pub source_bytes: usize,
    pub utf8_validation_bytes: usize,
    pub source_tree_steps: usize,
    /// 同一个Global为代码权限实际重放的工作；父请求必须一并计入累计fuel。
    pub global_source_bytes: usize,
    pub global_source_tree_steps: usize,
    pub structural_transitions: usize,
    pub copied_source_bytes: usize,
    pub metadata_bytes: usize,
    pub normalizer_input_bytes: usize,
    pub parser_input_bytes: usize,
    pub allocated_source_capacity: usize,
    pub ready: bool,
}

#[derive(Debug)]
enum Phase {
    BeforeHtml,
    Retain,
    Rewind,
    ReferenceSource,
    Classify,
    Reference,
    Inline,
    Read {
        end: usize,
        literal: Option<Range<usize>>,
        real_eof: bool,
    },
    Restore,
    Publish,
    Ready,
}

#[derive(Clone, Copy, Debug)]
/// 构造入口决定原 scanner 的实际后继拥有者；普通窗口的 UTF16 发布完成就结束其扫描用途。
enum UnderlineContextUse {
    Window,
    StreamedSource,
}

#[derive(Debug)]
pub(crate) struct MarkdownTableUnderlineRequest {
    source: MarkdownTableSourceIdentity,
    quota: MarkdownTableWindowQuota,
    pub(super) context: Option<MarkdownTableUnderlineContext>,
    context_use: UnderlineContextUse,
    pairs: Vec<MarkdownTableUnderline>,
    phase: Phase,
    references: Option<Box<ReferenceResolution>>,
    inline: Option<Box<InlinePermission>>,
}

#[derive(Debug)]
/// 仅需要真实引用时分配；普通/巨段的原固定 owner 不嵌入整个引用解析器工作区。
struct ReferenceResolution {
    queries: Rc<MarkdownTableReferenceQueries>,
    fact: Option<VerifiedGlobalReference>,
    request: Option<table_reference_request::Request>,
}

impl MarkdownDocumentBuffer {
    /// 只保有限coverage、原Source弱身份与一份scanner，不保存源正文或历史代码范围。
    pub(crate) fn table_underline_request(
        &self,
        byte_coverage: &[Range<usize>],
        quota: MarkdownTableWindowQuota,
    ) -> Result<MarkdownTableUnderlineRequest, MarkdownTableWindowError> {
        let context =
            MarkdownTableUnderlineContext::new(self.generation(), self.byte_len(), byte_coverage)?;
        self.table_underline_from_context(context, quota, UnderlineContextUse::Window)
    }

    /// 单格来自同一个真实 scanner 的 sealed 来源；巨格只增加扫描回合，不分配其正文 String。
    pub(crate) fn table_cell_underline_request(
        &self,
        cell: &super::super::VerifiedMarkdownTableCellSource,
        quota: MarkdownTableWindowQuota,
    ) -> Result<MarkdownTableUnderlineRequest, super::super::MarkdownTableRequestError> {
        let seed = cell
            .inline_seed_for(self)?
            .expect("非空 CellInline 保同 scanner 的原 seed");
        self.table_source_underline_request(seed.physical_range(), quota)
            .map_err(super::super::MarkdownTableRequestError::Window)
    }

    /// 原 SourceRequest 串行交接一个当前范围；保原 scanner/UTF16 前沿，不保窗口历史或正文。
    pub(crate) fn table_source_underline_request(
        &self,
        byte_coverage: Range<usize>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<MarkdownTableUnderlineRequest, MarkdownTableWindowError> {
        let context = MarkdownTableUnderlineContext::for_streamed_source(
            self.generation(),
            self.byte_len(),
            byte_coverage,
        )?;
        self.table_underline_from_context(context, quota, UnderlineContextUse::StreamedSource)
    }

    /// 同一实际 scanner 的唯一 owner 图；上下文已经按对应业务入口取得真实范围。
    fn table_underline_from_context(
        &self,
        context: MarkdownTableUnderlineContext,
        quota: MarkdownTableWindowQuota,
        context_use: UnderlineContextUse,
    ) -> Result<MarkdownTableUnderlineRequest, MarkdownTableWindowError> {
        Ok(MarkdownTableUnderlineRequest {
            source: self
                .table_source_identity()
                .ok_or(MarkdownTableWindowError::InvalidSource)?,
            quota,
            context: Some(context),
            context_use,
            pairs: Vec::new(),
            phase: Phase::BeforeHtml,
            references: None,
            inline: None,
        })
    }
}

impl MarkdownTableUnderlineRequest {
    /// 原单格完成后沿同一 Source 接下一个格；这里只变更实际覆盖和 quota，不重置扫描位置或标签状态。
    /// 新覆盖向后时明确归还 false，调用方会释放这一旧 owner 并重新从原文首取得真实上下文。
    pub(crate) fn continue_cell(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cell: &super::super::VerifiedMarkdownTableCellSource,
        quota: MarkdownTableWindowQuota,
    ) -> Result<bool, Error> {
        document.original_backing_health().map_err(Error::Source)?;
        let seed = cell
            .inline_seed_for(document)?
            .expect("非空格持原 inline seed");
        self.continue_source(
            document,
            seed.physical_range(),
            cell.canonical_range_for(document)?.clone(),
            quota,
        )
    }

    /// 当前 RootLeaf 或格已真实认证这份范围；这里只串行推进原同源配对 owner。
    /// 反向/重叠范围不复用旧前沿，调用者须释放后走原冷扫描，不能把未知标签当作不存在。
    pub(crate) fn continue_source(
        &mut self,
        document: &MarkdownDocumentBuffer,
        bytes: Range<usize>,
        utf16: Range<usize>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<bool, Error> {
        document.original_backing_health().map_err(Error::Source)?;
        if !self.source.is_current(document)
            || !matches!(self.phase, Phase::Ready)
            || !matches!(self.context_use, UnderlineContextUse::StreamedSource)
        {
            return Ok(false);
        }
        let Some(context) = self.context.as_mut() else {
            return Ok(false);
        };
        if !context.continue_source(bytes, utf16, &mut self.pairs) {
            return Ok(false);
        }
        // 后继 Source 批次可有新的联合 quota；真实保留的 scanner/pair 容量仍占原有限工作区。
        quota.reserve_underline_workspace().map_err(Error::Window)?;
        self.quota = quota;
        self.references = None; // 上一格已决 label 不是新格查询；其有限字符串随原唯一 owner 归还。
        self.phase = if context.needs_classified_continuation() {
            Phase::Retain
        } else {
            Phase::BeforeHtml
        };
        Ok(true)
    }

    /// 闭合小块的有限 label 查询借同一个 Global 前探；已配对的 underline 不重建、不丢失。
    pub(crate) fn resolve_references(
        &mut self,
        queries: MarkdownTableReferenceQueries,
    ) -> Result<usize, Error> {
        if self.references.is_some() {
            return Err(Error::InvalidRequest);
        }
        let ready = queries.is_ready();
        let allocated = std::mem::size_of::<ReferenceResolution>()
            + std::mem::size_of::<MarkdownTableReferenceQueries>()
            + 2 * std::mem::size_of::<usize>(); // Rc的两个计数和唯一Map owner都计入本次真实分配。
        self.quota
            .admit_metadata(allocated)
            .map_err(Error::Window)?;
        self.references = Some(Box::new(ReferenceResolution {
            queries: Rc::new(queries),
            fact: None,
            request: None,
        }));
        if !ready {
            if !matches!(self.phase, Phase::Ready | Phase::BeforeHtml) {
                return Err(Error::InvalidRequest);
            }
            if matches!(self.phase, Phase::Ready) {
                self.phase = Phase::Retain;
            } // 无 '<' 快路先完成真实源扫描，之后再为引用保留/回退同一个 Global。
        }
        Ok(allocated)
    }

    /// 原边界已恢复之后才移交查询值；未解析引用不能因局部 Parser 找不到定义而变成 Missing。
    pub(crate) fn take_resolved_references(
        &mut self,
    ) -> Result<Option<MarkdownTableReferenceQueries>, Error> {
        if !matches!(self.phase, Phase::Ready) || self.references_pending() {
            return Err(Error::InvalidRequest);
        }
        // 原inline owner已在Restore阶段归还；这里只有同一查询库存的最后一个实际claim。
        Ok(self.references.take().map(|state| {
            Rc::try_unwrap(state.queries).expect("原code解析器必须先于引用库存移交结束")
        }))
    }

    /// 只有真实未解析 label 令前探继续；不存在引用事务与已闭合 Missing 都不能被当作 Pending。
    fn references_pending(&self) -> bool {
        self.references
            .as_ref()
            .is_some_and(|state| !state.queries.is_ready())
    }

    /// 每步最多一个真实源借用或一个Global控制操作；归还成功之后才改变可发布阶段。
    pub(crate) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        global: &mut GlobalFrontier,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        if !self.source.is_current(document) {
            return Err(Error::SourceSuperseded);
        }
        if !(2048..=MAX_TABLE_SOURCE_FRAME_BYTES).contains(&budget) {
            return Err(Error::InvalidRequest);
        }
        let mut work = MarkdownTableUnderlineRequestStep {
            structural_transitions: 64,
            ..Default::default()
        };
        let authority = GlobalSourceAuthority {
            current_revision: document.generation(),
            cancelled: false,
        };
        match &self.phase {
            Phase::Ready => {
                return Ok(MarkdownTableUnderlineRequestStep {
                    ready: true,
                    ..Default::default()
                });
            }
            Phase::Retain => {
                // 原来源必须已在真实闭合块或已提交表行；非法边界不丢Fact前行，也不以零进展忙等。
                let measured = global
                    .retain_semantic_boundary(authority, budget - 64)
                    .map_err(Error::Global)?
                    .ok_or(Error::Window(MarkdownTableWindowError::ContextPending))?;
                work.structural_transitions += measured.transitions;
                self.phase = Phase::Rewind;
            }
            Phase::Rewind => {
                // 同一个Global回到其真实Root入口，保留原预算/累计work及一次性恢复点。
                let measured = global
                    .rewind_semantic_source_to_start(authority, budget - 64)
                    .map_err(Error::Global)?;
                work.structural_transitions += measured.transitions;
                self.phase = if self.references_pending() {
                    Phase::ReferenceSource
                } else {
                    Phase::Classify
                };
            }
            Phase::Restore => {
                if self.inline.take().is_some() {
                    // 先在独立量子归还固定小块 Parser/正文与 code cursor，再恢复原 Global。
                    work.structural_transitions += 2048;
                    return Ok(work);
                }
                // 所有前探frame已在先前量子归还；恢复完成前配对结果始终不可读取。
                let measured = global
                    .restore_semantic_boundary(authority, budget - 64)
                    .map_err(Error::Global)?;
                work.structural_transitions += measured.transitions;
                self.phase = Phase::Publish;
            }
            Phase::Publish => {
                // scanner 同步计量的 UTF16 配对只移交一次，公开样式只读发布 Vec。
                if let Some(context) = self.context.as_mut() {
                    self.pairs = context
                        .take_ready_utf16_pairs()
                        .ok_or(Error::Window(MarkdownTableWindowError::ContextPending))?;
                } // 已发布配对只为引用重放时继续保留原有限 pairs。
                if matches!(self.context_use, UnderlineContextUse::Window) {
                    // 普通窗口后继只消费已发布配对或引用；它没有串行 Cell 前沿消费者。
                    // 立即归还 scanner 与仅测试构建存在的 byte 配对目录，保留原完成即释放合同。
                    self.context = None;
                }
                // 只有原串行 Source 构造入口保真实 scanner/UTF16 前沿，供同 SourceRequest
                // 下一窗口/格接管。UTF16 Vec 已移出，不保正文、第二个 Global 或全源标签索引。
                self.phase = Phase::Ready;
                work.ready = true;
            }
            Phase::Classify => {
                if self
                    .context
                    .as_ref()
                    .is_none_or(|context| context.is_ready())
                    && !self.references_pending()
                {
                    self.phase = Phase::Restore;
                    return Ok(work);
                }
                return self.advance_global(document, global, authority, budget);
            }
            Phase::ReferenceSource => {
                if !self.references_pending() {
                    // 定义先按原Global的真实顺序闭合，再重放同一前沿解释code/HTML优先级。
                    // 否则引用先于code事件时会等待引用，而引用又被这个code事件阻止向前解析。
                    self.phase = Phase::Rewind;
                    return Ok(work);
                }
                return self.advance_global(document, global, authority, budget);
            }
            Phase::Reference => return self.advance_reference(document, budget),
            Phase::Inline => return self.advance_inline(document, budget),
            Phase::BeforeHtml | Phase::Read { .. } => {
                return self.advance_context(document, budget);
            }
        }
        Ok(work)
    }

    /// 一步只驱动原Global并取得一个有界权限；未来Fact不进入主来源或历史目录。
    fn advance_global(
        &mut self,
        document: &MarkdownDocumentBuffer,
        global: &mut GlobalFrontier,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        let references_only = matches!(self.phase, Phase::ReferenceSource);
        let tree = document
            .root
            .as_ref()
            .map_or(0, |root| usize::from(root.height()));
        // Global内部byte额度至多转换额度的1/4；4/5分配同时覆盖读取、控制与AVL路径的联合上限。
        let child = budget.saturating_sub(tree + 64) * 4 / 5;
        if child < 2048 {
            return Err(Error::WorkLimit);
        }
        let frame = document
            .table_source_frame(global.next_byte(), child)
            .map_err(Error::Source)?;
        let step = global
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                authority,
                child,
            )
            .map_err(Error::Global);
        frame.finish().map_err(Error::Source)?; // 真实回收失败优先，不能恢复旧边界后伪作读取成功。
        let (measured, permission) = match step? {
            GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => (work, None),
            GlobalStep::Complete { work } => {
                if let Some(state) = self.references.as_mut()
                    && !state.queries.is_ready()
                {
                    Rc::get_mut(&mut state.queries)
                        .expect("解析期间尚未给inline借出引用库存")
                        .finish_verified_eof();
                }
                (work, Some((document.byte_len(), None, true)))
            }
            GlobalStep::Fact { fact, work } => {
                // Fact逐个使用后释放；前探不碰主结构的pending、表格math或普通来源游标。
                let (end, literal) = match fact {
                    GlobalFact::Table(header) => (header.separator_byte_end, None), // 完整H/D已由原Global提交。
                    GlobalFact::Row(row) => (row.byte_end, None), // 只包含真实成员行，不吞后继候选。
                    GlobalFact::TableEnd(end) => (end.byte_end(), None), // 失败候选留待后继块重新分类。
                    GlobalFact::Reference(reference) => {
                        let end = reference.candidate().span_end;
                        if self.references_pending() {
                            let state = self.references.as_mut().unwrap();
                            state.fact = Some(reference);
                            self.phase = Phase::Reference;
                        }
                        (end, None) // 不采用 title 前瞻的 backup 尾部；真实 Fact 只移交一次。
                    }
                    // 闭合InlineBlock不证明inline-code优先级，本次只解决真实块级代码的HTML抑制。
                    GlobalFact::InlineBlock(block) => {
                        let end = block.physical_range().end;
                        if !references_only
                            && self.context.as_ref().is_some_and(|context| {
                                !context.is_ready() && context.next_byte() < end
                            })
                        {
                            self.inline = Some(Box::new(InlinePermission::new(block)?));
                            self.phase = Phase::Inline;
                        }
                        (end, None) // InlineBlock 只授予来源；code/HTML 优先级仍由该事实下的真实 cursor 证明。
                    }
                    GlobalFact::LeafBlock(block) => {
                        let end = block.consumed_end();
                        if !references_only
                            && matches!(block.kind(), LeafBlockKind::AtxHeading(_))
                            && self.context.as_ref().is_some_and(|context| {
                                !context.is_ready() && context.next_byte() < end
                            })
                        {
                            self.inline = Some(Box::new(InlinePermission::from_atx(block)?));
                            self.phase = Phase::Inline;
                            return Ok(MarkdownTableUnderlineRequestStep {
                                global_source_bytes: work.byte_reads,
                                global_source_tree_steps: tree,
                                structural_transitions: work.transitions + tree + 64,
                                ..Default::default()
                            }); // 真 Leaf 下逐个 Code 事件先交唯一 scanner，不能让标题代码中的假 HTML 改外部配对。
                        }
                        let literal = matches!(
                            block.kind(),
                            LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode
                        )
                        .then(|| block.physical_start()..end);
                        (end, literal)
                    }
                };
                (work, Some((end, literal, false)))
            }
        };
        if let Some((end, literal, real_eof)) = permission
            && !matches!(self.phase, Phase::Reference | Phase::Inline)
        {
            if references_only {
                // 首遍只消费定义事实；唯一underline scanner的UTF16游标与未决标签保持原位。
                self.phase = Phase::ReferenceSource;
            } else if self
                .context
                .as_ref()
                .is_none_or(|context| context.is_ready())
            {
                self.phase = Phase::Classify;
            } else {
                self.phase = Phase::Read {
                    end,
                    literal,
                    real_eof,
                };
            }
        }
        Ok(MarkdownTableUnderlineRequestStep {
            global_source_bytes: measured.byte_reads,
            global_source_tree_steps: tree,
            structural_transitions: measured.transitions + tree + 64,
            ..Default::default()
        })
    }

    /// 同一 code 权限 owner 逐条交接 literal；前后普通内容始终沿唯一 underline scanner 顺序消费。
    fn advance_inline(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        if self.inline.as_ref().unwrap().reference_input()?.is_some() {
            if self.references.is_none() {
                // 普通目标没有自己的引用投影；只在此真实含tick的小块需要Parser时建立唯一库存。
                let mut queries =
                    MarkdownTableReferenceQueries::for_batch(Some(self.quota.clone()));
                queries.seal_batch();
                let allocated = self.resolve_references(queries)?;
                return Ok(MarkdownTableUnderlineRequestStep {
                    metadata_bytes: allocated,
                    structural_transitions: 64,
                    ..Default::default()
                }); // owner分配与后继有限Parser收集分开，保持原单量子工作上限。
            }
            let inline = self.inline.as_mut().unwrap();
            let (source, options) = inline.reference_input()?.unwrap();
            let length = source.len();
            let state = self.references.as_mut().unwrap();
            Rc::get_mut(&mut state.queries)
                .expect("当前小块尚未向Parser借出原引用库存")
                .collect_for_classification(source, options)
                .map_err(Error::Window)?;
            let mut work = MarkdownTableUnderlineRequestStep {
                parser_input_bytes: length,
                structural_transitions: 64,
                ..Default::default()
            };
            if state.queries.is_ready() {
                // 旧Resolved/Missing仍有效；无新增未知时，下一量子才创建实际code Parser。
                inline.accept_references(Rc::clone(&state.queries));
            } else {
                // 保留scanner当前位置和原主恢复票据，只归还当前有限scratch；
                // 同一Global从Root按源序解析新增查询，重到此块后才允许解释code/HTML。
                self.inline = None;
                self.phase = Phase::Rewind;
                work.structural_transitions += 2048;
            }
            return Ok(work);
        }
        let inline = self.inline.as_mut().unwrap();
        let step = inline.advance(document, budget)?;
        if let Some(literal) = step.literal {
            self.phase = Phase::Read {
                end: literal.end,
                literal: Some(literal),
                real_eof: false,
            };
        } else if step.done {
            self.phase = Phase::Read {
                end: inline.end(),
                literal: None,
                real_eof: false,
            };
        }
        Ok(MarkdownTableUnderlineRequestStep {
            global_source_bytes: step.work.source_bytes,
            global_source_tree_steps: step.work.source_tree_steps,
            copied_source_bytes: step.work.copied_source_bytes,
            metadata_bytes: step.work.metadata_bytes,
            parser_input_bytes: step.work.parser_input_bytes,
            allocated_source_capacity: step.work.allocated_source_capacity,
            structural_transitions: step.work.structural_transitions,
            ..Default::default()
        })
    }

    /// 一个实际定义沿既有 scope/normalizer/Parser 事务解析，失败保原首因，绝不采用后继同名值。
    fn advance_reference(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        let state = self.references.as_mut().ok_or(Error::InvalidRequest)?;
        if let Some(fact) = state.fact.take() {
            state.request =
                table_reference_request::Request::new(self.source.clone(), fact, &state.queries)
                    .map_err(Error::Window)?;
            return Ok(MarkdownTableUnderlineRequestStep {
                structural_transitions: 64,
                ..Default::default()
            });
        }
        if let Some(request) = state.request.as_mut() {
            let step = request
                .advance(
                    document,
                    Rc::get_mut(&mut state.queries)
                        .expect("引用首遍与inline解释不并行持有可变库存"),
                    budget,
                )
                .map_err(Error::Window)?;
            if step.complete {
                state.request = None;
            }
            return Ok(MarkdownTableUnderlineRequestStep {
                global_source_bytes: step.work.source_bytes,
                global_source_tree_steps: step.work.source_tree_steps,
                copied_source_bytes: step.work.copied_bytes,
                metadata_bytes: step.work.metadata_bytes,
                structural_transitions: step.work.structural_transitions,
                normalizer_input_bytes: step.work.normalizer_input_bytes,
                parser_input_bytes: step.work.parser_input_bytes,
                ..Default::default()
            });
        }
        // 定义事务只在首遍运行；不把定义结束位置授予尚未走code优先级的scanner。
        self.phase = Phase::ReferenceSource;
        Ok(MarkdownTableUnderlineRequestStep {
            structural_transitions: 64,
            ..Default::default()
        })
    }

    /// 已证明的字节才解释标签；无HTML快路在首个候选前停住并保留原UTF16进度。
    fn advance_context(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableUnderlineRequestStep, Error> {
        let context = self.context.as_mut().unwrap();
        if context.is_ready() {
            // 空coverage同样只移交一次；非空扫描的完成阶段已决定是否先恢复Global。
            self.phase = if self.references_pending() {
                if matches!(self.phase, Phase::BeforeHtml) {
                    Phase::Retain
                } else {
                    Phase::Classify
                }
            } else {
                Phase::Publish
            };
            return Ok(MarkdownTableUnderlineRequestStep {
                structural_transitions: 64,
                ..Default::default()
            });
        }
        let next = context.next_byte();
        let tree = document
            .root
            .as_ref()
            .map_or(0, |root| usize::from(root.height()));
        let maximum = (budget.saturating_sub(tree + 64) / 8).clamp(1, 4096);
        let (end, literal, proven_eof) = match &self.phase {
            Phase::BeforeHtml => (document.byte_len(), false, false),
            Phase::Read {
                end,
                literal,
                real_eof,
            } => {
                if next >= *end && !real_eof {
                    // 前缀已由无token快路计量，或相邻事实没有新增字节；仍交回原Global继续证明。
                    self.phase = if self.inline.as_ref().is_some_and(|inline| !inline.done()) {
                        Phase::Inline
                    } else {
                        self.inline = None;
                        Phase::Classify
                    };
                    return Ok(MarkdownTableUnderlineRequestStep {
                        structural_transitions: 64,
                        ..Default::default()
                    });
                }
                let is_literal = literal.as_ref().is_some_and(|range| range.contains(&next));
                let end = literal
                    .as_ref()
                    .filter(|range| next < range.start)
                    .map_or(*end, |range| range.start);
                (end, is_literal, *real_eof)
            }
            _ => unreachable!("只有源读取阶段借用正文"),
        };
        let frame = document
            .table_source_frame(next, maximum.min(end.saturating_sub(next)).max(1))
            .map_err(Error::Source)?;
        let count = frame.bytes.len().min(end.saturating_sub(next));
        let count = if matches!(self.phase, Phase::BeforeHtml) {
            frame.bytes[..count]
                .iter()
                .position(|byte| *byte == b'<')
                .unwrap_or(count)
        } else {
            count
        };
        let html = matches!(self.phase, Phase::BeforeHtml) && frame.bytes.get(count) == Some(&b'<');
        let real_eof = frame.real_eof
            && count == frame.bytes.len()
            && (matches!(self.phase, Phase::BeforeHtml) || proven_eof);
        let input = MarkdownTableContextFrame {
            generation: frame.generation,
            byte_start: frame.byte_start,
            bytes: &frame.bytes[..count],
            real_eof,
        };
        let step = if literal {
            context.advance_literal(input, maximum)
        } else {
            context.advance(input, maximum)
        };
        frame.finish().map_err(Error::Source)?;
        let step = step.map_err(Error::Window)?;
        if step.ready {
            self.phase = if self.references_pending() {
                if matches!(self.phase, Phase::BeforeHtml) {
                    Phase::Retain
                } else {
                    Phase::Classify
                }
            } else if matches!(self.phase, Phase::BeforeHtml) {
                Phase::Publish
            } else {
                Phase::Restore
            };
        } else if html {
            // 首个可能的HTML字节尚未进入scanner，原UTF16位置保留，再取得真实结构权限。
            self.phase = Phase::Retain;
        }
        Ok(MarkdownTableUnderlineRequestStep {
            source_bytes: step.consumed,
            utf8_validation_bytes: step.utf8_validation_bytes,
            source_tree_steps: tree,
            structural_transitions: step.consumed + step.utf8_validation_bytes + tree + 64,
            ..Default::default()
        })
    }

    /// 只有完成源回收和原Global恢复后才公开结果，旧代次不能复用已发布配对。
    pub(crate) fn ready_pairs_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&[MarkdownTableUnderline]>, MarkdownTableWindowError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok(matches!(self.phase, Phase::Ready).then_some(self.pairs.as_slice()))
    }
}
