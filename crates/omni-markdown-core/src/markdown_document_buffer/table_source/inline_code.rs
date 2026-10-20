//! 同一 Global 闭合 InlineBlock 的代码权限；每次只保当前事实、一个游标和一个 span。

use super::inline_reference::ReferenceLabel;
use super::inline_stack::StackFile;
use super::*;
use crate::markdown_document_buffer::table_window_request::{
    MarkdownTableRequestError as Error, SemanticWork,
};
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use crate::markdown_viewport_presentation::table_window::references::MarkdownTableReferenceQueries;
use pulldown_cmark::block_frontier::SourceFrame;
use pulldown_cmark::global_frontier::{
    GlobalSourceAuthority, GlobalWork, InlineCodeCursor, InlineCodeStep, InlineLinkStorageRequest,
    InlineLinkStorageStep, InlinePlainLimits, InlineReferenceResolution, LeafBlockKind,
    VerifiedGlobalInlineBlock, VerifiedGlobalLeafBlock,
};
use pulldown_cmark::{BrokenLink, CowStr, Event, OffsetIter, Options, Parser};
use std::{cell::Cell, ops::Range, rc::Rc};

type Callback = Box<dyn FnMut(BrokenLink<'static>) -> Option<(CowStr<'static>, CowStr<'static>)>>;

/// Parser 永远先于其固定输入销毁；私有类型不暴露文本替换或跨 owner 借用。
struct SmallParser {
    events: OffsetIter<'static, Callback>,
    _text: Box<str>,
    _references: Rc<MarkdownTableReferenceQueries>,
    failure: Rc<Cell<Option<MarkdownTableWindowError>>>,
}
impl SmallParser {
    /// 原 Parser 需要连续借用；固定<=2048字节输入拥有稳定独占分配，移动 owner 不移动这块堆内存。
    fn new(
        bytes: Vec<u8>,
        options: Options,
        references: Rc<MarkdownTableReferenceQueries>,
    ) -> Result<Self, Error> {
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::InvalidRequest)?
            .into_boxed_str();
        // SAFETY: text 的唯一 Box 随本私有 owner 保存，之后既不改写也不取出；events 字段先 Drop。
        // 该借用从不外带，源代次/真实回收仍由父事务逐步核验；错误或取消会整体 Drop 本 owner。
        let source: &'static str = unsafe { &*(text.as_ref() as *const str) };
        let failure = Rc::new(Cell::new(None));
        let observed = Rc::clone(&failure);
        let callback_references = Rc::clone(&references);
        let callback: Callback = Box::new(move |link| {
            // 同一Global已解析的唯一库存只读共享；没有查询权限时保首错，不猜缺失或复制整个Map。
            let result = callback_references.resolve_for_classification(&link.reference);
            match result {
                Ok(value) => value.map(|(destination, title)| {
                    // SAFETY: sealed Queries的entries没有内部可变；Parser活期有原Rc持有同一Map，
                    // 因而原Arc<String>/title分配既不替换也不reopen。events（含callback）字段
                    // 先于_text/_references销毁，任何Event/Cow只在本私有owner的方法栈消费，
                    // 不外带或存入更长寿命对象。与上方稳定Box正文的私有自借用责任相同。
                    let destination: &'static str = unsafe { &*(destination as *const str) };
                    // SAFETY: 同上；title也由同一不可变Queries及原Rc保活，无独立临时String。
                    let title: &'static str = unsafe { &*(title as *const str) };
                    (CowStr::from(destination), CowStr::from(title))
                }),
                Err(error) => {
                    if observed.get().is_none() {
                        observed.set(Some(error));
                    }
                    None
                }
            }
        });
        let events = Parser::new_with_broken_link_callback(source, options, Some(callback))
            .into_offset_iter();
        Ok(Self {
            events,
            _text: text,
            _references: references,
            failure,
        })
    }
}

enum Phase {
    VerifyRoot,
    Inspect,
    Copy,
    Queries,
    Parse,
    Events,
    Cursor,
    Done,
}

pub(super) struct InlinePermission {
    range: Range<usize>,
    next: usize,
    options: Options,
    root: bool,
    phase: Phase,
    bytes: Vec<u8>,
    parser: Option<SmallParser>,
    // 原Fact只在真实cursor构造前暂存，下一步消费后即归还；不是第二seed/解析缓存。
    block: Option<VerifiedGlobalInlineBlock>,
    cursor: Option<Box<InlineCodeCursor>>,
    pending_storage: Option<InlineLinkStorageRequest>,
    reference: Option<Box<ReferenceLabel>>,
    references: Option<Rc<MarkdownTableReferenceQueries>>,
}

pub(super) enum ReferenceInput<'a> {
    Bounded { source: &'a str, options: Options },
    Streamed(&'a str),
}

pub(super) struct PermissionStep {
    pub work: SemanticWork,
    pub literal: Option<Range<usize>>,
    pub done: bool,
}

/// AST内部开销仍作为固定小块Parser输入单独报告；这里只预留本owner与原2KiB源容量。
/// 可选大cursor在真实构造的独立量子另行准入/报分配，不能把同一载荷重复算在固定预留中。
pub(super) fn workspace_bytes() -> usize {
    2048 + std::mem::size_of::<InlinePermission>()
}

impl std::fmt::Debug for InlinePermission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InlinePermission")
            .field("range", &self.range)
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl InlinePermission {
    /// 真 ATX Leaf 先检查无 tick 快路；只有真实 Root 小标题才借有限原 Parser 证明 code，其他范围不猜语法。
    pub(super) fn from_atx(block: VerifiedGlobalLeafBlock) -> Result<Self, Error> {
        if !matches!(block.kind(), LeafBlockKind::AtxHeading(_)) {
            return Err(Error::InvalidRequest);
        }
        let range = block.physical_start()..block.source_range().end;
        Ok(Self {
            next: range.start,
            root: block.scope_depth() == 0,
            range,
            options: block.options(),
            phase: Phase::Inspect,
            bytes: Vec::new(),
            parser: None,
            block: None,
            cursor: None,
            pending_storage: None,
            reference: None,
            references: None,
        })
    }

    /// 实际 Fact 决定 Root/scope/options/范围；不接受调用方布尔、裸 Range 或 lookahead 作为权限。
    pub(super) fn new(block: VerifiedGlobalInlineBlock) -> Result<Self, Error> {
        let range = block.physical_range();
        let root = block.scope_depth() == 1;
        let options = block.options();
        Ok(Self {
            next: range.start,
            range,
            options,
            root,
            phase: Phase::Inspect,
            bytes: Vec::new(),
            parser: None,
            block: root.then_some(block),
            cursor: None,
            pending_storage: None,
            reference: None,
            references: None,
        })
    }

    pub(super) fn end(&self) -> usize {
        self.range.end
    }
    pub(super) fn done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    /// 只有实际含tick且完整复制的有限Root块才收引用；Parser尚未产生任何code/HTML效果。
    pub(super) fn reference_input(&self) -> Result<Option<ReferenceInput<'_>>, Error> {
        if let Some(reference) = self.reference.as_ref() {
            return Ok(reference.label().map(ReferenceInput::Streamed));
        }
        if !matches!(self.phase, Phase::Queries) {
            return Ok(None);
        }
        let source = std::str::from_utf8(&self.bytes).map_err(|_| Error::InvalidRequest)?;
        Ok(Some(ReferenceInput::Bounded {
            source,
            options: self.options,
        }))
    }

    /// 原引用请求仍停在同一个cursor；只有唯一Map真实结果才能恢复它，绝不以局部未见当Missing。
    pub(super) fn accept_reference(
        &mut self,
        resolution: InlineReferenceResolution,
        authority: GlobalSourceAuthority,
    ) -> Result<GlobalWork, Error> {
        let request = &self
            .reference
            .as_ref()
            .ok_or(Error::InvalidRequest)?
            .request;
        let work = self
            .cursor
            .as_mut()
            .ok_or(Error::InvalidRequest)?
            .accept_reference(request, resolution, authority)
            .map_err(Error::Global)?;
        self.reference = None;
        Ok(work)
    }

    /// 唯一库存完成当前块实际查询后才借给Parser；这里不复制Map或创建新的定义解析器。
    pub(super) fn accept_references(&mut self, references: Rc<MarkdownTableReferenceQueries>) {
        self.references = Some(references);
        self.phase = Phase::Parse;
    }

    /// 原 scanner 只有收到一个最终 code span 后才跳过其 literal；所有 frame 先真实 finish。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        stack: &mut StackFile,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<PermissionStep, Error> {
        document.original_backing_health().map_err(Error::Source)?;
        if self.block.is_some() {
            let metadata = std::mem::size_of::<InlineCodeCursor>();
            if metadata > budget - 64 {
                return Err(Error::WorkLimit);
            }
            // 初始小owner与原大cursor分别真实分配、分别交回；没有把同一次大Box摊账。
            // 原Fact在into_code_cursor中消费，成功只留下这一份独占cursor，不保存历史事实。
            let block = self.block.take().unwrap();
            let end = if self.range.len() > 2048 {
                self.range.end
            } else {
                block.source_range().start
            };
            let cursor = block
                .into_code_cursor(
                    end,
                    InlinePlainLimits {
                        max_byte_work: usize::MAX,
                        max_transitions: usize::MAX,
                    },
                )
                .map_err(Error::Global)?;
            let metadata = if let Some(cursor) = cursor {
                quota.admit_metadata(metadata).map_err(Error::Window)?;
                self.cursor = Some(Box::new(cursor));
                self.phase = if self.range.len() > 2048 {
                    Phase::Cursor
                } else {
                    Phase::VerifyRoot
                };
                metadata
            } else {
                0
            };
            return Ok(PermissionStep {
                work: SemanticWork {
                    metadata_bytes: metadata,
                    structural_transitions: 64,
                    ..Default::default()
                },
                literal: None,
                done: false,
            });
        }
        if let Some(request) = self.pending_storage.as_ref() {
            let result = stack.storage(request, quota, budget - 64);
            document.original_backing_health().map_err(Error::Source)?;
            let (step, metadata) = result?;
            let completed = matches!(&step, InlineLinkStorageStep::Ready { .. });
            let work = self
                .cursor
                .as_mut()
                .ok_or(Error::InvalidRequest)?
                .accept_storage(
                    request,
                    step,
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    budget - 64,
                )
                .map_err(Error::Global)?;
            if completed {
                self.pending_storage = None;
            }
            return Ok(PermissionStep {
                work: SemanticWork {
                    structural_transitions: work.transitions + 64,
                    metadata_bytes: metadata,
                    ..Default::default()
                },
                literal: None,
                done: false,
            });
        }
        if let Some(reference) = self.reference.as_mut() {
            return reference
                .advance(document, quota, budget)
                .map(|work| PermissionStep {
                    work,
                    literal: None,
                    done: false,
                });
        }

        let mut work = SemanticWork {
            structural_transitions: 64,
            ..Default::default()
        };
        match self.phase {
            Phase::Queries => unreachable!("有限code块必须先由原配对owner完成引用库存交接"),
            Phase::Done => {
                return Ok(PermissionStep {
                    work,
                    literal: None,
                    done: true,
                });
            }
            Phase::Parse => {
                work.parser_input_bytes = self.bytes.len();
                self.parser = Some(SmallParser::new(
                    std::mem::take(&mut self.bytes),
                    self.options,
                    self.references
                        .take()
                        .expect("父事务已交付当前块的唯一已闭合引用库存"),
                )?);
                self.phase = Phase::Events;
            }
            Phase::Events => {
                let parser = self.parser.as_mut().unwrap();
                // 一步只取一个真实事件；原 Parser 输入已由上个量子固定准入，不积攒全块范围目录。
                let event = parser.events.next();
                if let Some(error) = parser.failure.get() {
                    return Err(Error::Window(error));
                }
                match event {
                    Some((Event::Code(_), range)) => {
                        return Ok(PermissionStep {
                            work,
                            literal: Some(
                                self.range.start + range.start..self.range.start + range.end,
                            ),
                            done: false,
                        });
                    }
                    Some(_) => {}
                    None => {
                        self.parser = None;
                        self.phase = Phase::Done;
                    }
                }
            }
            Phase::Cursor | Phase::VerifyRoot => {
                let verifying = matches!(self.phase, Phase::VerifyRoot);
                let tree = document
                    .root
                    .as_ref()
                    .map_or(0, |root| usize::from(root.height()));
                let child = budget.saturating_sub(tree + 64) * 4 / 5;
                if child < 2048 {
                    return Err(Error::WorkLimit);
                }
                let cursor = self.cursor.as_mut().unwrap();
                let frame = document
                    .table_source_frame(cursor.next_byte(), child)
                    .map_err(Error::Source)?;
                let source_bytes = frame.bytes.len();
                let workspace_before = cursor.allocated_workspace_bytes();
                let step = cursor
                    .advance(
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
                    )
                    .map_err(Error::Global);
                frame.finish().map_err(Error::Source)?;
                let step = step?;
                let allocated = cursor.allocated_workspace_bytes() - workspace_before;
                quota.admit_metadata(allocated).map_err(Error::Window)?;
                work.metadata_bytes += allocated;
                let measured = match &step {
                    InlineCodeStep::Progress { work }
                    | InlineCodeStep::NeedInput { work }
                    | InlineCodeStep::Span { work, .. }
                    | InlineCodeStep::Prefix { work, .. }
                    | InlineCodeStep::Emphasis { work, .. }
                    | InlineCodeStep::Link { work, .. }
                    | InlineCodeStep::LinkEnd { work, .. }
                    | InlineCodeStep::Storage { work, .. }
                    | InlineCodeStep::Reference { work, .. }
                    | InlineCodeStep::Proven { work, .. }
                    | InlineCodeStep::NeedsGeneralInline { work } => *work,
                };
                // 游标可先交Storage/Reference而未吃完整帧；源账仍收本次实际借片长度。
                work.source_bytes = source_bytes;
                work.structural_transitions += measured.transitions + tree;
                work.source_tree_steps = tree;
                match step {
                    InlineCodeStep::Storage { request, .. } => self.pending_storage = Some(request),
                    InlineCodeStep::Reference { request, .. } => {
                        let metadata = std::mem::size_of::<ReferenceLabel>();
                        quota.admit_metadata(metadata).map_err(Error::Window)?;
                        work.metadata_bytes += metadata;
                        self.reference = Some(Box::new(ReferenceLabel::new(request)));
                    }
                    InlineCodeStep::Span { proof, .. } => {
                        return Ok(PermissionStep {
                            work,
                            literal: Some(proof.source_range()),
                            done: false,
                        });
                    }
                    InlineCodeStep::Proven { .. } => {
                        self.cursor = None;
                        self.phase = if verifying {
                            Phase::Inspect
                        } else {
                            Phase::Done
                        };
                    }
                    InlineCodeStep::NeedsGeneralInline { .. } => {
                        self.cursor = None;
                        self.next = self.range.start;
                        self.phase = Phase::Inspect; // 若全块根本无 tick，可继续证明无代码；遇真实 tick 则保持未知。
                    }
                    _ => {}
                }
            }
            Phase::Inspect | Phase::Copy => {
                if self.next == self.range.end {
                    self.phase = if matches!(self.phase, Phase::Inspect) {
                        Phase::Done
                    } else {
                        Phase::Queries
                    };
                    return Ok(PermissionStep {
                        work,
                        literal: None,
                        done: self.done(),
                    });
                }
                let tree = document
                    .root
                    .as_ref()
                    .map_or(0, |root| usize::from(root.height()));
                let maximum = (budget.saturating_sub(tree + 64) / 4).max(1);
                let frame = document
                    .table_source_frame(self.next, maximum.min(self.range.end - self.next))
                    .map_err(Error::Source)?;
                let count = frame.bytes.len().min(self.range.end - self.next);
                let tick =
                    matches!(self.phase, Phase::Inspect) && frame.bytes[..count].contains(&b'`');
                if matches!(self.phase, Phase::Copy) {
                    self.bytes.extend_from_slice(&frame.bytes[..count]);
                    work.copied_source_bytes = count;
                }
                self.next += count;
                frame.finish().map_err(Error::Source)?;
                work.source_bytes = count;
                work.source_tree_steps = tree;
                work.structural_transitions += count + tree;
                if tick {
                    if !self.root || self.range.len() > 2048 {
                        return Err(Error::Window(MarkdownTableWindowError::ContextPending));
                    }
                    self.bytes = Vec::with_capacity(self.range.len());
                    work.allocated_source_capacity = self.bytes.capacity();
                    self.next = self.range.start;
                    self.phase = Phase::Copy;
                }
            }
        }
        Ok(PermissionStep {
            work,
            literal: None,
            done: self.done(),
        })
    }
}
