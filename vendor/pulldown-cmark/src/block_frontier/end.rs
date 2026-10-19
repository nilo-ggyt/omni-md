// Copyright 2015 Google Inc. All rights reserved.
// 本地结束事实来自固定0.13.4真实table续行；完整MIT条款见LICENSE。

//! 只有真实表尾才有追加新行权限；viewport末行、失败和NeedInput均不能创建本证明。

use super::{TableContinuationState, TableScalars};
use crate::structural_spine::StructuralSpineSnapshot;
use std::ops::Range;

#[derive(Clone, Debug)]
/// 不可由公开字段伪造的真实表尾。它仅持坐标和共享scope，不持源、行正文或整表AST。
/// 消费者仍须核验原DocRoot弱身份、source_revision及当前编辑意图；revision不是跨文档身份。
/// 克隆不会重新授予编辑权限，且所有保留的scope页仍受原spine共同容量/fuel限制。
pub struct VerifiedTableEnd {
    spine: StructuralSpineSnapshot,
    scalars: TableScalars,
}

impl VerifiedTableEnd {
    /// 与原global构造时相同的源代次；不能单独代替实际文档root身份核验。
    pub fn source_revision(&self) -> u64 {
        self.scalars.source_revision
    }
    /// 真实header完整物理行起点，含首次容器marker，不是局部投影的header下标。
    pub fn header_byte_start(&self) -> usize {
        self.scalars.header_byte_start
    }
    /// 已验证header的真实列数；不能用当前window的最大已物化列数猜末列。
    pub fn columns(&self) -> usize {
        self.scalars.columns
    }
    /// body ordinal沿原续行合同从1开始；只有H/D时为None，不能把delimiter冒充body行。
    pub fn last_body_row_ordinal(&self) -> Option<usize> {
        self.scalars
            .next_row_ordinal
            .checked_sub(1)
            .filter(|ordinal| *ordinal != 0)
    }
    /// 最后一条已提交table源行的末尾，通常含EOL；失败候选/下一块不包含在范围内。
    pub fn byte_end(&self) -> usize {
        self.scalars.next_byte
    }
    /// 与byte_end对应的全源UTF16坐标，来自真实逐byte计量，不能用byte长度替代。
    pub fn utf16_end(&self) -> usize {
        self.scalars.next_utf16
    }
    /// byte_end处的真实零起始物理行编号；未终止末行仍是该末行的编号。
    pub fn source_line_at_end(&self) -> usize {
        self.scalars.next_source_line
    }
    /// 说明追加前是否已存在真实CR/LF；调用者必须按它决定是否先补分隔换行。
    pub fn ended_with_eol(&self) -> bool {
        self.scalars.last_line_terminated
    }
    /// 只有由源owner明确交付真实EOF时才可能等于source总长；短frame尾没有此权限。
    pub fn ended_at_source_eof(&self) -> bool {
        self.scalars.next_byte == self.scalars.source_end
    }
    /// 可有限借出原D行的实际容器continuation prefix，保留tab/quote/list indent原字节。
    /// 不能复制H行初始的`- `或footnote定义marker来追加，否则会建立另一个容器。
    /// 本范围只有语法context权限；读源与分配仍由消费者按自身预算及root身份单独准入。
    pub fn continuation_prefix_byte_range(&self) -> Range<usize> {
        self.scalars.separator_byte_start
            ..self.scalars.separator_byte_start + self.scalars.separator_prefix_bytes
    }
    /// 精确scope仍由共享页保存；此有限标量不宣称可从depth重建角色或quote/list样式。
    pub fn container_depth(&self) -> usize {
        self.spine.len()
    }
}

impl TableContinuationState {
    /// 只有真实续体完成EOF或非成员分类后才返回表尾；部分行、失败和NeedInput均无此权限。
    /// 从既有checkpoint恢复的续体仍沿同一结构与补格预算验证，不能由公开字段伪造结束种子。
    /// 调用方继续核原文档root、源代次和编辑意图；本证明不保存源owner，也不恢复外层Global。
    pub fn verified_end(&self) -> Option<VerifiedTableEnd> {
        if self.failed || self.row.is_some() || self.ended != Some(self.scalars.next_byte) {
            // 失败、部分读取与仍在前瞻中的源行保留未完成状态，绝不当成可追加的空白表尾。
            return None;
        }
        Some(VerifiedTableEnd {
            spine: self.spine.clone(),
            scalars: self.scalars,
        })
    }
}
