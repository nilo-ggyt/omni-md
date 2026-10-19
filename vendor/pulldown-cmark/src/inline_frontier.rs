// Copyright 2015 Google Inc. All rights reserved.
// 固定0.13.4词法规则的本地分段入口；完整MIT许可见LICENSE。

//! 第一阶段只生产原始行内候选，不授予最终样式权限。
//!
//! HTML、math、link与emphasis仍按原Parser的实际优先序配对。特别是HTML之前出现的
//! backtick与HTML内部的backtick不能凭本模块的候选直接发布为Code。调用者也不能把
//! lookahead的read cursor当语义commit；失败opener之后仍可能有另一段有效代码。

mod code;
mod index;
mod lexical;
#[cfg(test)]
mod tests;

pub use crate::inline_scan::EntityValue as DecodedEntity;
pub use code::{CodeSpanCandidate, CodeSpanProbe, CodeSpanStep};
pub use index::{
    CodeIndexError, CodeIndexLookup, CodeIndexStatus, CodeIndexStep, CodeIndexWork,
    CodeLastRunIndex, CodeLastRunIndexBuilder, IndexedCodeDecision, MAX_CODE_INDEX_METADATA_BYTES,
    MIN_CODE_INDEX_STEP_WORK,
};
pub use lexical::{
    CodeDelimiterCandidate, EntityCandidate, InlineCandidate, InlineLexicalCursor, InlineStep,
    InlineStepStatus,
};

/// 小预算仍须容纳一次静态实体lookup；不允许反复Progress却永远不能执行同一转换。
pub const MIN_INLINE_STEP_WORK: usize = crate::entities::ENTITY_LOOKUP_BYTE_WORK_BOUND + 4;
pub const MAX_INLINE_STEP_WORK: usize = 64 * 1024;
pub const MAX_INLINE_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineError {
    InvalidFrame,
    InvalidBudget,
    InvalidRange,
    Cancelled,
    Closed,
    Overflow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 每次实际peek也收byte费；零byte完成/拒绝转换另外收费，不以cursor位移冒充总工作。
pub struct InlineWork {
    pub byte_reads: usize,
    pub transitions: usize,
    /// 静态表二分比较的保守字节上界，区别于已观察到的源peek；不把它称为实际CPU指令数。
    pub entity_lookup_byte_work_bound: usize,
}

impl InlineWork {
    pub fn total(self) -> usize {
        // 三项由单step同一上限准入，因而这个和不会接近usize溢出。
        self.byte_reads + self.transitions + self.entity_lookup_byte_work_bound
    }
}
