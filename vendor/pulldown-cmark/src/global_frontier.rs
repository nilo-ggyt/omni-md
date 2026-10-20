// Copyright 2015 Google Inc. All rights reserved.
// 本地全局结构前沿复用固定0.13.4共享规则；MIT条款见LICENSE。

//! 无全AST的分段结构producer；所有前瞻锚点与语义提交位置分开。

use crate::block_context::{container_role_plan, ContainerRolePlan};
use crate::block_frontier::SourceFrame;
pub use crate::block_frontier::VerifiedTableEnd;
use crate::line_start_stream::{
    LineStartError, LineStartFrame, LineStartOperation, LineStartProgress, LineStartState,
    LineStartStream, LineStartValue, LineStartWork, MAX_LINE_START_STEP,
};
use crate::structural_spine::{
    StructuralCursorStep, StructuralSpineCursor, StructuralSpineSnapshot,
};
pub use crate::structural_spine::{StructuralSpineError, StructuralSpineLimits};
use crate::Options;

mod inline_block;
pub use inline_block::{InlineBlockKind, VerifiedGlobalInlineBlock};
mod inline_origin;
pub use inline_origin::{
    BlockOriginCursor, BlockOriginKind, BlockOriginLimits, BlockOriginLine, BlockOriginLineKind,
    BlockOriginState, BlockOriginStep,
};
mod inline_semantic;
pub(crate) use inline_semantic::TableCellInlineContext;
pub use inline_semantic::{
    InlineCodeCursor, InlineCodeStep, InlinePlainContainers, InlinePlainCursor, InlinePlainLimits,
    InlinePlainStep, InlinePlainTerminal, VerifiedInlineCodeSpan, VerifiedInlineEmphasisRun,
    VerifiedInlinePlainProof, VerifiedInlineSemanticSeed,
};
mod leaf_block;
pub use leaf_block::{
    HtmlLeafTermination, LeafBlockCloseReason, LeafBlockDetails, LeafBlockKind,
    VerifiedGlobalLeafBlock,
};
use leaf_block::{PendingGlobalLeaf, PendingLeafKind};
mod scope;
pub use scope::{CapturedScopeCursor, CapturedScopeHeader, CapturedScopeRole, CapturedScopeStep};

mod inline_paragraph;
pub use inline_paragraph::{InlineParagraphContext, InlineParagraphLine, InlineParagraphStep};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalFrontierError {
    InvalidFrame,
    InvalidBudget,
    Cancelled,
    SourceSuperseded,
    SourceUtf8,
    WorkFuel,
    UnsupportedOptions,
    Spine(StructuralSpineError),
    ArithmeticOverflow,
    Frontier(crate::block_frontier::FrontierError),
}

impl From<crate::block_frontier::FrontierError> for GlobalFrontierError {
    fn from(error: crate::block_frontier::FrontierError) -> Self {
        Self::Frontier(error)
    }
}

impl From<StructuralSpineError> for GlobalFrontierError {
    fn from(error: StructuralSpineError) -> Self {
        Self::Spine(error)
    }
}
impl From<LineStartError> for GlobalFrontierError {
    fn from(error: LineStartError) -> Self {
        match error {
            LineStartError::InvalidState | LineStartError::InvalidFrame => Self::InvalidFrame,
            LineStartError::InvalidBudget => Self::InvalidBudget,
            LineStartError::ArithmeticOverflow => Self::ArithmeticOverflow,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PrefixPhase {
    Role,
    SpaceQuote,
    Quote,
    SpaceIndent,
    Eol,
    Done,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrefixProgress {
    Progress,
    NeedInput {
        next_byte: usize,
    },
    Complete {
        state: LineStartState,
        matched: usize,
    },
}

/// 一个prefix只有固定page游标和一个LineStart续体，不存完整物理行或flatten祖先Vec。
pub(crate) struct ContainerPrefix {
    revision: u64,
    origin: usize,
    source_end: usize,
    options: Options,
    state: LineStartState,
    saved: LineStartState,
    cursor: StructuralSpineCursor,
    current: Option<LineStartStream>,
    matched: usize,
    phase: PrefixPhase,
}

impl ContainerPrefix {
    /// 只交出原cursor已实际分配的目录大小，不把保留容量或整个scope算作源正文。
    fn allocated_workspace_bytes(&self) -> usize {
        self.cursor.allocated_workspace_bytes()
    }

    pub(crate) fn new(
        revision: u64,
        origin: usize,
        source_end: usize,
        spine: &StructuralSpineSnapshot,
        options: Options,
    ) -> Self {
        Self {
            revision,
            origin,
            source_end,
            options,
            state: LineStartState::default(),
            saved: LineStartState::default(),
            cursor: spine.cursor(),
            current: None,
            matched: 0,
            phase: PrefixPhase::Role,
        }
    }

    /// 工作位置可能未消费源；外层按本锚点重借frame，不把它误加到semantic position。
    pub(crate) fn next_byte(&self) -> usize {
        self.current
            .as_ref()
            .map_or(self.origin + self.state.ix, LineStartStream::next_byte)
    }

    fn start(
        &mut self,
        operation: LineStartOperation,
        phase: PrefixPhase,
    ) -> Result<(), GlobalFrontierError> {
        self.current = Some(LineStartStream::new(
            self.revision,
            self.origin,
            self.source_end,
            self.state,
            operation,
        )?);
        self.phase = phase;
        Ok(())
    }

    /// 所有role（包括Other）都收费，不能在bytes预算为零时隐形遍历任意spine。
    pub(crate) fn advance(
        &mut self,
        frame: &SourceFrame<'_>,
        byte_budget: usize,
        transition_budget: usize,
    ) -> Result<(PrefixProgress, LineStartWork), GlobalFrontierError> {
        if byte_budget == 0
            || transition_budget == 0
            || byte_budget > MAX_LINE_START_STEP
            || transition_budget > MAX_LINE_START_STEP
        {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        let mut work = LineStartWork::default();
        loop {
            if matches!(self.phase, PrefixPhase::Done) {
                return Ok((
                    PrefixProgress::Complete {
                        state: self.state,
                        matched: self.matched,
                    },
                    work,
                ));
            }
            if work.transitions == transition_budget || work.byte_reads == byte_budget {
                return Ok((PrefixProgress::Progress, work));
            }
            if matches!(self.phase, PrefixPhase::Role) {
                work.transitions += 1;
                match self.cursor.next()? {
                    StructuralCursorStep::Building => continue, // 每页一个真实工作step，游标尚未取得首role。
                    StructuralCursorStep::End => {
                        self.phase = PrefixPhase::Done;
                        continue;
                    }
                    StructuralCursorStep::Entry { index, entry } => {
                        debug_assert_eq!(index, self.matched);
                        self.saved = self.state;
                        match container_role_plan(entry.role(), self.options) {
                            ContainerRolePlan::Quote => {
                                self.start(LineStartOperation::Space(3), PrefixPhase::SpaceQuote)?
                            }
                            ContainerRolePlan::Indent(indent) => self.start(
                                LineStartOperation::Space(indent),
                                PrefixPhase::SpaceIndent,
                            )?,
                            ContainerRolePlan::Pass => self.matched += 1,
                        }
                    }
                }
                continue;
            }
            let child = self.current.as_mut().expect("非Role阶段拥有LineStart续体");
            let step = child.advance(
                LineStartFrame {
                    source_revision: frame.source_revision,
                    byte_start: frame.byte_start,
                    bytes: frame.bytes,
                    real_eof: frame.real_eof,
                },
                byte_budget - work.byte_reads,
                transition_budget - work.transitions,
            )?;
            work.byte_reads += step.work.byte_reads;
            work.transitions += step.work.transitions;
            match step.progress {
                LineStartProgress::NeedInput { next_byte } => {
                    return Ok((PrefixProgress::NeedInput { next_byte }, work));
                }
                LineStartProgress::Progress { .. } => return Ok((PrefixProgress::Progress, work)),
                LineStartProgress::Complete { state, value } => {
                    // child可能在上一step已Complete；父返回点仍收费一次，避免零输入busy循环。
                    if work.transitions == transition_budget {
                        return Ok((PrefixProgress::Progress, work));
                    }
                    work.transitions += 1;
                    self.state = state;
                    self.current = None;
                    match (self.phase, value) {
                        (PrefixPhase::SpaceQuote, LineStartValue::SpaceRemaining(_)) => {
                            self.start(LineStartOperation::Quote, PrefixPhase::Quote)?
                        }
                        (PrefixPhase::Quote, LineStartValue::Bool(true)) => {
                            self.matched += 1;
                            self.phase = PrefixPhase::Role;
                        }
                        (PrefixPhase::Quote, LineStartValue::Bool(false)) => {
                            self.state = self.saved;
                            self.phase = PrefixPhase::Done;
                        }
                        (PrefixPhase::SpaceIndent, LineStartValue::SpaceRemaining(0)) => {
                            self.matched += 1;
                            self.phase = PrefixPhase::Role;
                        }
                        (PrefixPhase::SpaceIndent, LineStartValue::SpaceRemaining(_)) => {
                            self.start(LineStartOperation::AtEol, PrefixPhase::Eol)?
                        }
                        (PrefixPhase::Eol, LineStartValue::Bool(true)) => {
                            self.matched += 1;
                            self.phase = PrefixPhase::Role;
                        }
                        (PrefixPhase::Eol, LineStartValue::Bool(false)) => {
                            self.state = self.saved;
                            self.phase = PrefixPhase::Done;
                        }
                        _ => unreachable!("内部prefix阶段与LineStart返回值必须同类型"),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

mod producer;
pub(crate) use producer::VerifiedGlobalTable;
pub use producer::{
    GlobalFact, GlobalFrontier, GlobalFrontierLimits, GlobalSourceAuthority, GlobalStep,
    GlobalTableFact, GlobalWork, VerifiedGlobalReference,
};
mod reference_scope;
pub use reference_scope::{
    ReferencePrefixPurpose, ReferencePrefixScanner, ReferencePrefixStep, VerifiedInlinePrefix,
};
