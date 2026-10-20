// Copyright 2015 Google Inc. All rights reserved.
// 闭合块分类沿固定pulldown-cmark0.13.4真实firstpass；完整MIT条款见LICENSE。

//! 只证明真实块已闭合；raw词法范围仍含真实换行，不宣称inline配对或renderer Ready。

use std::ops::Range;

use crate::line_start_stream::LineStartState;
use crate::{structural_spine::StructuralSpineSnapshot, HeadingLevel, Options};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只导出已经确定的最终块种类；paragraph候选变成table时不会经过此出口。
pub enum InlineBlockKind {
    Paragraph,
    SetextHeading(HeadingLevel),
    TableCell,
}

#[derive(Debug)]
/// 字段与构造只在真实producer可见；外部不能用任意Range制造闭合块权限。
/// 页只共享同一个producer总预算，不持source、映射、AST或正文行集合。
pub struct VerifiedGlobalInlineBlock {
    pub(super) revision: u64,
    pub(super) source_end: usize,
    pub(super) options: Options,
    pub(super) kind: InlineBlockKind,
    pub(super) start: usize,
    pub(super) physical_start: usize,
    pub(super) first_state_origin: usize,
    pub(super) first_state: LineStartState,
    pub(super) end: usize,
    pub(super) lexical_end: usize,
    pub(super) last_content_end: usize,
    pub(super) last_line_end: usize,
    pub(super) spine: StructuralSpineSnapshot,
}

impl VerifiedGlobalInlineBlock {
    /// 原 Fact 一次性配对移交 origin 与语义 seed；两者共享原 spine 页及累计角色账本。
    /// seed 只证明真实来源，后继仍须完成 Plain/完整 inline 与 Core 上下文才能授予样式权限。
    pub fn into_origins_and_semantic_seed(
        self,
        limits: super::BlockOriginLimits,
    ) -> Result<
        (super::BlockOriginCursor, super::VerifiedInlineSemanticSeed),
        super::GlobalFrontierError,
    > {
        let seed = super::VerifiedInlineSemanticSeed::from_verified(&self);
        let origins = super::BlockOriginCursor::from_verified(self, limits)?;
        Ok((origins, seed))
    }

    /// 消耗真实闭合票据创建逐物理行源坐标游标；不接受任意 Range 或调用方伪造的容器种子。
    /// 限额在整个游标生命周期内累计，原结构页的 role fuel 仍与真实 producer 共享。
    pub fn into_line_origins(
        self,
        limits: super::BlockOriginLimits,
    ) -> Result<super::BlockOriginCursor, super::GlobalFrontierError> {
        super::BlockOriginCursor::from_verified(self, limits)
    }

    pub fn source_revision(&self) -> u64 {
        self.revision
    }

    pub fn source_end(&self) -> usize {
        self.source_end
    }

    pub fn kind(&self) -> InlineBlockKind {
        self.kind
    }

    pub fn options(&self) -> Options {
        self.options
    }

    /// 与原firstpass完成leaf相同的源范围；Setext包含underline行。
    pub fn source_range(&self) -> Range<usize> {
        self.start..self.end
    }

    /// 额外保留真实首物理行，不能从content起点减去猜测的quote/list前缀。
    pub fn physical_range(&self) -> Range<usize> {
        self.physical_start..self.end
    }

    /// 原始候选扫描范围不含Setext underline，但仍有真实prefix/EOL。
    /// 此范围不能直接拼成新文档或用作最终inline输出的连续语义范围。
    pub fn lexical_range(&self) -> Range<usize> {
        self.start..self.lexical_end
    }

    /// 最后正文物理行的真实separator；原firstpass是否提交break由后继共享lexer决定。
    pub fn terminal_eol_range(&self) -> Range<usize> {
        self.last_content_end..self.last_line_end
    }

    /// 只读标量不遍历祖先；本阶段不flatten scope或把起点tight标量当最终list Tag。
    pub fn scope_depth(&self) -> usize {
        self.spine.len()
    }
}

#[cfg(test)]
mod tests;
