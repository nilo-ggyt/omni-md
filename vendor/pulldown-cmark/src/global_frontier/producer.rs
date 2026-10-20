// Copyright 2015 Google Inc. All rights reserved.
// 全局控制流来自固定pulldown-cmark0.13.4 firstpass；完整MIT条款见LICENSE。

//! 全局活动spine与一个叶块续体共同决定table/refdef权限；不保存已完成AST或正文行。

use super::*;
use crate::leaf_scan::{ClosingFenceResult, CodeFenceCloser, HtmlTerminator, LeafStart};
use crate::leaf_stream::{LeafProbe, LeafProbeStep};
use crate::line_frontier::{
    LineFacts, LineReadStep, LineReader, PositionError, SourcePosition as Position,
};
use crate::structural_spine::{StructuralEntry, StructuralKind};
impl From<PositionError> for GlobalFrontierError {
    fn from(error: PositionError) -> Self {
        match error {
            PositionError::Utf8 => Self::SourceUtf8,
            PositionError::Overflow => Self::ArithmeticOverflow,
        }
    }
}
use crate::block_frontier::{TableContinuationState, TableRowFact, TableStep};
use crate::container_state::{ListState, ParentKind};
use crate::label_frontier::{LabelScanner, LabelStep};
use crate::reference_frontier::{
    ReferenceCandidate, ReferenceDefinitionScanner, ReferenceLinePurpose, ReferenceStep,
};
use crate::source_frontier::{
    admitted_bytes, MetadataSourceStep, MetadataValidation, PendingMetadataValidation,
};

// 逻辑状态机控制费用不是 CPU 指令测量；固定工作区只在真实初始化/释放时另外收费。
const GLOBAL_CONTROL_WORK: usize = 128;
const PREFIX_WORKSPACE_WORK: usize = 1024;

mod semantic_boundary;
use semantic_boundary::SemanticBoundary;

#[derive(Clone, Copy, Debug)]
pub struct GlobalFrontierLimits {
    pub spine: StructuralSpineLimits,
    pub max_byte_work: usize,
    pub max_transitions: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 每step由真实source owner给出的取消/代次；不能只在构造时检查一次后沿用过期权限。
pub struct GlobalSourceAuthority {
    pub current_revision: u64,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// byte_reads包括必要重放；transitions包括容器页、零byte状态转换，均为单调fuel。
pub struct GlobalWork {
    pub byte_reads: usize,
    pub transitions: usize,
}

#[derive(Debug)]
pub enum GlobalStep {
    Progress { next_byte: usize, work: GlobalWork },
    NeedInput { next_byte: usize, work: GlobalWork },
    Fact { fact: GlobalFact, work: GlobalWork },
    Complete { work: GlobalWork },
}

#[derive(Debug)]
pub enum GlobalFact {
    Table(GlobalTableFact),
    Row(TableRowFact),
    TableEnd(crate::block_frontier::VerifiedTableEnd),
    Reference(VerifiedGlobalReference),
    /// 原容器路径已经确认的GFM脚注定义；存在性不要求body非空，也不授正文inline权限。
    Footnote(VerifiedGlobalFootnoteDefinition),
    InlineBlock(VerifiedGlobalInlineBlock),
    /// 真实 leaf offset 可能含后继行 prefix，不能据此推进无表水位或 parser cursor。
    LeafBlock(VerifiedGlobalLeafBlock),
}

#[derive(Clone, Debug)]
/// 只由原FootnoteSpace之前的真实label与closing-colon路径创建，字段不允许外部拼造。
/// 票据不保Doc/spine/正文；调用方仍须核原Source身份、健康与取消，才能借标签字节解码。
pub struct VerifiedGlobalFootnoteDefinition {
    revision: u64,
    source_end: usize,
    source: std::ops::Range<usize>,
    reference: std::ops::Range<usize>,
}

impl VerifiedGlobalFootnoteDefinition {
    /// 同原Global的修订；不能替代外层DocRoot/原文件健康守卫。
    pub fn source_revision(&self) -> u64 {
        self.revision
    }
    /// 真正整篇Source末端，不是定义标签、body或当前frame的末端。
    pub fn source_end(&self) -> usize {
        self.source_end
    }
    /// 原'[^'开头至已确认colon之后；不含body与随后容器空白。
    pub fn source_range(&self) -> std::ops::Range<usize> {
        self.source.clone()
    }
    /// key从'^'后开始，包含真正closing bracket，供原有限LabelScanner确认结束。
    pub fn reference_range(&self) -> std::ops::Range<usize> {
        self.reference.clone()
    }
    /// 原GFM定义禁止换行；解码借同一真实label内核，不建立全源footdefs Map。
    pub fn reference_decoder(&self) -> super::InlineReferenceDecoder {
        super::InlineReferenceDecoder::from_footnote(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// H/D均为含容器prefix和EOL的完整物理源行；内容起点必须显式加各自prefix_bytes。
/// 不从原Parser的content起点减一个猜测prefix，也不伪造projection/NSString offset。
pub struct GlobalTableFact {
    pub header_byte_start: usize,
    pub header_byte_end: usize,
    pub header_prefix_bytes: usize,
    pub header_source_line: usize,
    pub separator_byte_start: usize,
    pub separator_byte_end: usize,
    pub separator_prefix_bytes: usize,
    pub separator_source_line: usize,
    pub columns: usize,
}

#[derive(Clone, Debug)]
/// 只由真实block路径创建；候选起点spine在label/title前瞻期间保持同一snapshot。
pub struct VerifiedGlobalReference {
    pub(super) candidate: ReferenceCandidate,
    pub(super) spine: StructuralSpineSnapshot,
    pub(super) options: Options,
    pub(super) source_end: usize,
}
impl VerifiedGlobalReference {
    pub fn candidate(&self) -> &ReferenceCandidate {
        &self.candidate
    }
    pub fn container_depth(&self) -> usize {
        self.spine.len()
    }
}

/// ActiveTable不能由任意调用者自造seed；只有下方真实header+separator成功路径构造本值。
pub(crate) struct VerifiedGlobalTable {
    spine: StructuralSpineSnapshot,
    revision: u64,
    source_end: usize,
    header_start: usize,
    separator_start: usize,
    separator_prefix_bytes: usize,
    separator_terminated: bool,
    columns: usize,
    next_byte: usize,
    next_utf16: usize,
    next_line: usize,
    options: Options,
    header_inline: crate::block_frontier::cell::TableInlineRow,
}
impl VerifiedGlobalTable {
    pub(crate) fn into_parts(
        self,
    ) -> (
        StructuralSpineSnapshot,
        u64,
        usize,
        usize,
        usize,
        usize,
        bool,
        usize,
        usize,
        usize,
        usize,
        Options,
        crate::block_frontier::cell::TableInlineRow,
    ) {
        (
            self.spine,
            self.revision,
            self.source_end,
            self.header_start,
            self.separator_start,
            self.separator_prefix_bytes,
            self.separator_terminated,
            self.columns,
            self.next_byte,
            self.next_utf16,
            self.next_line,
            self.options,
            self.header_inline,
        )
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Block,
    Paragraph,
    ReferenceContinuation,
    QuoteContinuation,
    TaskContinuation,
    Opaque(Opaque),
}
#[derive(Clone, Copy, Debug)]
enum Opaque {
    Fence {
        marker: u8,
        count: usize,
        indent: usize,
    },
    Html(HtmlTerminator),
    HtmlBlank,
    Indented,
    Metadata {
        marker: u8,
    },
}
#[derive(Clone, Copy, Debug)]
enum PrefixUse {
    Block,
    Paragraph,
    Reference,
    Opaque,
    Separator,
    ReferenceLine(ReferenceLinePurpose),
}
#[derive(Clone, Copy, Debug)]
enum OpAfter {
    OpeningSpace,
    OpeningList,
    OpeningQuote,
    BlockBlankEol,
    BlockIndent,
    ParagraphIndent,
    ParagraphSpace,
    ParagraphBlank,
    SeparatorIndent,
    ReferenceTailSpace,
    ReferenceTailEol,
    FootnoteSpace,
    OpaqueIndent,
    OpaqueFenceExtra,
    OpaqueBlankEol,
    InterruptList,
    InterruptListSpace,
    InterruptListEol,
    InterruptQuote,
    ListBlankEol,
    HtmlBlankEol,
    HtmlBlankScan,
    TaskSpace,
    QuoteTagSpace,
    QuoteTagEol,
}
#[derive(Clone, Copy, Debug)]
enum LineUse {
    Paragraph { first: bool },
    Separator,
    SimpleBlock,
    Opaque,
    EmptyItemBlank,
    Setext,
    ReferenceTail,
    InterruptCheck,
    MetadataOpening,
    Closing,
}
#[derive(Clone, Copy, Debug)]
enum LeafUse {
    Block,
    Interrupt,
}
#[derive(Clone, Copy, Debug)]
enum AfterCommit {
    NewBlock,
    ParagraphNext,
    ReferenceNext,
    OpaqueNext,
    TableReady,
    EmitReference,
    EmitInlineBlock,
    EmitLeafBlock,
}
#[derive(Clone, Copy, Debug)]
enum Ready {
    Begin,
    PopTo(usize),
    Opening,
    TryFootnote,
    TryList,
    TryQuote,
    BlockBlank,
    Leaf,
    ParagraphStart,
    ParagraphLine(bool),
    Interrupt,
    ContinueParagraph,
    EndParagraph,
    ReadOpaque,
    ReferenceStart,
    ReferenceAccepted,
    PublishTable,
    PublishInlineBlock,
    PublishLeafBlock,
    Finish,
}
enum Phase {
    Ready(Ready),
    Prefix(PrefixUse),
    Operation(LineStartStream, OpAfter),
    Leaf(LeafProbe, LeafUse),
    Line(LineReader, LineUse),
    Commit {
        target: usize,
        after: AfterCommit,
    },
    Peek {
        start: usize,
        next: usize,
        count: usize,
        bytes: [u8; 16],
        used: usize,
        kind: u8,
    },
    Footnote {
        scanner: LabelScanner,
        start: usize,
        next: usize,
    },
    Closing {
        probe: CloseProbe,
        start: usize,
        next: usize,
    },
    Reference(ReferenceDefinitionScanner),
    Metadata(PendingMetadataValidation),
    Table,
    Closed,
}

/// 只保一个尚可改型的真实paragraph；最后正文行只存锚点，不保存行字节。
struct PendingInlineBlock {
    start: usize,
    physical_start: usize,
    // 首行已经通过真实 opening；保留完整 tab 状态，不让后继 consumer 重扫或猜测首行 prefix。
    first_state_origin: usize,
    first_state: LineStartState,
    last_line: Option<LineFacts>,
    spine: StructuralSpineSnapshot,
}

/// 一个活动producer只持有限结构metadata；所有source由调用者在next_byte处短借，不保映射强owner。
pub struct GlobalFrontier {
    revision: u64,
    source_end: usize,
    options: Options,
    limits: GlobalFrontierLimits,
    work: GlobalWork,
    position: Position,
    classified: (usize, usize),
    spine: StructuralSpineSnapshot,
    phase: Phase,
    mode: Mode,
    origin: usize,
    state: LineStartState,
    saved: LineStartState,
    container_saved: LineStartState,
    matched: usize,
    outer_indent: usize,
    indent: usize,
    header: Option<LineFacts>,
    header_line: usize,
    header_physical_start: usize,
    separator: Option<LineFacts>,
    reference: Option<ReferenceDefinitionScanner>,
    reference_fact: Option<VerifiedGlobalReference>,
    // closing colon确认后暂持唯一未发布定义；原FootnoteSpace完成scope归属后才交Fact。
    footnote_fact: Option<VerifiedGlobalFootnoteDefinition>,
    reference_line: Option<ReferenceLinePurpose>,
    reference_spine: Option<StructuralSpineSnapshot>,
    begin_list_item: Option<usize>,
    last_line_blank: bool,
    spine_error: Option<StructuralSpineError>,
    metadata_start: usize,
    metadata_marker: u8,
    interrupt_line: Option<LineFacts>,
    header_interrupt: bool,
    opaque_body_state: LineStartState,
    header_origin: usize,
    header_state: LineStartState,
    reference_origin: usize,
    reference_state: LineStartState,
    prefix_last: Option<u8>,
    footnote_interrupt: bool,
    prefix: Option<ContainerPrefix>,
    // 每次 advance 清零，helper 只登记本步真实发生的一次固定 page-pointer 工作区操作。
    step_prefix_workspace_work: usize,
    table: Option<TableContinuationState>,
    inline_block: Option<PendingInlineBlock>,
    inline_fact: Option<VerifiedGlobalInlineBlock>,
    leaf_block: Option<PendingGlobalLeaf>,
    leaf_fact: Option<VerifiedGlobalLeafBlock>,
    // 只有语义上下文前探持一个真实恢复点；不保存未来Fact目录，也不另建结构前沿。
    semantic_boundary: Option<SemanticBoundary>,
    failure: Option<GlobalFrontierError>,
}
impl std::fmt::Debug for GlobalFrontier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalFrontier")
            .field("revision", &self.revision)
            .field("source_bytes", &self.source_end)
            .field("committed_byte", &self.position.byte)
            .field("depth", &self.spine.len())
            .field("work", &self.work)
            .finish()
    }
}

enum CloseProbe {
    Fence(CodeFenceCloser),
    Metadata(crate::metadata_scan::MetadataCloser),
}

impl GlobalFrontier {
    /// 以实际不可变source的revision/总长建立前沿；调用者保留弱root身份并在每step交当前authority。
    /// 不取得source强owner或全文slice。只有产品实际选项可用；旧footnote/definition-list返回显式不支持。
    /// 页容量/分配与工作fuel在分配前准入，预算不足是未完成，不能映射为SourceFallback成功。
    pub fn new(
        revision: u64,
        source_end: usize,
        options: Options,
        limits: GlobalFrontierLimits,
    ) -> Result<Self, GlobalFrontierError> {
        // 产品未开启的旧footnote/definition-list语法不由此原型猜测；原完整Parser选项保持原实现。
        if options.contains(Options::ENABLE_OLD_FOOTNOTES)
            || options.contains(Options::ENABLE_DEFINITION_LIST)
        {
            return Err(GlobalFrontierError::UnsupportedOptions);
        }
        if limits.max_byte_work == 0 || limits.max_transitions == 0 {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        Ok(Self {
            revision,
            source_end,
            options,
            limits,
            work: GlobalWork::default(),
            position: Position::default(),
            classified: (0, 0),
            spine: StructuralSpineSnapshot::new(limits.spine)?,
            phase: Phase::Ready(Ready::Begin),
            mode: Mode::Block,
            origin: 0,
            state: LineStartState::default(),
            saved: LineStartState::default(),
            container_saved: LineStartState::default(),
            matched: 0,
            outer_indent: 0,
            indent: 0,
            header: None,
            header_line: 0,
            header_physical_start: 0,
            separator: None,
            reference: None,
            reference_fact: None,
            footnote_fact: None,
            reference_line: None,
            reference_spine: None,
            begin_list_item: None,
            last_line_blank: false,
            spine_error: None,
            metadata_start: 0,
            metadata_marker: 0,
            interrupt_line: None,
            header_interrupt: false,
            opaque_body_state: LineStartState::default(),
            prefix_last: None,
            header_origin: 0,
            header_state: LineStartState::default(),
            reference_origin: 0,
            reference_state: LineStartState::default(),
            footnote_interrupt: false,
            prefix: None,
            step_prefix_workspace_work: 0,
            table: None,
            inline_block: None,
            inline_fact: None,
            leaf_block: None,
            leaf_fact: None,
            semantic_boundary: None,
            failure: None,
        })
    }

    /// 可能要求重借lookahead之前的源；调用者不能以自己上次交付的frame末尾覆盖本锚点。
    pub fn next_byte(&self) -> usize {
        match &self.phase {
            Phase::Prefix(_) => self
                .prefix
                .as_ref()
                .expect("Prefix阶段拥有固定游标")
                .next_byte(),
            Phase::Operation(operation, _) => operation.next_byte(),
            Phase::Leaf(probe, _) => probe.next_byte(),
            Phase::Line(reader, _) => reader.next_byte(),
            Phase::Commit { .. } => self.position.byte,
            // 固定至多16字节仅用于有限marker；不足时继续借真实source，不把短切片尾当语法EOF。
            Phase::Peek { next, .. }
            | Phase::Footnote { next, .. }
            | Phase::Closing { next, .. } => *next,
            Phase::Reference(scanner) => scanner.next_byte(),
            Phase::Metadata(scanner) => scanner.next_byte(),
            Phase::Table => self
                .table
                .as_ref()
                .expect("Table阶段拥有成员续体")
                .next_source_byte(),
            Phase::Ready(ready) => match ready {
                Ready::Begin
                | Ready::PopTo(_)
                | Ready::Finish
                | Ready::PublishTable
                | Ready::PublishInlineBlock => self.position.byte,
                _ => self.origin + self.state.ix,
            },
            Phase::Closed => self.position.byte,
        }
    }
    pub fn committed_byte(&self) -> usize {
        self.position.byte
    }
    /// 已完整分类的源前缀；两个标量分别为真实byte与UTF16末端。
    /// 部分Commit、header/separator前瞻和短frame不推进此水位。调用者必须先消费同step
    /// 返回的Table/Row/End事实，才能用其补集签无表区间；本值自身不是普通inline Ready。
    pub fn table_classified_position(&self) -> (usize, usize) {
        self.classified
    }

    pub fn total_work(&self) -> GlobalWork {
        self.work
    }

    /// 只导出真实ActiveTable的已提交行边界；调用者必须在自身metadata总预算内保留handle。
    /// 页继续共享同一4MiB活容量/分配fuel，不按每个viewport复制整段spine或重新授予任意seed。
    pub fn table_checkpoint(&self) -> Option<crate::block_frontier::TableCheckpoint> {
        self.table
            .as_ref()
            .and_then(TableContinuationState::checkpoint)
    }

    fn start_prefix(
        &mut self,
        origin: usize,
        purpose: PrefixUse,
    ) -> Result<(), GlobalFrontierError> {
        if self.prefix.is_some() {
            // 原游标必须先完成并释放；禁止同一步隐式替换而遗漏第二份固定工作区费用。
            return Err(GlobalFrontierError::InvalidFrame);
        }
        self.record_prefix_workspace()?;
        // 固定4KiB游标独立于phase enum保存，避免每个小frame转换复制整个最大variant或每行Box分配。
        self.prefix = Some(ContainerPrefix::new(
            self.revision,
            origin,
            self.source_end,
            &self.spine,
            self.options,
        ));
        self.phase = Phase::Prefix(purpose);
        Ok(())
    }

    fn record_prefix_workspace(&mut self) -> Result<(), GlobalFrontierError> {
        if self.step_prefix_workspace_work != 0 {
            // 当前状态机一阶段至多一次初始化或释放；内部新增双操作须先扩大具名准入，不能漏收费。
            return Err(GlobalFrontierError::InvalidFrame);
        }
        self.step_prefix_workspace_work = PREFIX_WORKSPACE_WORK;
        Ok(())
    }
    fn operation(
        &mut self,
        operation: LineStartOperation,
        after: OpAfter,
    ) -> Result<(), GlobalFrontierError> {
        self.phase = Phase::Operation(
            LineStartStream::new(
                self.revision,
                self.origin,
                self.source_end,
                self.state,
                operation,
            )?,
            after,
        );
        Ok(())
    }
    fn read_line(&mut self, start: usize, raw: bool, purpose: LineUse) {
        // 只为正在读取的真实叶行重置标量判定，关闭行不会重新写入正文包络。
        if matches!(purpose, LineUse::Opaque | LineUse::SimpleBlock) {
            if let Some(leaf) = self.leaf_block.as_mut() {
                leaf.begin_line();
            }
        }
        self.phase = Phase::Line(LineReader::new(start, self.source_end, raw), purpose);
    }
    fn commit(&mut self, target: usize, after: AfterCommit) {
        self.phase = Phase::Commit { target, after };
    }
    fn peek(&mut self, start: usize, count: usize, kind: u8) {
        self.phase = Phase::Peek {
            start,
            next: start,
            count,
            bytes: [0; 16],
            used: 0,
            kind,
        };
    }

    /// 每次先检查实际owner代次、取消和frame；预算失败不发布半个table/refdef事实。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<GlobalStep, GlobalFrontierError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if authority.cancelled {
            // 本轮新叶块的私有 scope claims 与未提交事实同时撤销，sticky 失败不能复活它们。
            self.leaf_block = None;
            self.leaf_fact = None;
            self.semantic_boundary = None;
            self.failure = Some(GlobalFrontierError::Cancelled);
            return Err(GlobalFrontierError::Cancelled);
        }
        if authority.current_revision != self.revision {
            self.leaf_block = None;
            self.leaf_fact = None;
            self.semantic_boundary = None;
            self.failure = Some(GlobalFrontierError::SourceSuperseded);
            return Err(GlobalFrontierError::SourceSuperseded);
        }
        // 调用协议错误在任何状态转换前拒绝，可用正确frame重试；真实解析/预算失败则保留不可伪签首因。
        if budget < 2048 {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        admitted_bytes(
            &frame,
            self.revision,
            self.source_end,
            self.next_byte(),
            budget,
        )?;
        match self.advance_inner(frame, authority, budget) {
            Ok(step) => Ok(step),
            Err(error) => {
                // UTF8、结构或总 fuel 失败同样丢弃尚未公开的叶块，不能在后继 advance 补签。
                self.leaf_block = None;
                self.leaf_fact = None;
                self.semantic_boundary = None;
                self.failure = Some(error);
                Err(error)
            }
        }
    }

    fn advance_inner(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<GlobalStep, GlobalFrontierError> {
        if authority.cancelled {
            return Err(GlobalFrontierError::Cancelled);
        }
        if authority.current_revision != self.revision {
            return Err(GlobalFrontierError::SourceSuperseded);
        }
        if budget < 2048 {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        let next = self.next_byte();
        let limit = admitted_bytes(&frame, self.revision, self.source_end, next, budget)?;
        if self.work.byte_reads >= self.limits.max_byte_work
            || self.work.transitions >= self.limits.max_transitions
        {
            return Err(GlobalFrontierError::WorkFuel);
        }
        let fuel = (self.limits.max_transitions - self.work.transitions).min(budget);
        if fuel < 2048 {
            return Err(GlobalFrontierError::WorkFuel);
        }
        let table_phase = matches!(self.phase, Phase::Table);
        // 非 Table 先为固定控制和本步最多一次真实工作区操作留出最坏额度，
        // 未实际使用的 workspace 预留不记入累计消费；Table 保持其原本自含控制费的准入合同。
        let reserve = if table_phase {
            PREFIX_WORKSPACE_WORK
        } else {
            PREFIX_WORKSPACE_WORK + GLOBAL_CONTROL_WORK
        };
        // 最小2048仍有224以上子 fuel；共享原语在这个有界额度内推进，不制造零预算忙循环。
        let child_fuel = (fuel - reserve) / 4;
        let byte_fuel = (self.limits.max_byte_work - self.work.byte_reads).min(child_fuel);
        if byte_fuel == 0 {
            return Err(GlobalFrontierError::WorkFuel);
        }
        let limit = limit.min(byte_fuel);
        let spine_before = self.spine.work_used();
        self.step_prefix_workspace_work = 0;
        let phase = std::mem::replace(&mut self.phase, Phase::Closed);
        let mut step_work = GlobalWork {
            byte_reads: 0,
            transitions: 1,
        };
        let mut fact = None;
        let mut need_input = false;
        let mut complete = false;
        match phase {
            // 纯控制转换不读取候选正文；其固定成本与可能发生的spine写时复制统一收费。
            Phase::Ready(ready) => {
                // 结束源的控制态也必须等真实EOF；不能先pop全部容器再把未完成读取伪装Complete。
                if self.position.byte == self.source_end
                    && matches!(ready, Ready::Begin | Ready::Finish)
                    && !frame.real_eof
                {
                    need_input = true;
                    self.phase = Phase::Ready(ready);
                } else {
                    self.ready(ready)?;
                }
            }
            // 每页/role单独可恢复；首不匹配层只返回matched，是否容许lazy continuation由真实调用上下文决定。
            Phase::Prefix(purpose) => {
                let (progress, work) = self
                    .prefix
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .advance(&frame, byte_fuel, child_fuel)?;
                step_work = GlobalWork {
                    byte_reads: work.byte_reads,
                    transitions: work.transitions.max(1),
                };
                match progress {
                    PrefixProgress::Complete { state, matched } => {
                        self.record_prefix_workspace()?;
                        self.prefix = None;
                        self.prefix_complete(purpose, state, matched)?;
                    }
                    PrefixProgress::Progress => self.phase = Phase::Prefix(purpose),
                    PrefixProgress::NeedInput { .. } => {
                        need_input = true;
                        self.phase = Phase::Prefix(purpose);
                    }
                }
            }
            // LineStart失败也可能部分消费空白；只有Complete才发布其四个原标量，禁止用lookahead cursor替代ix。
            Phase::Operation(mut operation, after) => {
                let step = operation.advance(
                    LineStartFrame {
                        source_revision: frame.source_revision,
                        byte_start: frame.byte_start,
                        bytes: frame.bytes,
                        real_eof: frame.real_eof,
                    },
                    byte_fuel,
                    child_fuel,
                )?;
                step_work = GlobalWork {
                    byte_reads: step.work.byte_reads,
                    transitions: step.work.transitions.max(1),
                };
                match step.progress {
                    LineStartProgress::Complete { state, value } => {
                        self.state = state;
                        self.operation_complete(after, value)?;
                    }
                    LineStartProgress::Progress { .. } => {
                        self.phase = Phase::Operation(operation, after)
                    }
                    LineStartProgress::NeedInput { .. } => {
                        need_input = true;
                        self.phase = Phase::Operation(operation, after);
                    }
                }
            }
            // HTML属性/fence info可能巨长；opening scanner持固定状态，未决时不授予opaque或中断权限。
            Phase::Leaf(mut probe, purpose) => {
                let (step, read, work) = probe.advance(
                    &frame.bytes[..limit],
                    frame.real_eof && limit == frame.bytes.len(),
                    child_fuel,
                );
                step_work = GlobalWork {
                    byte_reads: read,
                    transitions: work.max(1),
                };
                match step {
                    LeafProbeStep::Complete(leaf) => {
                        self.leaf_complete(leaf, purpose, probe.html_type7())?
                    }
                    LeafProbeStep::Progress => self.phase = Phase::Leaf(probe, purpose),
                    LeafProbeStep::NeedInput => {
                        need_input = true;
                        self.phase = Phase::Leaf(probe, purpose);
                    }
                }
            }
            // 行只生成范围/计数，既不缓存源行也不创建inline AST；UTF16与源提交另由Commit处理。
            Phase::Line(mut reader, purpose) => {
                let line_byte_start = reader.next_byte();
                let (step, read, work) = reader.advance(
                    &frame.bytes[..limit],
                    frame.real_eof && limit == frame.bytes.len(),
                    child_fuel,
                );
                let observing_leaf = matches!(purpose, LineUse::Opaque | LineUse::SimpleBlock)
                    && self.leaf_block.is_some();
                if observing_leaf {
                    if let Some(leaf) = self.leaf_block.as_mut() {
                        leaf.observe_line(line_byte_start, &frame.bytes[..read]);
                    }
                }
                if matches!(purpose, LineUse::Opaque) {
                    if let Mode::Opaque(Opaque::Html(mut probe)) = self.mode {
                        // 使用原同一个 terminator 状态逐字节定位首个成功点，不用第二套字符串匹配。
                        for (offset, byte) in frame.bytes[..read].iter().enumerate() {
                            let found_before = probe.is_found();
                            if probe.feed(std::slice::from_ref(byte)) && !found_before {
                                self.leaf_block
                                    .as_mut()
                                    .ok_or(GlobalFrontierError::InvalidFrame)?
                                    .note_terminator(line_byte_start + offset + 1);
                            }
                        }
                        self.mode = Mode::Opaque(Opaque::Html(probe));
                    }
                }
                step_work = GlobalWork {
                    byte_reads: read,
                    // 新叶块观察器每个真实 byte 另收费一次；有限 scalar 更新不领取新扫描配额。
                    transitions: work
                        .saturating_mul(3)
                        .saturating_add(if observing_leaf { read } else { 0 })
                        .max(1),
                };
                match step {
                    LineReadStep::Complete(line) => self.line_complete(line, purpose)?,
                    LineReadStep::Progress => self.phase = Phase::Line(reader, purpose),
                    LineReadStep::NeedInput => {
                        need_input = true;
                        self.phase = Phase::Line(reader, purpose);
                    }
                }
            }
            // 分类前瞻不会自动提交；逐字节验证实际UTF8并推进真实UTF16/行号，完成后才能发布事实。
            Phase::Commit { target, after } => {
                if target < self.position.byte || target > self.source_end {
                    return Err(GlobalFrontierError::InvalidFrame);
                }
                let count = limit.min(target - self.position.byte);
                for byte in &frame.bytes[..count] {
                    self.position.feed(*byte)?;
                }
                step_work = GlobalWork {
                    byte_reads: count,
                    transitions: count.max(1),
                };
                if self.position.byte == target {
                    if self.position.utf8_left != 0 {
                        return Err(GlobalFrontierError::SourceUtf8);
                    }
                    self.commit_complete(after)?;
                } else {
                    need_input = count == frame.bytes.len();
                    self.phase = Phase::Commit { target, after };
                }
            }
            Phase::Peek {
                start,
                mut next,
                count,
                mut bytes,
                mut used,
                kind,
            } => {
                let count_read = (count - used).min(limit);
                bytes[used..used + count_read].copy_from_slice(&frame.bytes[..count_read]);
                used += count_read;
                next += count_read;
                step_work = GlobalWork {
                    byte_reads: count_read,
                    transitions: count_read.max(1),
                };
                if used == count || next == self.source_end {
                    self.peek_complete(start, &bytes[..used], kind)?;
                } else {
                    need_input = count_read == frame.bytes.len();
                    self.phase = Phase::Peek {
                        start,
                        next,
                        count,
                        bytes,
                        used,
                        kind,
                    };
                }
            }
            // GFM footnote标签禁止跨行；失败保留原LineStart供list/quote或paragraph继续分类。
            Phase::Footnote {
                mut scanner,
                start,
                mut next,
            } => {
                // Label每byte最多两个内部转换，输入预算先减半，不能把其bytes上限当全部工作。
                let label_limit = limit;
                let step = scanner.feed(
                    &frame.bytes[..label_limit],
                    frame.real_eof && label_limit == frame.bytes.len(),
                    label_limit.max(1),
                )?;
                let consumed = match step {
                    LabelStep::Progress { consumed }
                    | LabelStep::NeedInput { consumed }
                    | LabelStep::NeedLineContinuation { consumed, .. }
                    | LabelStep::Complete { consumed, .. }
                    | LabelStep::Rejected { consumed } => consumed,
                };
                next += consumed;
                step_work = GlobalWork {
                    byte_reads: consumed,
                    transitions: consumed.saturating_mul(2).max(1),
                };
                match step {
                    LabelStep::Complete { byte_count, .. } => {
                        self.state.ix = start + 2 + byte_count - self.origin;
                        self.peek(
                            start + 2 + byte_count,
                            1,
                            if self.footnote_interrupt { 9 } else { 2 },
                        );
                    }
                    LabelStep::Rejected { .. } | LabelStep::NeedLineContinuation { .. } => {
                        self.state = self.saved;
                        if self.footnote_interrupt {
                            self.footnote_interrupt = false;
                            self.interrupt_other()?;
                        } else {
                            self.phase = Phase::Ready(Ready::TryList);
                        }
                    }
                    LabelStep::Progress { .. } | LabelStep::NeedInput { .. } => {
                        need_input = matches!(step, LabelStep::NeedInput { .. });
                        self.phase = Phase::Footnote {
                            scanner,
                            start,
                            next,
                        };
                    }
                }
            }
            // closing只保marker计数；失败从保留的正文起点重读，不能吞掉extra-indent前瞻。
            Phase::Closing {
                mut probe,
                start,
                mut next,
            } => {
                let mut used = 0;
                let mut result = None;
                while used < limit {
                    let byte = &frame.bytes[used..used + 1];
                    result = match &mut probe {
                        CloseProbe::Fence(scanner) => match scanner.feed(byte, false) {
                            ClosingFenceResult::NeedInput => None,
                            ClosingFenceResult::Closed(count) => Some(Some(count)),
                            ClosingFenceResult::NotClosed => Some(None),
                        },
                        CloseProbe::Metadata(scanner) => match scanner.feed(byte, false) {
                            crate::metadata_scan::MetadataClosing::NeedInput => None,
                            crate::metadata_scan::MetadataClosing::Closed(count) => {
                                Some(Some(count))
                            }
                            crate::metadata_scan::MetadataClosing::NotClosed => Some(None),
                        },
                    };
                    used += 1;
                    next += 1;
                    if result.is_some() {
                        break;
                    }
                }
                if result.is_none() && used == frame.bytes.len() && frame.real_eof {
                    result = match &mut probe {
                        CloseProbe::Fence(scanner) => match scanner.feed(&[], true) {
                            ClosingFenceResult::Closed(count) => Some(Some(count)),
                            _ => Some(None),
                        },
                        CloseProbe::Metadata(scanner) => match scanner.feed(&[], true) {
                            crate::metadata_scan::MetadataClosing::Closed(count) => {
                                Some(Some(count))
                            }
                            _ => Some(None),
                        },
                    };
                }
                step_work = GlobalWork {
                    byte_reads: used,
                    transitions: used.saturating_mul(2).max(1),
                };
                // 仅记录实际由原 closer 消费的短借字节；探测失败时撤销这些临时锚点。
                self.leaf_block
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .observe_closing(frame.byte_start, &frame.bytes[..used]);
                match result {
                    Some(Some(count)) => {
                        self.leaf_block
                            .as_mut()
                            .ok_or(GlobalFrontierError::InvalidFrame)?
                            .accept_closing(start + count);
                        self.read_line(start + count, false, LineUse::Closing);
                    }
                    Some(None) => {
                        self.leaf_block
                            .as_mut()
                            .ok_or(GlobalFrontierError::InvalidFrame)?
                            .reject_closing();
                        self.state = self.opaque_body_state;
                        self.read_line(self.origin + self.state.ix, true, LineUse::Opaque);
                    }
                    None => {
                        need_input = used == frame.bytes.len();
                        self.phase = Phase::Closing { probe, start, next };
                    }
                }
            }
            // refdef可前瞻到多行title后回退URL末尾；候选起点scope与semantic cursor始终独立保留。
            Phase::Reference(mut scanner) => {
                let step = scanner.advance(
                    SourceFrame {
                        source_revision: frame.source_revision,
                        byte_start: frame.byte_start,
                        bytes: &frame.bytes[..limit],
                        real_eof: frame.real_eof && limit == frame.bytes.len(),
                    },
                    child_fuel,
                )?;
                self.reference_step(step, scanner, &mut step_work, &mut need_input)?;
            }
            // 远处closing仅证明opening成立；Accepted后仍从body重放真实容器，不能跳过中途退出的quote/list。
            Phase::Metadata(mut scanner) => {
                let step = scanner.advance(
                    SourceFrame {
                        source_revision: frame.source_revision,
                        byte_start: frame.byte_start,
                        bytes: &frame.bytes[..limit],
                        real_eof: frame.real_eof && limit == frame.bytes.len(),
                    },
                    child_fuel,
                )?;
                let consumed = match step {
                    MetadataSourceStep::Progress { consumed }
                    | MetadataSourceStep::NeedInput { consumed }
                    | MetadataSourceStep::Validated { consumed, .. } => consumed,
                };
                step_work = GlobalWork {
                    byte_reads: consumed,
                    transitions: consumed.max(1),
                };
                match step {
                    MetadataSourceStep::Validated {
                        result: MetadataValidation::Accepted { body_start, .. },
                        ..
                    } => {
                        self.finish_list_checked(self.origin)?;
                        // Accepted 只证明 opening；实际 body/容器退出仍由原 opaque 路径逐行闭合。
                        self.begin_leaf(
                            PendingLeafKind::Metadata {
                                marker: self.metadata_marker,
                                opening_end: body_start,
                            },
                            self.metadata_start,
                        )?;
                        self.spine.push(StructuralEntry {
                            kind: StructuralKind::Other,
                            source_start: self.metadata_start,
                        })?;
                        self.mode = Mode::Opaque(Opaque::Metadata {
                            marker: self.metadata_marker,
                        });
                        self.commit(body_start, AfterCommit::OpaqueNext);
                    }
                    MetadataSourceStep::Validated {
                        result: MetadataValidation::Rejected { .. },
                        ..
                    } => {
                        self.phase =
                            Phase::Leaf(LeafProbe::new(self.metadata_start), LeafUse::Block);
                    }
                    MetadataSourceStep::Progress { .. } | MetadataSourceStep::NeedInput { .. } => {
                        need_input = matches!(step, MetadataSourceStep::NeedInput { .. });
                        self.phase = Phase::Metadata(scanner);
                    }
                }
            }
            // ActiveTable也使用分段分类/计量；失败行留在原semantic起点，交还全局块分类而非消失。
            Phase::Table => {
                let table = self
                    .table
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?;
                let step = table.advance_bounded(
                    SourceFrame {
                        source_revision: frame.source_revision,
                        byte_start: frame.byte_start,
                        bytes: &frame.bytes[..limit],
                        real_eof: frame.real_eof && limit == frame.bytes.len(),
                    },
                    byte_fuel,
                    fuel,
                )?;
                let table_work = table.last_work();
                step_work = GlobalWork {
                    byte_reads: table_work.byte_reads,
                    transitions: table_work.transitions,
                };
                match step {
                    TableStep::Row { fact: row, .. } => {
                        // 真实成员已经完整提交；与同一次Fact一起发布，不能先把该行误签为gap。
                        self.classified = (row.byte_end, row.utf16_end);
                        fact = Some(GlobalFact::Row(row));
                        self.phase = Phase::Table;
                    }
                    TableStep::Ended { byte_offset, .. } => {
                        // 不由“当前window没有下一行”推断表尾；只消费ActiveTable已经真实分类/EOF确认的opaque证明。
                        // clone只有共享spine handle；随后移除活动table确保同一结束事实仅发布一次。
                        fact = Some(GlobalFact::TableEnd(
                            table
                                .verified_end()
                                .ok_or(GlobalFrontierError::InvalidFrame)?,
                        ));
                        let (utf16, line) = table.next_semantic_position();
                        self.position = Position {
                            byte: byte_offset,
                            utf16,
                            line,
                            ..Position::default()
                        };
                        self.classified = (byte_offset, utf16);
                        self.table = None;
                        self.spine.pop()?;
                        self.mode = Mode::Block;
                        self.phase = Phase::Ready(Ready::Begin);
                    }
                    TableStep::Progress { .. } | TableStep::NeedInput { .. } => {
                        need_input = matches!(step, TableStep::NeedInput { .. });
                        self.phase = Phase::Table;
                    }
                }
            }
            // 真正完成与NeedInput分离；已结束实例不重新建立spine或产生空的成功table。
            Phase::Closed => {
                complete = true;
                self.phase = Phase::Closed;
            }
        }
        if let Some(error) = self.spine_error.take() {
            return Err(error.into());
        }
        if matches!(self.phase, Phase::Ready(Ready::PublishTable)) {
            let header = self.header.unwrap();
            let separator = self.separator.unwrap();
            fact = Some(GlobalFact::Table(GlobalTableFact {
                header_byte_start: self.header_physical_start,
                header_byte_end: header.end,
                header_prefix_bytes: header.start - self.header_physical_start,
                header_source_line: self.header_line,
                separator_byte_start: header.end,
                separator_byte_end: separator.end,
                separator_prefix_bytes: separator.start - header.end,
                separator_source_line: self.header_line + 1,
                columns: header.header_columns,
            }));
            self.start_verified_table()?;
        }
        if matches!(self.phase, Phase::Ready(Ready::ReferenceAccepted)) {
            fact = self.reference_fact.take().map(GlobalFact::Reference);
            self.origin = self.position.byte;
            self.state = LineStartState::default();
            self.operation(LineStartOperation::AllSpace, OpAfter::ReferenceTailSpace)?;
        }
        if matches!(self.phase, Phase::Ready(Ready::Opening)) && self.footnote_fact.is_some() {
            // body可以为空，定义存在性在这次原容器接纳后已经成立；不等待后继段落。
            // 只移交这一条Fact，不复制label、不读取body，也不推进消费者的普通inline水位。
            fact = self.footnote_fact.take().map(GlobalFact::Footnote);
        }
        if matches!(self.phase, Phase::Ready(Ready::PublishInlineBlock)) {
            // 只有semantic commit已经到达闭合点才能交付；lookahead即使走得更远也不是块end。
            fact = Some(GlobalFact::InlineBlock(
                self.inline_fact
                    .take()
                    .ok_or(GlobalFrontierError::InvalidFrame)?,
            ));
            self.mode = Mode::Block;
            self.phase = Phase::Ready(Ready::Begin);
        }
        if matches!(self.phase, Phase::Ready(Ready::PublishLeafBlock)) {
            // 所有源坐标与原关闭原因已固定；只有完整 Commit 完成后才能移交票据。
            fact = Some(GlobalFact::LeafBlock(
                self.leaf_fact
                    .take()
                    .ok_or(GlobalFrontierError::InvalidFrame)?,
            ));
            self.mode = Mode::Block;
            self.phase = Phase::Ready(Ready::Begin);
        }
        // Table 已包含自身控制费用。其它阶段只支付具名标量控制和真实工作区事件，
        // 原 spine 的写时复制/角色实际增量仍只加一次；不把每步最坏预留伪记为已执行工作。
        let spine_work = self.spine.work_used() - spine_before;
        if !table_phase {
            step_work.transitions = step_work
                .transitions
                .checked_add(GLOBAL_CONTROL_WORK)
                .and_then(|work| work.checked_add(self.step_prefix_workspace_work))
                .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        }
        step_work.transitions = step_work
            .transitions
            .checked_add(spine_work)
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        if step_work.byte_reads > budget || step_work.transitions > budget {
            return Err(GlobalFrontierError::WorkFuel);
        }
        self.work.byte_reads = self
            .work
            .byte_reads
            .checked_add(step_work.byte_reads)
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        self.work.transitions = self
            .work
            .transitions
            .checked_add(step_work.transitions)
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        if self.work.byte_reads > self.limits.max_byte_work
            || self.work.transitions > self.limits.max_transitions
        {
            return Err(GlobalFrontierError::WorkFuel);
        }
        if let Some(fact) = fact {
            Ok(GlobalStep::Fact {
                fact,
                work: step_work,
            })
        } else if complete {
            Ok(GlobalStep::Complete { work: step_work })
        } else if need_input {
            Ok(GlobalStep::NeedInput {
                next_byte: self.next_byte(),
                work: step_work,
            })
        } else {
            Ok(GlobalStep::Progress {
                next_byte: self.next_byte(),
                work: step_work,
            })
        }
    }
}

#[cfg(test)]
mod tests;

impl GlobalFrontier {
    fn ready(&mut self, ready: Ready) -> Result<(), GlobalFrontierError> {
        match ready {
            Ready::Begin => {
                if self.position.byte == self.source_end {
                    self.phase = Phase::Ready(Ready::Finish);
                    return Ok(());
                }
                self.origin = self.position.byte;
                self.state = LineStartState::default();
                let purpose = match self.mode {
                    Mode::Block => PrefixUse::Block,
                    Mode::Paragraph => PrefixUse::Paragraph,
                    Mode::ReferenceContinuation
                    | Mode::QuoteContinuation
                    | Mode::TaskContinuation => PrefixUse::Reference,
                    Mode::Opaque(_) => PrefixUse::Opaque,
                };
                self.start_prefix(self.origin, purpose)?;
            }
            Ready::PopTo(target) => {
                if self.spine.len() > target {
                    self.spine.pop()?;
                    self.phase = Phase::Ready(Ready::PopTo(target));
                } else {
                    self.phase = Phase::Ready(Ready::Opening);
                }
            }
            // 进入新容器前保存本轮全部tab/indent标量；没有marker时恢复原空白交给叶块缩进规则。
            Ready::Opening => {
                self.container_saved = self.state;
                self.operation(LineStartOperation::Space(4), OpAfter::OpeningSpace)?;
            }
            // 原parse_block优先footnote，再list、quote；标签失败不改变后续候选起点。
            Ready::TryFootnote => {
                self.saved = self.state;
                self.footnote_interrupt = false;
                if self.options.has_gfm_footnotes() {
                    self.peek(self.origin + self.state.ix, 2, 1);
                } else {
                    self.phase = Phase::Ready(Ready::TryList);
                }
            }
            Ready::TryList => self.operation(
                LineStartOperation::List(self.outer_indent),
                OpAfter::OpeningList,
            )?,
            Ready::TryQuote => {
                // 真实扫描状态指向候选 `>` 字节；部分消耗的 tab 可继续贡献缩进列，却不能再加到字节锚点。
                self.saved = self.state;
                self.operation(LineStartOperation::Quote, OpAfter::OpeningQuote)?;
            }
            Ready::BlockBlank => {
                self.saved = self.state;
                self.operation(LineStartOperation::BlankLine, OpAfter::BlockBlankEol)?;
            }
            Ready::Leaf => {
                let start = self.origin + self.state.ix;
                if self.indent == 0
                    && self.options.intersects(
                        Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
                            | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS,
                    )
                {
                    self.peek(start, 1, 6);
                } else {
                    self.phase = Phase::Leaf(LeafProbe::new(start), LeafUse::Block);
                }
            }
            // 真正paragraph只在refdef失败后建立；仅它的首行拥有轻表header扫描资格。
            Ready::ParagraphStart => {
                self.finish_list_checked(self.origin)?;
                self.spine.push(StructuralEntry {
                    kind: StructuralKind::Paragraph,
                    source_start: self.origin + self.state.ix,
                })?;
                // 起点仍可能被真实首行table/后行Setext改型；这里只保pending，不提前发布。
                self.inline_block = Some(PendingInlineBlock {
                    start: self.origin + self.state.ix,
                    physical_start: self.position.byte,
                    first_state_origin: self.origin,
                    first_state: self.state,
                    last_line: None,
                    spine: self.spine.clone(),
                });
                self.mode = Mode::Paragraph;
                self.phase = Phase::Ready(Ready::ParagraphLine(true));
            }
            Ready::ParagraphLine(first) => self.read_line(
                self.origin + self.state.ix,
                false,
                LineUse::Paragraph { first },
            ),
            Ready::Interrupt => {
                self.read_line(self.origin + self.state.ix, false, LineUse::InterruptCheck)
            }
            Ready::ContinueParagraph => {
                self.operation(LineStartOperation::AllSpace, OpAfter::ParagraphSpace)?
            }
            // lazy continuation不提前pop外层容器；确认中断后只关本paragraph，再从原行重走block入口。
            Ready::EndParagraph => {
                if matches!(self.mode, Mode::Paragraph) {
                    // 先保存同一真实scope与已提交正文末端，随后才能移除活动paragraph。
                    self.close_inline_block(InlineBlockKind::Paragraph, self.position.byte)?;
                    self.spine.pop()?;
                    self.phase = Phase::Ready(Ready::PublishInlineBlock);
                } else {
                    // refdef/quote/task的虚拟延续状态本身不是空paragraph，不能制造事实。
                    self.phase = Phase::Ready(Ready::Begin);
                }
                self.mode = Mode::Block;
            }
            Ready::ReadOpaque => self.read_line(self.origin + self.state.ix, true, LineUse::Opaque),
            Ready::ReferenceStart => {
                self.reference_origin = self.origin;
                self.reference_state = self.state;
                self.reference_spine = Some(self.spine.clone());
                self.phase = Phase::Reference(ReferenceDefinitionScanner::new(
                    self.revision,
                    self.source_end,
                    self.origin + self.state.ix,
                )?);
            }
            Ready::ReferenceAccepted => unreachable!("reference事实在提交完成的统一发布点取出"),
            Ready::PublishTable => unreachable!("table事实在提交完成的统一发布点取出"),
            Ready::PublishInlineBlock => unreachable!("inline块事实在提交完成的统一发布点取出"),
            Ready::PublishLeafBlock => unreachable!("leaf事实在提交完成的统一发布点取出"),
            Ready::Finish => {
                // 每step只关闭一层；源结束不允许一次while同步销毁任意深度spine。
                if self.spine.is_empty() {
                    self.phase = Phase::Closed;
                } else if matches!(self.mode, Mode::Paragraph) {
                    // EOF也关闭真实paragraph；不能让通用pop把尚未移交的块事实静默丢掉。
                    self.close_inline_block(InlineBlockKind::Paragraph, self.position.byte)?;
                    self.spine.pop()?;
                    self.mode = Mode::Block;
                    self.phase = Phase::Ready(Ready::PublishInlineBlock);
                } else if matches!(self.mode, Mode::Opaque(_)) {
                    // EOF 要闭合当前真实 leaf，不能被通用容器 pop 静默丢弃。
                    let reason = if matches!(self.mode, Mode::Opaque(Opaque::Html(probe)) if probe.is_found())
                    {
                        LeafBlockCloseReason::HtmlTerminator
                    } else {
                        LeafBlockCloseReason::TrueEof
                    };
                    self.end_opaque(reason, self.opaque_semantic_end()?, self.position.byte)?;
                } else {
                    self.spine.pop()?;
                    self.phase = Phase::Ready(Ready::Finish);
                }
            }
        }
        Ok(())
    }

    fn prefix_complete(
        &mut self,
        purpose: PrefixUse,
        state: LineStartState,
        matched: usize,
    ) -> Result<(), GlobalFrontierError> {
        self.state = state;
        self.matched = matched;
        match purpose {
            PrefixUse::Block => self.phase = Phase::Ready(Ready::PopTo(matched)),
            PrefixUse::Paragraph | PrefixUse::Reference => {
                self.operation(LineStartOperation::Space(4), OpAfter::ParagraphIndent)?
            }
            PrefixUse::Separator => {
                if matched != self.spine.len() {
                    self.reject_separator();
                } else {
                    self.operation(LineStartOperation::Space(4), OpAfter::SeparatorIndent)?;
                }
            }
            PrefixUse::ReferenceLine(purpose) => {
                self.reference_line = Some(purpose);
                self.operation(LineStartOperation::Space(4), OpAfter::ParagraphIndent)?;
            }
            PrefixUse::Opaque => {
                if matched != self.spine.len() {
                    self.end_opaque(
                        LeafBlockCloseReason::ContainerExit,
                        self.opaque_semantic_end()?,
                        self.position.byte,
                    )?;
                    return Ok(());
                }
                match self.mode {
                    Mode::Opaque(Opaque::Html(probe)) if probe.is_found() => self.end_opaque(
                        LeafBlockCloseReason::HtmlTerminator,
                        self.position.byte,
                        self.position.byte,
                    )?,
                    Mode::Opaque(Opaque::Html(_)) if self.origin + state.ix == self.source_end => {
                        // 原 HTML item 可包含真实 EOF 前的最后容器 prefix，返回 cursor 仍是前一行末。
                        self.end_opaque(
                            LeafBlockCloseReason::TrueEof,
                            self.source_end,
                            self.position.byte,
                        )?;
                    }
                    Mode::Opaque(Opaque::Fence { indent, .. }) => {
                        self.operation(LineStartOperation::Space(indent), OpAfter::OpaqueIndent)?
                    }
                    Mode::Opaque(Opaque::Indented) => {
                        self.operation(LineStartOperation::Space(4), OpAfter::OpaqueIndent)?
                    }
                    Mode::Opaque(Opaque::HtmlBlank) => {
                        // 原 is_at_eol 优先于 scan_blank_line，必须保留这两个不同的 source_end 出口。
                        self.operation(LineStartOperation::AtEol, OpAfter::HtmlBlankEol)?;
                    }
                    Mode::Opaque(Opaque::Metadata { marker }) => {
                        // metadata关闭检查针对raw行起点，不能先去掉quote/list prefix再伪称原calc_indent==0。
                        self.opaque_body_state = self.state;
                        self.leaf_block
                            .as_mut()
                            .ok_or(GlobalFrontierError::InvalidFrame)?
                            .begin_closing(self.origin);
                        self.phase = Phase::Closing {
                            probe: CloseProbe::Metadata(crate::metadata_scan::MetadataCloser::new(
                                marker,
                            )),
                            start: self.origin,
                            next: self.origin,
                        };
                    }
                    _ => self.phase = Phase::Ready(Ready::ReadOpaque),
                }
            }
        }
        Ok(())
    }

    fn operation_complete(
        &mut self,
        after: OpAfter,
        value: LineStartValue,
    ) -> Result<(), GlobalFrontierError> {
        match (after, value) {
            (OpAfter::OpeningSpace, LineStartValue::SpaceRemaining(remaining)) => {
                if remaining == 0 {
                    self.state = self.container_saved;
                    self.phase = Phase::Ready(Ready::BlockBlank);
                } else {
                    self.outer_indent = 4 - remaining;
                    self.phase = Phase::Ready(Ready::TryFootnote);
                }
            }
            // 同marker复用已有list；新item的indent来自共享真实tab算法，不按字符数猜层级。
            (OpAfter::OpeningList, LineStartValue::List(Some((marker, _index, indent)))) => {
                let start = self.origin + self.container_saved.ix;
                let new = crate::container_state::prepare_list(self, start, marker);
                self.check_spine_error()?;
                if new {
                    self.spine.push(StructuralEntry {
                        kind: StructuralKind::List {
                            marker,
                            tight: true,
                        },
                        source_start: start,
                    })?;
                }
                self.spine.push(StructuralEntry {
                    kind: StructuralKind::Item { indent },
                    source_start: start,
                })?;
                self.saved = self.state;
                self.operation(LineStartOperation::BlankLine, OpAfter::ListBlankEol)?;
            }
            (OpAfter::OpeningList, LineStartValue::List(None)) => {
                self.phase = Phase::Ready(Ready::TryQuote)
            }
            // quote关闭悬空list后才push；GFM提示标签另做真实整行blank验证，失败仍保普通quote正文。
            (OpAfter::OpeningQuote, LineStartValue::Bool(true)) => {
                self.finish_list_checked(self.origin)?;
                self.spine.push(StructuralEntry::for_role(
                    crate::block_context::ContainerRole::Quote,
                    // 只使用共享 Quote 扫描成功前的真实字节位置；成功后的状态另存给 GFM 标签失败恢复。
                    self.origin + self.saved.ix,
                ))?;
                self.saved = self.state;
                if self.options.contains(Options::ENABLE_GFM) {
                    self.peek(self.origin + self.state.ix, 12, 4);
                } else {
                    self.phase = Phase::Ready(Ready::Opening);
                }
            }
            (OpAfter::OpeningQuote, LineStartValue::Bool(false)) => {
                // 候选失败仍恢复本轮容器探测前的完整 tab 状态，不把仅供锚点的 save 当成回退位置。
                self.state = self.container_saved;
                self.phase = Phase::Ready(Ready::BlockBlank);
            }
            (OpAfter::ListBlankEol, LineStartValue::BlankLine(Some(_))) => {
                // 完整 blank 原语已经给出包含真实 EOL 的原 cursor，不再读取后继行补齐。
                let end = self.origin + self.state.ix;
                self.begin_list_item = Some(end);
                self.commit(end, AfterCommit::NewBlock);
            }
            (OpAfter::ListBlankEol, LineStartValue::BlankLine(None)) => {
                self.state = self.saved;
                if self.options.contains(Options::ENABLE_TASKLISTS) {
                    self.operation(LineStartOperation::Space(3), OpAfter::TaskSpace)?;
                } else {
                    self.phase = Phase::Ready(Ready::Opening);
                }
            }
            (OpAfter::TaskSpace, LineStartValue::SpaceRemaining(_)) => {
                self.peek(self.origin + self.state.ix, 4, 3)
            }
            (OpAfter::QuoteTagSpace, LineStartValue::Unit) => {
                self.operation(LineStartOperation::AtEol, OpAfter::QuoteTagEol)?
            }
            (OpAfter::QuoteTagEol, LineStartValue::Bool(true)) => {
                self.mode = Mode::QuoteContinuation;
                self.read_line(self.origin + self.state.ix, false, LineUse::ReferenceTail);
            }
            (OpAfter::QuoteTagEol, LineStartValue::Bool(false)) => {
                self.state = self.saved;
                self.phase = Phase::Ready(Ready::Opening);
            }
            // 空quote不把父list置loose；刚开启的空item降indent为零，防止后续块被错误嵌套。
            (OpAfter::BlockBlankEol, LineStartValue::BlankLine(Some(_))) => {
                match self.spine.entry(self.spine.len().saturating_sub(1)) {
                    Some(StructuralEntry {
                        kind: StructuralKind::Quote,
                        ..
                    }) => (),
                    Some(
                        mut entry @ StructuralEntry {
                            kind: StructuralKind::Item { .. },
                            ..
                        },
                    ) if self.begin_list_item.is_some() => {
                        self.last_line_blank = true;
                        entry.kind = StructuralKind::Item { indent: 0 };
                        self.spine.update_near_top(0, entry)?;
                    }
                    _ => self.last_line_blank = true,
                }
                self.commit(self.origin + self.state.ix, AfterCommit::NewBlock);
            }
            (OpAfter::BlockBlankEol, LineStartValue::BlankLine(None)) => {
                self.state = self.saved;
                self.operation(LineStartOperation::Space(4), OpAfter::BlockIndent)?;
            }
            (OpAfter::BlockIndent, LineStartValue::SpaceRemaining(remaining)) => {
                self.indent = 4 - remaining;
                if remaining == 0 {
                    self.start_opaque(
                        Opaque::Indented,
                        PendingLeafKind::Indented {
                            remaining: self.state.spaces_remaining,
                        },
                    )?;
                } else {
                    self.phase = Phase::Ready(Ready::Leaf);
                }
            }
            // 四格缩进不触发paragraph中断；不足四格才检查setext、块opening及heavy-table两行证据。
            (OpAfter::ParagraphIndent, LineStartValue::SpaceRemaining(remaining)) => {
                if remaining == 0 {
                    if self.reference_line.is_some() {
                        self.allow_reference_line()?;
                    } else {
                        self.phase = Phase::Ready(Ready::ContinueParagraph);
                    }
                } else {
                    self.phase = Phase::Ready(Ready::Interrupt);
                }
            }
            (OpAfter::ParagraphSpace, LineStartValue::Unit) => {
                self.operation(LineStartOperation::AtEol, OpAfter::ParagraphBlank)?
            }
            (OpAfter::ParagraphBlank, LineStartValue::Bool(true)) => {
                if self.reference_line.is_some() {
                    self.reject_reference_line()?;
                } else if matches!(self.mode, Mode::TaskContinuation) {
                    self.read_line(self.origin + self.state.ix, false, LineUse::EmptyItemBlank);
                } else {
                    self.phase = Phase::Ready(Ready::EndParagraph);
                }
            }
            (OpAfter::ParagraphBlank, LineStartValue::Bool(false)) => {
                if self.reference_line.is_some() {
                    self.allow_reference_line()?;
                } else if matches!(self.mode, Mode::TaskContinuation) {
                    self.phase = Phase::Ready(Ready::ParagraphStart);
                } else if matches!(self.mode, Mode::ReferenceContinuation) {
                    self.phase = Phase::Ready(Ready::ReferenceStart);
                } else if matches!(self.mode, Mode::QuoteContinuation) {
                    self.mode = Mode::Block;
                    self.phase = Phase::Ready(Ready::BlockBlank);
                } else {
                    self.phase = Phase::Ready(Ready::ParagraphLine(false));
                }
            }
            (OpAfter::SeparatorIndent, LineStartValue::SpaceRemaining(0)) => {
                self.reject_separator()
            }
            (OpAfter::SeparatorIndent, LineStartValue::SpaceRemaining(_)) => {
                self.read_line(self.origin + self.state.ix, false, LineUse::Separator)
            }
            (OpAfter::ReferenceTailSpace, LineStartValue::Unit) => {
                self.operation(LineStartOperation::AtEol, OpAfter::ReferenceTailEol)?
            }
            (OpAfter::ReferenceTailEol, LineStartValue::Bool(true)) => {
                self.mode = Mode::ReferenceContinuation;
                self.read_line(self.origin + self.state.ix, false, LineUse::ReferenceTail);
            }
            (OpAfter::ReferenceTailEol, LineStartValue::Bool(false)) => {
                self.finish_list_checked(self.origin)?;
                self.mode = Mode::Block;
                self.phase = Phase::Ready(Ready::Begin);
            }
            // 已核closing colon才建立footnote；GFM正文另起LineStart origin，保原tab列重置语义。
            (OpAfter::FootnoteSpace, LineStartValue::Unit) => {
                let start = self.origin + self.saved.ix;
                self.finish_list_checked(start)?;
                if matches!(
                    self.spine
                        .entry(self.spine.len().saturating_sub(1))
                        .map(|e| e.kind),
                    Some(StructuralKind::Footnote { .. })
                ) {
                    self.spine.pop()?;
                }
                // 同一原 scanner 已确认 closing colon 并消费其后真实空白；首行 label 字面
                // enclosure 到此为止。不能让后续语义消费者重扫数字、标点或猜祖先 opening。
                let body_start = self.origin + self.state.ix;
                self.spine.push(StructuralEntry {
                    kind: StructuralKind::Footnote {
                        body_start: std::num::NonZeroUsize::new(body_start),
                    },
                    source_start: start,
                })?;
                self.origin += self.state.ix;
                self.state = LineStartState::default();
                self.phase = Phase::Ready(Ready::Opening);
            }
            (OpAfter::OpaqueIndent, LineStartValue::SpaceRemaining(remaining)) => match self.mode {
                Mode::Opaque(Opaque::Fence { indent, .. }) => {
                    self.opaque_body_state = self.state;
                    self.operation(
                        LineStartOperation::Space(4 - indent),
                        OpAfter::OpaqueFenceExtra,
                    )?;
                }
                Mode::Opaque(Opaque::Indented) if remaining == 0 => {
                    if self.origin + self.state.ix == self.source_end {
                        self.end_opaque(
                            LeafBlockCloseReason::TrueEof,
                            self.opaque_semantic_end()?,
                            self.origin,
                        )?;
                    } else {
                        self.phase = Phase::Ready(Ready::ReadOpaque)
                    }
                }
                Mode::Opaque(Opaque::Indented) => {
                    self.operation(LineStartOperation::AtEol, OpAfter::OpaqueBlankEol)?
                }
                _ => unreachable!(),
            },
            (OpAfter::OpaqueFenceExtra, LineStartValue::SpaceRemaining(0)) => {
                self.state = self.opaque_body_state;
                self.phase = Phase::Ready(Ready::ReadOpaque);
            }
            (OpAfter::OpaqueFenceExtra, LineStartValue::SpaceRemaining(_)) => {
                let Mode::Opaque(Opaque::Fence { marker, count, .. }) = self.mode else {
                    unreachable!()
                };
                let start = self.origin + self.state.ix;
                self.leaf_block
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .begin_closing(start);
                self.phase = Phase::Closing {
                    probe: CloseProbe::Fence(CodeFenceCloser::new(marker, count)),
                    start,
                    next: start,
                };
            }
            (OpAfter::OpaqueBlankEol, LineStartValue::Bool(at_eol)) => {
                // indented code可跨空行，HTML6/7恰在空行结束；只合并等价动作，不合并两种语法权限。
                if matches!(self.mode, Mode::Opaque(Opaque::Indented))
                    && at_eol
                    && self.origin + self.state.ix == self.source_end
                {
                    // 原 indented 函数先检查 next_line_ix==EOF，再决定消费该尾 prefix；返回旧物理行末。
                    self.end_opaque(
                        LeafBlockCloseReason::TrueEof,
                        self.opaque_semantic_end()?,
                        self.origin,
                    )?;
                } else if matches!(self.mode, Mode::Opaque(Opaque::Indented)) == at_eol {
                    self.phase = Phase::Ready(Ready::ReadOpaque);
                } else {
                    let reason = if matches!(self.mode, Mode::Opaque(Opaque::Indented)) {
                        LeafBlockCloseReason::IndentBreak
                    } else {
                        LeafBlockCloseReason::BlankLine
                    };
                    self.end_opaque(reason, self.opaque_semantic_end()?, self.position.byte)?;
                }
            }
            (OpAfter::HtmlBlankEol, LineStartValue::Bool(true)) => {
                self.end_opaque(
                    LeafBlockCloseReason::BlankLine,
                    self.position.byte,
                    self.position.byte,
                )?;
            }
            (OpAfter::HtmlBlankEol, LineStartValue::Bool(false)) => {
                self.opaque_body_state = self.state;
                self.operation(LineStartOperation::BlankLine, OpAfter::HtmlBlankScan)?;
            }
            (OpAfter::HtmlBlankScan, LineStartValue::BlankLine(Some(_))) => {
                // 原 scan_blank_line 成功时 item 范围止于 prefix 末，原返回 cursor 仍在前行末。
                self.end_opaque(
                    LeafBlockCloseReason::BlankLine,
                    self.origin + self.opaque_body_state.ix,
                    self.position.byte,
                )?;
            }
            (OpAfter::HtmlBlankScan, LineStartValue::BlankLine(None)) => {
                self.phase = Phase::Ready(Ready::ReadOpaque);
            }
            // paragraph内只有非空bullet/编号1可以中断；容器失配时仍沿原更宽中断资格。
            (OpAfter::InterruptList, LineStartValue::List(Some(marker))) => {
                if self.matched != self.spine.len()
                    || matches!(marker.0, b'*' | b'-' | b'+')
                    || marker.1 == 1
                {
                    if self.matched != self.spine.len() {
                        self.interrupt(true)?;
                    } else {
                        self.operation(LineStartOperation::AllSpace, OpAfter::InterruptListSpace)?;
                    }
                } else {
                    self.state = self.saved;
                    self.operation(LineStartOperation::Quote, OpAfter::InterruptQuote)?;
                }
            }
            (OpAfter::InterruptList, LineStartValue::List(None)) => {
                self.state = self.saved;
                self.operation(LineStartOperation::Quote, OpAfter::InterruptQuote)?;
            }
            (OpAfter::InterruptListSpace, LineStartValue::Unit) => {
                self.operation(LineStartOperation::AtEol, OpAfter::InterruptListEol)?
            }
            (OpAfter::InterruptListEol, LineStartValue::Bool(blank)) => {
                if !blank {
                    self.interrupt(true)?;
                } else {
                    self.state = self.saved;
                    self.operation(LineStartOperation::Quote, OpAfter::InterruptQuote)?;
                }
            }
            (OpAfter::InterruptQuote, LineStartValue::Bool(found)) => {
                self.state = self.saved;
                if found {
                    self.interrupt(true)?;
                } else if self.options.contains(Options::ENABLE_FOOTNOTES) {
                    self.peek(self.origin + self.state.ix, 2, 8);
                } else {
                    self.interrupt_other()?;
                }
            }
            _ => unreachable!("内部operation返回类型与其固定续点一致"),
        }
        Ok(())
    }

    fn leaf_complete(
        &mut self,
        leaf: LeafStart,
        purpose: LeafUse,
        html_type7: bool,
    ) -> Result<(), GlobalFrontierError> {
        if matches!(purpose, LeafUse::Interrupt) {
            let interrupts = matches!(
                leaf,
                LeafStart::HtmlTerminated(_)
                    | LeafStart::Rule(_)
                    | LeafStart::Heading(_)
                    | LeafStart::Fence { .. }
            ) || (matches!(leaf, LeafStart::HtmlBlankTerminated) && !html_type7);
            if interrupts {
                self.interrupt(true)?;
            } else {
                self.saved = self.state;
                self.operation(LineStartOperation::List(0), OpAfter::InterruptList)?;
            }
            return Ok(());
        }
        match leaf {
            LeafStart::HtmlTerminated(end) => self.start_opaque(
                Opaque::Html(HtmlTerminator::new(end)),
                PendingLeafKind::Html(HtmlLeafTermination::Terminator(end)),
            )?,
            LeafStart::HtmlBlankTerminated => self.start_opaque(
                Opaque::HtmlBlank,
                PendingLeafKind::Html(HtmlLeafTermination::BlankLine { type7: html_type7 }),
            )?,
            LeafStart::Fence { marker, count } => self.start_opaque(
                Opaque::Fence {
                    marker,
                    count,
                    indent: self.indent,
                },
                PendingLeafKind::Fence {
                    marker,
                    count,
                    indent: self.indent,
                },
            )?,
            LeafStart::Rule(count) => {
                self.finish_list_checked(self.origin)?;
                let start = self.origin + self.state.ix;
                self.begin_leaf(PendingLeafKind::Rule, start)?;
                self.close_leaf(
                    start + count,
                    start + count,
                    LeafBlockCloseReason::CompleteLine,
                )?;
                self.commit(start + count, AfterCommit::EmitLeafBlock);
            }
            LeafStart::Heading(level) => {
                self.finish_list_checked(self.origin)?;
                self.begin_leaf(PendingLeafKind::Atx(level), self.origin + self.state.ix)?;
                self.read_line(self.origin + self.state.ix, false, LineUse::SimpleBlock);
            }
            LeafStart::DefinitionOrParagraph => self.phase = Phase::Ready(Ready::ReferenceStart),
        }
        Ok(())
    }
}

impl GlobalFrontier {
    fn peek_complete(
        &mut self,
        start: usize,
        bytes: &[u8],
        kind: u8,
    ) -> Result<(), GlobalFrontierError> {
        match kind {
            1 => {
                if bytes == b"[^" {
                    self.phase = Phase::Footnote {
                        scanner: LabelScanner::new(false),
                        start,
                        next: start + 2,
                    };
                } else {
                    self.phase = Phase::Ready(Ready::TryList);
                }
            }
            2 => {
                if bytes.first() == Some(&b':') {
                    let opening = self.origin + self.saved.ix;
                    self.footnote_fact = Some(VerifiedGlobalFootnoteDefinition {
                        revision: self.revision,
                        source_end: self.source_end,
                        source: opening..start + 1,
                        reference: opening + 2..start,
                    });
                    self.state.ix += 1;
                    self.operation(LineStartOperation::AllSpace, OpAfter::FootnoteSpace)?;
                } else {
                    self.state = self.saved;
                    self.phase = Phase::Ready(Ready::TryList);
                }
            }
            3 => {
                let valid = bytes.len() == 4
                    && bytes[0] == b'['
                    && bytes[2] == b']'
                    && (crate::scanners::is_ascii_whitespace_no_nl(bytes[1])
                        || matches!(bytes[1], b'x' | b'X'))
                    && crate::scanners::is_ascii_whitespace(bytes[3]);
                if valid {
                    self.state.ix += 3;
                    self.operation(LineStartOperation::AllSpace, OpAfter::ParagraphSpace)?;
                    self.mode = Mode::TaskContinuation;
                } else {
                    self.state = self.saved;
                    self.phase = Phase::Ready(Ready::Opening);
                }
            }
            4 => {
                let tag = [
                    b"[!NOTE]".as_slice(),
                    b"[!TIP]",
                    b"[!IMPORTANT]",
                    b"[!WARNING]",
                    b"[!CAUTION]",
                ]
                .into_iter()
                .find(|tag| {
                    bytes.len() >= tag.len() && bytes[..tag.len()].eq_ignore_ascii_case(tag)
                });
                if let Some(tag) = tag {
                    self.state.ix += tag.len();
                    self.operation(LineStartOperation::AllSpace, OpAfter::QuoteTagSpace)?;
                } else {
                    self.state = self.saved;
                    self.phase = Phase::Ready(Ready::Opening);
                }
            }
            6 => {
                let marker = bytes.first().copied();
                let enabled = marker == Some(b'-')
                    && self
                        .options
                        .contains(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS)
                    || marker == Some(b'+')
                        && self
                            .options
                            .contains(Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS);
                if enabled {
                    self.metadata_start = start;
                    self.read_line(start, true, LineUse::MetadataOpening);
                } else {
                    self.phase = Phase::Leaf(LeafProbe::new(start), LeafUse::Block);
                }
            }
            7 => {
                self.prefix_last = bytes.first().copied();
                self.finish_reference_line(Some(self.state.ix))?;
            }
            8 => {
                if bytes == b"[^" {
                    self.footnote_interrupt = true;
                    self.phase = Phase::Footnote {
                        scanner: LabelScanner::new(false),
                        start,
                        next: start + 2,
                    };
                } else {
                    self.interrupt_other()?;
                }
            }
            9 => {
                self.footnote_interrupt = false;
                self.state = self.saved;
                if bytes.first() == Some(&b':') {
                    self.interrupt(true)?;
                } else {
                    self.interrupt_other()?;
                }
            }
            _ => unreachable!("固定peek任务只由内部已知入口建立"),
        }
        Ok(())
    }

    fn line_complete(
        &mut self,
        line: LineFacts,
        purpose: LineUse,
    ) -> Result<(), GlobalFrontierError> {
        match purpose {
            LineUse::Paragraph { first } => {
                self.inline_block
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .last_line = Some(line);
                if first
                    && line.header_columns > 0
                    && !line.hard_break
                    && line.end < self.source_end
                    && self.options.contains(Options::ENABLE_TABLES)
                {
                    self.header = Some(line);
                    self.header_line = self.position.line;
                    self.header_physical_start = self.position.byte;
                    self.header_interrupt = false;
                    self.begin_separator(line.end)?;
                } else {
                    self.commit(line.end, AfterCommit::ParagraphNext);
                }
            }
            // delimiter的列数必须与完整首行计数相同；失败只撤候选，不消费其lookahead源行。
            LineUse::Separator => {
                if line.delimiter_columns == self.header.map(|header| header.header_columns) {
                    if self.header_interrupt {
                        // heavy-table只证明旧paragraph应结束；仍从该真实源行重新经过block容器入口。
                        self.header_interrupt = false;
                        if self.reference_line.is_some() {
                            self.reject_reference_line()?;
                        } else {
                            self.phase = Phase::Ready(Ready::EndParagraph);
                        }
                    } else {
                        self.separator = Some(line);
                        self.commit(line.end, AfterCommit::TableReady);
                    }
                } else {
                    self.reject_separator();
                }
            }
            LineUse::SimpleBlock => {
                self.leaf_block
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .finish_line(line);
                self.close_leaf(line.end, line.end, LeafBlockCloseReason::CompleteLine)?;
                self.commit(line.end, AfterCommit::EmitLeafBlock);
            }
            LineUse::Opaque => {
                self.leaf_block
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .finish_line(line);
                self.commit(line.end, AfterCommit::OpaqueNext);
            }
            LineUse::EmptyItemBlank => {
                // 只有新marker后立即空白才是空item；已含正文的item中间空行不能重新获得begin标记。
                self.begin_list_item = Some(line.end);
                self.commit(line.end, AfterCommit::NewBlock);
            }
            LineUse::Setext => {
                if matches!(self.mode, Mode::Paragraph) {
                    // Setext是最终heading，不先签发旧Paragraph；正文扫描范围止于underline之前。
                    let level = line.setext_level.ok_or(GlobalFrontierError::InvalidFrame)?;
                    self.close_inline_block(InlineBlockKind::SetextHeading(level), line.end)?;
                    self.spine.pop()?;
                    self.commit(line.end, AfterCommit::EmitInlineBlock);
                } else {
                    self.commit(line.end, AfterCommit::NewBlock);
                }
            }
            LineUse::ReferenceTail => self.commit(line.end, AfterCommit::ReferenceNext),
            // setext对label续行与空白/title续行的限制不同，必须保留原purpose而非统一拒绝。
            LineUse::InterruptCheck => {
                self.interrupt_line = Some(line);
                let setext_reject =
                    self.reference_line
                        .map_or(self.matched == self.spine.len(), |purpose| {
                            purpose != ReferenceLinePurpose::Label
                                || self.matched == self.spine.len()
                        });
                if line.start == line.content_end {
                    self.interrupt(true)?;
                } else if line.setext
                    && setext_reject
                    && (self.reference_line.is_some() || matches!(self.mode, Mode::Paragraph))
                {
                    // 原parse_setext仅属于真正parse_paragraph；refdef/GFM quote/task的虚拟
                    // continuation只用scan_paragraph_interrupt，不能把单独'-'改成新空list。
                    if self.reference_line.is_some() {
                        self.reject_reference_line()?;
                    } else if matches!(self.mode, Mode::Paragraph) {
                        self.read_line(line.start, false, LineUse::Setext);
                    } else {
                        self.phase = Phase::Ready(Ready::EndParagraph);
                    }
                } else {
                    self.phase = Phase::Leaf(LeafProbe::new(line.start), LeafUse::Interrupt);
                }
            }
            LineUse::MetadataOpening => {
                if let Some(marker) = line.metadata_marker {
                    self.metadata_marker = marker;
                    self.phase = Phase::Metadata(PendingMetadataValidation::new(
                        self.revision,
                        self.source_end,
                        line.start,
                        line.end,
                        marker,
                    )?);
                } else {
                    self.phase = Phase::Leaf(LeafProbe::new(line.start), LeafUse::Block);
                }
            }
            LineUse::Closing => {
                let semantic_end = self
                    .leaf_block
                    .as_ref()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .closing_end()
                    .unwrap_or(self.source_end);
                let reason = if semantic_end == self.source_end
                    && self
                        .leaf_block
                        .as_ref()
                        .and_then(PendingGlobalLeaf::closing_end)
                        .is_none()
                {
                    LeafBlockCloseReason::TrueEof
                } else {
                    LeafBlockCloseReason::ExplicitMarker
                };
                self.close_leaf(semantic_end, line.end, reason)?;
                self.spine.pop()?;
                self.commit(line.end, AfterCommit::EmitLeafBlock);
            }
        }
        Ok(())
    }

    fn begin_separator(&mut self, start: usize) -> Result<(), GlobalFrontierError> {
        self.header_origin = self.origin;
        self.header_state = self.state;
        self.origin = start;
        self.state = LineStartState::default();
        self.start_prefix(start, PrefixUse::Separator)
    }
    fn reject_separator(&mut self) {
        if self.header_interrupt {
            self.header_interrupt = false;
            self.origin = self.header_origin;
            self.state = self.header_state;
            // 不合法heavy table仍是同一paragraph正文；不能把前瞻分隔行吞掉或误作新段首。
            self.phase = Phase::Ready(Ready::ContinueParagraph);
        } else {
            self.commit(self.header.unwrap().end, AfterCommit::ParagraphNext);
        }
    }
    fn commit_complete(&mut self, after: AfterCommit) -> Result<(), GlobalFrontierError> {
        // 只有完整目标才是语法分类边界；TableReady在本advance尾部原子发布真实H/D事实。
        // position在分片Commit内部可先行，不能将它直接暴露为无表覆盖。
        self.classified = (self.position.byte, self.position.utf16);
        self.phase = match after {
            AfterCommit::NewBlock => {
                self.mode = Mode::Block;
                Phase::Ready(Ready::Begin)
            }
            AfterCommit::ParagraphNext => {
                self.mode = Mode::Paragraph;
                Phase::Ready(Ready::Begin)
            }
            AfterCommit::ReferenceNext => Phase::Ready(Ready::Begin),
            AfterCommit::OpaqueNext => Phase::Ready(Ready::Begin),
            AfterCommit::TableReady => Phase::Ready(Ready::PublishTable),
            AfterCommit::EmitReference => Phase::Ready(Ready::ReferenceAccepted),
            AfterCommit::EmitInlineBlock => Phase::Ready(Ready::PublishInlineBlock),
            AfterCommit::EmitLeafBlock => Phase::Ready(Ready::PublishLeafBlock),
        };
        Ok(())
    }
    fn start_opaque(
        &mut self,
        opaque: Opaque,
        kind: PendingLeafKind,
    ) -> Result<(), GlobalFrontierError> {
        self.finish_list_checked(self.origin)?;
        self.begin_leaf(kind, self.origin + self.state.ix)?;
        self.spine.push(StructuralEntry {
            kind: StructuralKind::Other,
            source_start: self.origin + self.state.ix,
        })?;
        self.mode = Mode::Opaque(opaque);
        self.read_line(self.origin + self.state.ix, true, LineUse::Opaque);
        Ok(())
    }
    fn end_opaque(
        &mut self,
        reason: LeafBlockCloseReason,
        semantic_end: usize,
        consumed_end: usize,
    ) -> Result<(), GlobalFrontierError> {
        self.close_leaf(semantic_end, consumed_end, reason)?;
        self.spine.pop()?;
        self.mode = Mode::Block;
        // 只提交原 parser cursor。HTML 的 semantic enclosure 可含后继 prefix，但那不是无表 gap 权限。
        // prefix 已由真实原语前瞻验证；不得为包住 source_range 而越过后继 Table 的物理行起点。
        self.commit(self.position.byte, AfterCommit::EmitLeafBlock);
        Ok(())
    }

    fn begin_leaf(
        &mut self,
        kind: PendingLeafKind,
        start: usize,
    ) -> Result<(), GlobalFrontierError> {
        if self.leaf_block.is_some() || self.leaf_fact.is_some() {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        self.leaf_block = Some(PendingGlobalLeaf::new(
            self.revision,
            self.source_end,
            self.options,
            kind,
            start,
            (self.position.byte, self.origin, self.state),
            self.spine.clone(),
        ));
        Ok(())
    }

    fn close_leaf(
        &mut self,
        semantic_end: usize,
        consumed_end: usize,
        reason: LeafBlockCloseReason,
    ) -> Result<(), GlobalFrontierError> {
        if self.leaf_fact.is_some() {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let leaf = self
            .leaf_block
            .take()
            .ok_or(GlobalFrontierError::InvalidFrame)?;
        self.leaf_fact = Some(
            leaf.finish(semantic_end, consumed_end, reason)
                .ok_or(GlobalFrontierError::InvalidFrame)?,
        );
        Ok(())
    }

    fn opaque_semantic_end(&self) -> Result<usize, GlobalFrontierError> {
        let leaf = self
            .leaf_block
            .as_ref()
            .ok_or(GlobalFrontierError::InvalidFrame)?;
        Ok(if matches!(self.mode, Mode::Opaque(Opaque::Indented)) {
            leaf.last_nonblank_end()
        } else {
            leaf.body_end()
        })
    }
    fn interrupt(&mut self, yes: bool) -> Result<(), GlobalFrontierError> {
        if self.reference_line.is_some() {
            if yes {
                self.reject_reference_line()?;
            } else if self.reference_line == Some(ReferenceLinePurpose::Title) {
                self.phase = Phase::Ready(Ready::ContinueParagraph);
            } else {
                self.allow_reference_line()?;
            }
        } else if yes {
            self.phase = Phase::Ready(Ready::EndParagraph);
        } else {
            self.phase = Phase::Ready(Ready::ContinueParagraph);
        }
        Ok(())
    }
    fn interrupt_other(&mut self) -> Result<(), GlobalFrontierError> {
        let header = self
            .interrupt_line
            .ok_or(GlobalFrontierError::InvalidFrame)?;
        if header.starts_pipe
            && header.header_columns > 0
            && self.options.contains(Options::ENABLE_TABLES)
        {
            self.header = Some(header);
            self.header_interrupt = true;
            self.begin_separator(header.end)?;
        } else {
            self.interrupt(false)?;
        }
        Ok(())
    }

    fn reference_step(
        &mut self,
        step: ReferenceStep,
        scanner: ReferenceDefinitionScanner,
        work: &mut GlobalWork,
        need_input: &mut bool,
    ) -> Result<(), GlobalFrontierError> {
        let consumed = match step {
            ReferenceStep::Progress { consumed }
            | ReferenceStep::NeedInput { consumed }
            | ReferenceStep::NeedLine { consumed, .. }
            | ReferenceStep::Candidate { consumed, .. }
            | ReferenceStep::Rejected { consumed, .. } => consumed,
        };
        // 上游refdef叶扫描的固定内部转换仍另有上界；其lookahead消耗不改变semantic committed_byte。
        *work = GlobalWork {
            byte_reads: consumed,
            transitions: consumed.saturating_mul(4).max(1),
        };
        match step {
            ReferenceStep::Progress { .. } | ReferenceStep::NeedInput { .. } => {
                *need_input = matches!(step, ReferenceStep::NeedInput { .. });
                self.phase = Phase::Reference(scanner);
            }
            ReferenceStep::Rejected { .. } => {
                self.origin = self.reference_origin;
                self.state = self.reference_state;
                self.reference_spine = None;
                self.phase = Phase::Ready(Ready::ParagraphStart);
            }
            // 先保存候选起点spine，再仅提交candidate.span_end；读过但不属于backup span的尾部仍需重分类。
            ReferenceStep::Candidate { candidate, .. } => {
                self.reference_fact = Some(VerifiedGlobalReference {
                    candidate,
                    spine: self
                        .reference_spine
                        .take()
                        .ok_or(GlobalFrontierError::InvalidFrame)?,
                    options: self.options,
                    source_end: self.source_end,
                });
                self.commit(candidate.span_end, AfterCommit::EmitReference);
            }
            ReferenceStep::NeedLine {
                after_eol, purpose, ..
            } => {
                self.reference = Some(scanner);
                self.origin = after_eol;
                self.state = LineStartState::default();
                self.start_prefix(after_eol, PrefixUse::ReferenceLine(purpose))?;
            }
        }
        Ok(())
    }
    fn allow_reference_line(&mut self) -> Result<(), GlobalFrontierError> {
        if self.state.ix == 0 {
            self.finish_reference_line(Some(0))?;
        } else {
            self.peek(self.origin + self.state.ix - 1, 1, 7);
        }
        Ok(())
    }
    fn reject_reference_line(&mut self) -> Result<(), GlobalFrontierError> {
        self.finish_reference_line(None)
    }
    fn finish_reference_line(&mut self, skip: Option<usize>) -> Result<(), GlobalFrontierError> {
        let mut scanner = self
            .reference
            .take()
            .ok_or(GlobalFrontierError::InvalidFrame)?;
        scanner.continue_line(
            skip.map(|skip| (skip, if skip == 0 { None } else { self.prefix_last })),
        )?;
        self.reference_line = None;
        self.prefix_last = None;
        self.phase = Phase::Reference(scanner);
        Ok(())
    }
    fn close_inline_block(
        &mut self,
        kind: InlineBlockKind,
        end: usize,
    ) -> Result<(), GlobalFrontierError> {
        let pending = self
            .inline_block
            .take()
            .ok_or(GlobalFrontierError::InvalidFrame)?;
        let last = pending.last_line.ok_or(GlobalFrontierError::InvalidFrame)?;
        if pending.physical_start > pending.start
            || pending.start > last.content_end
            || last.content_end > last.end
            || last.end > end
            || end > self.source_end
            || self.inline_fact.is_some()
        {
            return Err(GlobalFrontierError::InvalidFrame); // 失败不发布部分块或用lookahead修补越界。
        }
        self.inline_fact = Some(VerifiedGlobalInlineBlock {
            revision: self.revision,
            source_end: self.source_end,
            options: self.options,
            kind,
            start: pending.start,
            physical_start: pending.physical_start,
            first_state_origin: pending.first_state_origin,
            first_state: pending.first_state,
            end,
            lexical_end: last.end,
            last_content_end: last.content_end,
            last_line_end: last.end,
            spine: pending.spine,
        });
        Ok(())
    }

    fn start_verified_table(&mut self) -> Result<(), GlobalFrontierError> {
        // 真实H/D已经把候选改成table；没有闭合Paragraph事实可交给普通inline consumer。
        self.inline_block = None;
        let mut entry = self
            .spine
            .entry(
                self.spine
                    .len()
                    .checked_sub(1)
                    .ok_or(GlobalFrontierError::InvalidFrame)?,
            )
            .ok_or(GlobalFrontierError::InvalidFrame)?;
        if entry.kind != StructuralKind::Paragraph {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        entry.kind = StructuralKind::Table;
        self.spine.update_near_top(0, entry)?;
        let header = self
            .header
            .take()
            .ok_or(GlobalFrontierError::InvalidFrame)?;
        let separator = self.separator.ok_or(GlobalFrontierError::InvalidFrame)?;
        let verified = VerifiedGlobalTable {
            spine: self.spine.clone(),
            revision: self.revision,
            source_end: self.source_end,
            header_start: self.header_physical_start,
            separator_start: header.end,
            separator_prefix_bytes: separator.start - header.end,
            separator_terminated: separator.content_end < separator.end,
            columns: header.header_columns,
            next_byte: self.position.byte,
            next_utf16: self.position.utf16,
            next_line: self.position.line,
            options: self.options,
            header_inline: crate::block_frontier::cell::TableInlineRow {
                ordinal: 0,
                start: self.header_physical_start,
                content_start: header.start,
                content_end: header.content_end,
                end: header.end,
            },
        };
        self.table = Some(TableContinuationState::from_verified_global(verified));
        self.phase = Phase::Table;
        Ok(())
    }
    fn check_spine_error(&mut self) -> Result<(), GlobalFrontierError> {
        self.spine_error
            .take()
            .map_or(Ok(()), |error| Err(error.into()))
    }
    fn finish_list_checked(&mut self, end: usize) -> Result<(), GlobalFrontierError> {
        crate::container_state::finish_list(self, end);
        self.check_spine_error()
    }
}

impl ListState for GlobalFrontier {
    fn parent_kind(&self) -> ParentKind {
        match self
            .spine
            .entry(self.spine.len().saturating_sub(1))
            .map(|entry| entry.kind)
        {
            Some(StructuralKind::List { marker, .. }) => ParentKind::List { marker },
            Some(StructuralKind::Item { .. } | StructuralKind::Definition { .. }) => {
                ParentKind::Item
            }
            Some(_) => ParentKind::Other,
            None => ParentKind::Root,
        }
    }
    fn begin_list_item(&self) -> Option<usize> {
        self.begin_list_item
    }
    fn set_begin_list_item(&mut self, start: Option<usize>) {
        self.begin_list_item = start;
    }
    fn last_line_blank(&self) -> bool {
        self.last_line_blank
    }
    fn set_last_line_blank(&mut self, blank: bool) {
        self.last_line_blank = blank;
    }
    fn pop_container(&mut self, _end: usize) {
        if let Err(error) = self.spine.pop() {
            if self.spine_error.is_none() {
                self.spine_error = Some(error);
            }
        }
    }
    fn loosen_parent_list(&mut self) {
        self.loosen(0);
    }
    fn loosen_grandparent_list(&mut self) {
        self.loosen(1);
    }
}
impl GlobalFrontier {
    fn loosen(&mut self, distance: usize) {
        let Some(index) = self.spine.len().checked_sub(distance + 1) else {
            return;
        };
        let Some(mut entry) = self.spine.entry(index) else {
            return;
        };
        match &mut entry.kind {
            StructuralKind::List { tight, .. } => *tight = false,
            _ => return,
        }
        if let Err(error) = self.spine.update_near_top(distance, entry) {
            if self.spine_error.is_none() {
                self.spine_error = Some(error);
            }
        }
    }
}
