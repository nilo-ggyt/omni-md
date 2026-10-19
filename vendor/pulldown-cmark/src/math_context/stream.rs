// Copyright 2015 Google Inc. All rights reserved.
// 固定上游数学上下文的本地分段适配；MIT条款见LICENSE。

//! 只恢复firstpass的数学状态，不构造inline AST、源行buffer或链接/HTML值。

use super::MathContext;
use crate::block_frontier::SourceFrame;
use crate::scanners::is_ascii_punctuation;
use std::ops::Range;

const MAX_STEP: usize = 64 * 1024;
const MAX_SEEDS: usize = 257;
const MAX_PROJECTION_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MathContextError {
    InvalidFrame,
    InvalidBudget,
    Cancelled,
    SourceSuperseded,
    InvalidSeeds,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 当前source owner每step重新证明权限；构造时的revision不能代替后继取消检查。
pub struct MathSourceAuthority {
    pub current_revision: u64,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 同时收取真实byte读取和零byte完成转换；重放也收费。
pub struct MathContextWork {
    pub byte_reads: usize,
    pub transitions: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MathContextStep {
    Progress {
        next_byte: usize,
        work: MathContextWork,
    },
    NeedInput {
        next_byte: usize,
        work: MathContextWork,
    },
    Complete {
        context: MathContext,
        work: MathContextWork,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 一个已验证完整语义row的扫描续体；range/end来自结构producer，不从任意frame尾推导行尾。
pub struct MathContextScan {
    revision: u64,
    source_end: usize,
    end: usize,
    next: usize,
    context: MathContext,
    escaped: bool,
    complete: bool,
    failed: bool,
}

impl MathContextScan {
    pub fn new(
        revision: u64,
        source_end: usize,
        range: Range<usize>,
        context: MathContext,
    ) -> Result<Self, MathContextError> {
        if range.start > range.end || range.end > source_end {
            // 结构范围必须在同一immutable source里；倒序range不能伪装成空完成。
            return Err(MathContextError::InvalidFrame);
        }
        Ok(Self {
            revision,
            source_end,
            end: range.end,
            next: range.start,
            context,
            escaped: false,
            complete: false,
            failed: false,
        })
    }

    pub fn next_byte(&self) -> usize {
        self.next
    }

    /// 每step借≤64KiB真实字节；原lexer各跳过分支中只有punctuation escape能吞掉数学token。
    /// Code/HTML在firstpass仅创建Maybe节点，内部字节仍经过math分支；entity/repeat/image跳段
    /// 只含原规则限定的非math字节。因此这里复用原punctuation集合及同一数学转移即可恢复状态。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: MathSourceAuthority,
        budget: usize,
    ) -> Result<MathContextStep, MathContextError> {
        let result = self.advance_inner(frame, authority, budget);
        if result.is_err() {
            // 首错撤权后不接受另一frame“修复”请求，不能由部分状态发布window。
            self.failed = true;
        }
        result
    }

    fn advance_inner(
        &mut self,
        frame: SourceFrame<'_>,
        authority: MathSourceAuthority,
        budget: usize,
    ) -> Result<MathContextStep, MathContextError> {
        if self.failed {
            return Err(MathContextError::Closed);
        }
        if authority.cancelled {
            return Err(MathContextError::Cancelled);
        }
        if authority.current_revision != self.revision || frame.source_revision != self.revision {
            // 完成值也先核owner代次，不使旧Ready绕过当前source authority。
            return Err(MathContextError::SourceSuperseded);
        }
        if !(2..=MAX_STEP).contains(&budget) {
            return Err(MathContextError::InvalidBudget);
        }
        let frame_end = frame
            .byte_start
            .checked_add(frame.bytes.len())
            .ok_or(MathContextError::InvalidFrame)?;
        if frame.bytes.len() > MAX_STEP
            || frame_end > self.source_end
            || (frame.real_eof && frame_end != self.source_end)
            || frame.byte_start > self.next
        {
            // 短frame不是EOF；real_eof只能在真实source尾成立。
            return Err(MathContextError::InvalidFrame);
        }
        let mut work = MathContextWork::default();
        if self.complete {
            return Ok(MathContextStep::Complete {
                context: self.context,
                work,
            });
        }
        while self.next < self.end {
            if work.transitions + 2 > budget {
                // 读+状态转换预先收费；从不先多做一个token再报告已超预算。
                return Ok(MathContextStep::Progress {
                    next_byte: self.next,
                    work,
                });
            }
            let Some(index) = self.next.checked_sub(frame.byte_start) else {
                return Err(MathContextError::InvalidFrame);
            };
            let Some(&byte) = frame.bytes.get(index) else {
                // 所需字节不在本借用frame内；保留escape续体，等待真实后继输入。
                return Ok(MathContextStep::NeedInput {
                    next_byte: self.next,
                    work,
                });
            };
            work.byte_reads += 1;
            work.transitions += 2;
            self.next += 1;
            if self.escaped {
                self.escaped = false;
                if is_ascii_punctuation(byte) {
                    // 原backslash_transition对punctuation至少跳过本字节；backtick run和
                    // double-backslash-pipe额外跳的字节都不含数学token，逐byte消费与其状态等价。
                    continue;
                }
                // 非punctuation不被原escape吞掉，必须继续走本字节的普通分派。
            }
            match byte {
                b'\\' => self.escaped = true,
                b'$' => {
                    self.context.dollar();
                }
                b'{' => self.context.open(),
                b'}' => self.context.close(),
                _ => {} // 其他真实字节不改变数学上下文，但同样纳入读取/转换fuel。
            }
        }
        if work.transitions == budget {
            // 完成转换留到下一step；不能在预算尾免费发布一个新的状态。
            return Ok(MathContextStep::Progress {
                next_byte: self.next,
                work,
            });
        }
        work.transitions += 1;
        self.complete = true;
        Ok(MathContextStep::Complete {
            context: self.context,
            work,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 有限projection的真实row起点与真实原源扫描所得状态；不持正文。
pub struct TableMathSeed {
    row_start: usize,
    context: MathContext,
}

impl TableMathSeed {
    pub fn new(row_start: usize, context: MathContext) -> Self {
        Self { row_start, context }
    }
    pub fn row_start(&self) -> usize {
        self.row_start
    }
    pub fn context(&self) -> MathContext {
        self.context
    }
}

/// 任何AST分配前限制整个seed输入；实际source/root成员权限由Core封装的producer承担。
pub(crate) fn validate_seeds(text: &str, seeds: &[TableMathSeed]) -> Result<(), MathContextError> {
    if text.len() > MAX_PROJECTION_BYTES || seeds.len() > MAX_SEEDS {
        return Err(MathContextError::InvalidSeeds);
    }
    let mut previous = None;
    for seed in seeds {
        let at = seed.row_start;
        if at >= text.len()
            || !text.is_char_boundary(at)
            || previous.is_some_and(|last| at <= last)
            || (at != 0 && !matches!(text.as_bytes()[at - 1], b'\n' | b'\r'))
            || (at != 0 && text.as_bytes()[at - 1] == b'\r' && text.as_bytes()[at] == b'\n')
        {
            // 只允许严格递增的完整物理row起点，不接受CRLF中部或UTF8中部坐标。
            return Err(MathContextError::InvalidSeeds);
        }
        previous = Some(at);
    }
    Ok(())
}
