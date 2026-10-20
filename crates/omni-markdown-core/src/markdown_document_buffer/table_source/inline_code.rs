//! 同一 Global 闭合 InlineBlock 的代码权限；每次只保当前事实、一个游标和一个 span。

use super::*;
use crate::markdown_document_buffer::table_window_request::{
    MarkdownTableRequestError as Error, SemanticWork,
};
use crate::markdown_viewport_presentation::table_window::references::MarkdownTableReferenceQueries;
use pulldown_cmark::block_frontier::SourceFrame;
use pulldown_cmark::global_frontier::{
    GlobalSourceAuthority, InlineCodeCursor, InlineCodeStep, InlinePlainLimits, LeafBlockKind,
    VerifiedGlobalInlineBlock, VerifiedGlobalLeafBlock,
};
use pulldown_cmark::{BrokenLink, CowStr, Event, OffsetIter, Options, Parser};
use std::{cell::Cell, ops::Range, rc::Rc};

type Callback = Box<dyn FnMut(BrokenLink<'static>) -> Option<(CowStr<'static>, CowStr<'static>)>>;

/// Parser 永远先于其固定输入销毁；私有类型不暴露文本替换或跨 owner 借用。
struct SmallParser {
    events: OffsetIter<'static, Callback>,
    _text: Box<str>,
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
        let callback: Callback = Box::new(move |link| {
            // 同一Global已解析的唯一库存只读共享；没有查询权限时保首错，不猜缺失或复制整个Map。
            let result = references.resolve_for_classification(&link.reference);
            match result {
                Ok(value) => value.map(|(destination, title)| {
                    (
                        CowStr::from(destination.to_owned()),
                        CowStr::from(title.to_owned()),
                    )
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
    cursor: Option<InlineCodeCursor>,
    references: Option<Rc<MarkdownTableReferenceQueries>>,
}

pub(super) struct PermissionStep {
    pub work: SemanticWork,
    pub literal: Option<Range<usize>>,
    pub done: bool,
}

/// AST 内部开销仍作为固定小块 Parser 输入单独报告；此处只计直接持有的状态与源容量。
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
            cursor: None,
            references: None,
        })
    }

    /// 实际 Fact 决定 Root/scope/options/范围；不接受调用方布尔、裸 Range 或 lookahead 作为权限。
    pub(super) fn new(block: VerifiedGlobalInlineBlock) -> Result<Self, Error> {
        let range = block.physical_range();
        let root = block.scope_depth() == 1;
        let options = block.options();
        let cursor = if root {
            let requested_end = if range.len() > 2048 {
                range.end
            } else {
                block.source_range().start
            };
            block
                .into_code_cursor(
                    requested_end,
                    InlinePlainLimits {
                        max_byte_work: usize::MAX,
                        max_transitions: usize::MAX,
                    },
                )
                .map_err(Error::Global)?
        } else {
            None
        };
        let phase = if cursor.is_some() {
            if range.len() > 2048 {
                Phase::Cursor
            } else {
                Phase::VerifyRoot
            }
        } else {
            Phase::Inspect
        };
        Ok(Self {
            next: range.start,
            range,
            options,
            root,
            phase,
            bytes: Vec::new(),
            parser: None,
            cursor,
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
    pub(super) fn reference_input(&self) -> Result<Option<(&str, Options)>, Error> {
        if !matches!(self.phase, Phase::Queries) {
            return Ok(None);
        }
        let source = std::str::from_utf8(&self.bytes).map_err(|_| Error::InvalidRequest)?;
        Ok(Some((source, self.options)))
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
        budget: usize,
    ) -> Result<PermissionStep, Error> {
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
                let measured = match &step {
                    InlineCodeStep::Progress { work }
                    | InlineCodeStep::NeedInput { work }
                    | InlineCodeStep::Span { work, .. }
                    | InlineCodeStep::Prefix { work, .. }
                    | InlineCodeStep::Emphasis { work, .. }
                    | InlineCodeStep::Proven { work, .. }
                    | InlineCodeStep::NeedsGeneralInline { work } => *work,
                };
                work.source_bytes = measured.byte_reads;
                work.structural_transitions += measured.transitions + tree;
                work.source_tree_steps = tree;
                match step {
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
