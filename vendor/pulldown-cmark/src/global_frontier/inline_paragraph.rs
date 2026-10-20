// Copyright 2015 Google Inc. All rights reserved.
// 原 Paragraph/Setext 的有限来源上下文；完整 MIT 条款见 LICENSE。

//! 完整短块只保原 scope 与逐行 LineStart 事实；三个 Parser consumer 借同一结果，不重扫祖先。

use std::ops::Range;

use super::{
    CapturedScopeCursor, CapturedScopeRole, CapturedScopeStep, ContainerPrefix,
    GlobalFrontierError, GlobalSourceAuthority, GlobalWork, InlineBlockKind, InlinePlainLimits,
    PrefixProgress, VerifiedInlineSemanticSeed,
};
use crate::block_frontier::SourceFrame;
use crate::line_start_stream::LineStartState;

const CONTROL: usize = 128;
const MAX_SOURCE: usize = 2048;

#[derive(Clone, Debug)]
/// 只由原 seed 的真实行扫描创建；物理 EOL、容器结束状态和匹配深度不能由调用方填写。
pub struct InlineParagraphLine {
    physical: Range<usize>,
    prefix: Range<usize>,
    eol: Range<usize>,
    state_origin: usize,
    state: LineStartState,
    matched: usize,
    depth: usize,
}

impl InlineParagraphLine {
    pub fn physical_range(&self) -> Range<usize> {
        self.physical.clone()
    }
    pub fn prefix_range(&self) -> Range<usize> {
        self.prefix.clone()
    }
    pub fn eol_range(&self) -> Range<usize> {
        self.eol.clone()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum InlineParagraphStep {
    Progress { work: GlobalWork },
    Complete { work: GlobalWork },
    Unsupported { work: GlobalWork },
}

#[derive(Clone, Copy)]
enum Phase {
    Scope,
    Prefix,
    Content,
    Complete,
    Unsupported,
}

/// 唯一活动短块的来源上下文；没有正文缓冲、祖先 AST、全源行表或独立 Global。
/// 行 metadata 的数量由同一个 ≤2KiB 完整源上限约束，释放随这个 Parser owner 结束。
pub struct InlineParagraphContext {
    seed: VerifiedInlineSemanticSeed,
    scope: Option<CapturedScopeCursor>,
    scope_seen: bool,
    starts: [Option<usize>; 3],
    // 只保真实 scope 与本短块物理前缀相交的 label enclosure；没有 label 字符串或祖先 AST。
    // 每项都来自同 producer 已完成的 opening，供 Core 保留其原字面而不误认数字列表标记。
    footnote_openings: Vec<Range<usize>>,
    prefix: Option<ContainerPrefix>,
    lines: Vec<InlineParagraphLine>,
    line_start: usize,
    next: usize,
    state_origin: usize,
    state: LineStartState,
    matched: usize,
    allocated: usize,
    total: GlobalWork,
    limits: InlinePlainLimits,
    phase: Phase,
    failure: Option<GlobalFrontierError>,
}

impl std::fmt::Debug for InlineParagraphContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 公开 Debug 仅给有限来源坐标和阶段事实，不展开共享 scope、行表或任何正文。
        formatter
            .debug_struct("InlineParagraphContext")
            .field("source", &self.seed.physical_range())
            .field("complete", &self.is_complete())
            .field("unsupported", &self.is_unsupported())
            .field("failure", &self.failure)
            .finish()
    }
}

impl VerifiedInlineSemanticSeed {
    /// 只有实际闭合 Paragraph/Setext 能创建；保留物理前缀，不把裁剪字符串重判为 Root。
    pub fn bounded_paragraph_context(
        &self,
        limits: InlinePlainLimits,
    ) -> Result<Option<InlineParagraphContext>, GlobalFrontierError> {
        if self.scope_depth() <= 1
            || !matches!(
                self.kind,
                InlineBlockKind::Paragraph | InlineBlockKind::SetextHeading(_)
            )
            || self.end - self.physical_start > MAX_SOURCE
        {
            return Ok(None);
        }
        if limits.max_byte_work == 0 || limits.max_transitions < MAX_SOURCE {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        let (origin, state) = self.opening.ok_or(GlobalFrontierError::InvalidFrame)?;
        if origin.checked_add(state.ix) != Some(self.start) {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let scope = CapturedScopeCursor::new(
            self.revision,
            self.source_end,
            self.start,
            self.spine.len().checked_sub(1),
            &self.spine,
        )?;
        Ok(Some(InlineParagraphContext {
            seed: self.clone(),
            scope: Some(scope),
            scope_seen: false,
            starts: [None; 3],
            footnote_openings: Vec::new(),
            prefix: None,
            lines: Vec::new(),
            line_start: self.physical_start,
            next: self.start,
            state_origin: origin,
            state,
            matched: self.spine.len(),
            allocated: 0,
            total: GlobalWork::default(),
            limits,
            phase: Phase::Scope,
            failure: None,
        }))
    }
}

impl InlineParagraphContext {
    /// 累计真实取得的目录与行槽；归还旧 prefix 不重发额度，Core 据增量联合准入原库存。
    pub fn allocated_workspace_bytes(&self) -> usize {
        self.allocated
    }
    pub fn is_complete(&self) -> bool {
        matches!(self.phase, Phase::Complete)
    }
    pub fn is_unsupported(&self) -> bool {
        matches!(self.phase, Phase::Unsupported)
    }
    /// 原 scope 阶段不读取正文；Core 可以先拒绝未支持角色，再决定是否分配和复制短块。
    pub fn requires_source(&self) -> bool {
        matches!(self.phase, Phase::Prefix | Phase::Content)
    }
    pub fn container_starts(&self) -> Option<[Option<usize>; 3]> {
        self.is_complete().then_some(self.starts)
    }
    pub fn line_at(&self, index: usize) -> Option<&InlineParagraphLine> {
        if !self.is_complete() {
            return None;
        }
        self.lines.get(index)
    }
    /// 只有完整 scope/行事实才可借用；原字面区间不能单独为另一 Source 生成 prefix 权限。
    pub fn footnote_openings(&self) -> Option<&[Range<usize>]> {
        self.is_complete()
            .then_some(self.footnote_openings.as_slice())
    }
    pub(crate) fn seed(&self) -> &VerifiedInlineSemanticSeed {
        &self.seed
    }

    fn fail(&mut self, error: GlobalFrontierError) -> GlobalFrontierError {
        // 首因保持 typed；失败立即归还活动游标及原行 metadata，不能续用部分成功。
        self.failure = Some(error);
        self.scope = None;
        self.prefix = None;
        self.lines = Vec::new();
        self.footnote_openings = Vec::new();
        error
    }

    /// 只读调用方已计量复制的同一完整短块；没有拼接或去前缀字符串。
    /// Core 仍在每次调用前后核原 Doc/generation；这里核 capture revision 并累计每次实际字节/角色工作。
    pub fn advance(
        &mut self,
        text: &str,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<InlineParagraphStep, GlobalFrontierError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if authority.cancelled {
            return Err(self.fail(GlobalFrontierError::Cancelled));
        }
        if authority.current_revision != self.seed.revision {
            return Err(self.fail(GlobalFrontierError::SourceSuperseded));
        }
        if self.requires_source()
            && (text.len() != self.seed.end - self.seed.physical_start || text.len() > MAX_SOURCE)
        {
            return Err(self.fail(GlobalFrontierError::InvalidFrame));
        }
        if !(MAX_SOURCE + CONTROL..=64 * 1024).contains(&budget) {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        let before = self.seed.spine.work_used();
        let was_scope = matches!(self.phase, Phase::Scope);
        let result = self.advance_inner(text, authority, budget);
        let mut work = match result {
            Ok(work) => work,
            Err(error) => return Err(self.fail(error)),
        };
        // 原 scope/prefix 的底层 role fuel 仍共享同一 spine；字节与额外目录操作同时交回外层总账。
        // CapturedScopeCursor 已包含其 role 增量，其他阶段在这里补回，避免重复或漏记。
        if !was_scope {
            work.transitions += self.seed.spine.work_used() - before;
        }
        self.total.byte_reads += work.byte_reads;
        self.total.transitions += work.transitions;
        if work.byte_reads > budget
            || work.transitions > budget
            || self.total.byte_reads > self.limits.max_byte_work
            || self.total.transitions > self.limits.max_transitions
        {
            return Err(self.fail(GlobalFrontierError::WorkFuel));
        }
        Ok(match self.phase {
            Phase::Complete => InlineParagraphStep::Complete { work },
            Phase::Unsupported => InlineParagraphStep::Unsupported { work },
            _ => InlineParagraphStep::Progress { work },
        })
    }

    fn advance_inner(
        &mut self,
        text: &str,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<GlobalWork, GlobalFrontierError> {
        let mut work = GlobalWork {
            byte_reads: 0,
            transitions: CONTROL,
        };
        match self.phase {
            Phase::Complete | Phase::Unsupported => return Ok(GlobalWork::default()),
            Phase::Scope => {
                let cursor = self
                    .scope
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?;
                let before = cursor.allocated_workspace_bytes();
                let opening_bytes = std::mem::size_of::<Range<usize>>();
                let step = cursor.advance(authority, budget - CONTROL - opening_bytes)?;
                self.allocated += cursor.allocated_workspace_bytes() - before;
                let measured = match step {
                    CapturedScopeStep::Progress { work }
                    | CapturedScopeStep::Entry { work, .. }
                    | CapturedScopeStep::Complete { work } => work,
                };
                work.transitions += measured.transitions;
                match step {
                    CapturedScopeStep::Entry {
                        role,
                        depth_index,
                        source_byte_start,
                        ..
                    } => {
                        let index = match role {
                            CapturedScopeRole::Quote => Some(0),
                            CapturedScopeRole::List {
                                marker: b'-' | b'+' | b'*',
                                ..
                            } => Some(1),
                            CapturedScopeRole::List {
                                marker: b'.' | b')',
                                ..
                            } => Some(2),
                            CapturedScopeRole::Item { .. } => None,
                            CapturedScopeRole::Footnote {
                                body_start: Some(end),
                            } => {
                                if end > self.seed.physical_start {
                                    // 保全真实 enclosure，不能先裁到 viewport 再把前面的数字当 marker。
                                    // 旧定义可能在此前物理行，只保与当前短块实际 raw 前缀相交的项。
                                    let before = self.footnote_openings.capacity();
                                    self.footnote_openings.reserve_exact(1);
                                    self.allocated += (self.footnote_openings.capacity() - before)
                                        * opening_bytes;
                                    self.footnote_openings.push(source_byte_start..end);
                                    work.transitions += opening_bytes;
                                }
                                None
                            }
                            CapturedScopeRole::Paragraph
                                if depth_index + 1 == self.seed.spine.len()
                                    && source_byte_start == self.seed.start =>
                            {
                                self.scope_seen = true;
                                None
                            }
                            _ => {
                                self.phase = Phase::Unsupported;
                                None
                            }
                        };
                        if let Some(index) = index {
                            self.starts[index] = Some(
                                self.starts[index]
                                    .map_or(source_byte_start, |old| old.min(source_byte_start)),
                            );
                        }
                    }
                    CapturedScopeStep::Complete { .. } => {
                        if !self.scope_seen {
                            return Err(GlobalFrontierError::InvalidFrame);
                        }
                        self.scope = None;
                        self.phase = Phase::Content;
                    }
                    _ => {}
                }
                if matches!(self.phase, Phase::Unsupported) {
                    self.scope = None;
                }
            }
            Phase::Prefix => {
                if self.prefix.is_none() {
                    self.prefix = Some(ContainerPrefix::new(
                        self.seed.revision,
                        self.line_start,
                        self.seed.source_end,
                        &self.seed.spine,
                        self.seed.options,
                    ));
                    return Ok(work);
                }
                let prefix = self.prefix.as_mut().unwrap();
                let at = prefix.next_byte();
                let local = at
                    .checked_sub(self.seed.physical_start)
                    .ok_or(GlobalFrontierError::InvalidFrame)?;
                let bytes = text
                    .as_bytes()
                    .get(local..)
                    .ok_or(GlobalFrontierError::InvalidFrame)?;
                let before = prefix.allocated_workspace_bytes();
                let (step, measured) = prefix.advance(
                    &SourceFrame {
                        source_revision: self.seed.revision,
                        byte_start: at,
                        bytes,
                        real_eof: self.seed.end == self.seed.source_end,
                    },
                    (budget - CONTROL - 1024) / 3,
                    (budget - CONTROL - 1024) / 3,
                )?;
                self.allocated += prefix.allocated_workspace_bytes() - before;
                work.byte_reads += measured.byte_reads;
                work.transitions += measured.transitions;
                match step {
                    PrefixProgress::Complete { state, matched } => {
                        self.state = state;
                        self.state_origin = self.line_start;
                        self.matched = matched;
                        self.next = self.line_start + state.ix;
                        self.prefix = None;
                        self.phase = Phase::Content;
                        work.transitions += 1024; // 真正归还本行 prefix 目录，不留给 Parser consumer 或下一量子。
                    }
                    PrefixProgress::NeedInput { .. } => {
                        return Err(GlobalFrontierError::InvalidFrame)
                    }
                    PrefixProgress::Progress => {}
                }
            }
            Phase::Content => {
                let bytes = text.as_bytes();
                let limit =
                    (budget - CONTROL - std::mem::size_of::<InlineParagraphLine>()).min(MAX_SOURCE);
                let mut end = None;
                while self.next < self.seed.end && work.byte_reads < limit {
                    let local = self.next - self.seed.physical_start;
                    let byte = bytes[local];
                    // CR 的 lookahead 也是真实读取；不是 LF 时下轮还会读同一个后继 byte，不能隐形省账。
                    // 剩余量只有一字节就保留 CR 未消费，下量子继续同一位置，绝不先假认独立 CR。
                    let lookahead = byte == b'\r' && local + 1 < bytes.len();
                    let reads = 1 + usize::from(lookahead);
                    if work.byte_reads + reads > limit {
                        work.byte_reads += 1;
                        work.transitions += 1;
                        break;
                    }
                    let count = if lookahead && bytes[local + 1] == b'\n' {
                        2
                    } else {
                        1
                    };
                    work.byte_reads += reads;
                    work.transitions += reads;
                    let eol = self.next;
                    self.next += count;
                    if matches!(byte, b'\r' | b'\n') {
                        end = Some(eol);
                        break;
                    }
                }
                if end.is_some() || self.next == self.seed.end {
                    // 行槽只保原坐标和四个 LineStart 标量；每次最多扩一项，容量按真实 reserve_exact 计量。
                    let item_bytes = std::mem::size_of::<InlineParagraphLine>();
                    let capacity = self.lines.capacity();
                    self.lines.reserve_exact(1);
                    self.allocated += (self.lines.capacity() - capacity) * item_bytes;
                    self.lines.push(InlineParagraphLine {
                        physical: self.line_start..self.next,
                        prefix: self.line_start..self.state_origin + self.state.ix,
                        eol: end.unwrap_or(self.next)..self.next,
                        state_origin: self.state_origin,
                        state: self.state,
                        matched: self.matched,
                        depth: self.seed.spine.len(),
                    });
                    work.transitions += item_bytes;
                    self.line_start = self.next;
                    self.phase = if self.next == self.seed.end {
                        Phase::Complete
                    } else {
                        Phase::Prefix
                    };
                }
            }
        }
        Ok(work)
    }

    /// 原 firstpass 只借已完成事实恢复同一物理续行；没有 marker 查找、source 读取或 scope 再遍历。
    pub(crate) fn line_state_at(
        &self,
        local: usize,
    ) -> Result<(LineStartState, usize, usize), GlobalFrontierError> {
        if !self.is_complete() {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let absolute = self.seed.physical_start + local;
        let at = self
            .lines
            .binary_search_by_key(&absolute, |line| line.physical.start)
            .map_err(|_| GlobalFrontierError::InvalidFrame)?;
        let line = &self.lines[at];
        if line.state_origin != absolute {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        Ok((line.state, line.matched, line.depth))
    }

    /// inline 第二遍仍按原 skip_container_prefixes 的语义，仅跳过容器，不额外 trim paragraph 空白。
    /// suffix 是 Parser 自己从同一 input 借出的片段；地址差只定位这次借用，既不保存指针也不授源权限。
    pub(crate) fn prefix_bytes_in(&self, input: &str, suffix: &[u8]) -> usize {
        let local = suffix
            .as_ptr()
            .addr()
            .checked_sub(input.as_ptr().addr())
            .expect("Parser inline slice 属于同一真实短块");
        assert!(local <= input.len() && suffix.len() <= input.len() - local);
        if local == input.len()
            || local > 0 && input.as_bytes()[local] == b'\n' && input.as_bytes()[local - 1] == b'\r'
        {
            return 0; // 原 code/math 循环分别观察 CR 和 LF，尚在 LF 处时不能提前消费下一行前缀。
        }
        let (state, _, _) = self
            .line_state_at(local)
            .expect("Parser 只在原物理换行后请求容器前缀");
        assert!(state.ix <= suffix.len());
        state.ix
    }
}

impl crate::block_context::ParagraphContext for InlineParagraphContext {
    fn is_in_table(&self) -> bool {
        false // 工厂仅接受真实 Paragraph/Setext，Global 已结束 table 候选改型。
    }
    fn parent_is_paragraph(&self) -> bool {
        self.is_complete() && self.scope_seen
    }
    fn spine_entry_is_definition(&self, _index: usize) -> bool {
        false // 已消费的 Footnote 仍不是 Definition；只有原 scope 全部通过后 context 才能 Complete。
    }
}
