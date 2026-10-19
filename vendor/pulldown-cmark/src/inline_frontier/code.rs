// Copyright 2017 Google Inc. All rights reserved.
// 原MaybeCode首个同长度closer规则的分段适配；MIT许可见LICENSE。

use super::{
    CodeDelimiterCandidate, InlineCandidate, InlineError, InlineLexicalCursor, InlineStepStatus,
    InlineWork, MAX_INLINE_STEP_WORK, MIN_INLINE_STEP_WORK,
};
use crate::block_frontier::SourceFrame;
use crate::inline_scan::{code_matches, code_search_length};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 尚未通过HTML/math/link优先序的代码范围候选；不包含规范化后的code正文。
pub struct CodeSpanCandidate {
    open: CodeDelimiterCandidate,
    close: CodeDelimiterCandidate,
}

impl CodeSpanCandidate {
    /// 包含真实源换行/容器前缀；后继有限投影需依原make_code_span规则转换，不得直接展示。
    pub fn raw_body_range(self) -> Range<usize> {
        self.open.end..self.close.start
    }

    pub fn range(self) -> Range<usize> {
        // 原make_code_span将转义opener的第一个tick留作literal，Code事件从后一个tick开始。
        (self.open.start + usize::from(self.open.escaped))..self.close.end
    }

    pub fn opening(self) -> CodeDelimiterCandidate {
        self.open
    }
    pub fn closing(self) -> CodeDelimiterCandidate {
        self.close
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeSpanStep {
    Progress {
        work: InlineWork,
    },
    NeedInput {
        work: InlineWork,
    },
    Candidate {
        work: InlineWork,
        candidate: CodeSpanCandidate,
    },
    Unmatched {
        work: InlineWork,
        replay_from: usize,
    },
}

#[derive(Debug)]
/// 一个opener的有限lookahead，不是所有代码跨度的全局配对索引。
/// 未匹配时外层必须从replay_from重新处理后继token，不能跳到本probe的read末尾；否则
/// `` `foo``bar`` ``等真实输入会丢掉后面的有效跨度。重复probe的累计工作由后继调度计费。
pub struct CodeSpanProbe {
    opener: CodeDelimiterCandidate,
    search_count: usize,
    cursor: InlineLexicalCursor,
    closed: bool,
    failure: Option<InlineError>,
}

impl CodeSpanProbe {
    /// escaped单tick按原规则只是literal并返回None；没有虚拟opener或布尔上下文seed。
    pub fn from_opener(
        opener: CodeDelimiterCandidate,
        source_end: usize,
        paragraph_end: usize,
    ) -> Result<Option<Self>, InlineError> {
        if opener.end > paragraph_end || paragraph_end > source_end {
            return Err(InlineError::InvalidRange);
        }
        let Some(search_count) = code_search_length(opener.raw_count, opener.escaped) else {
            return Ok(None);
        };
        Ok(Some(Self {
            opener,
            search_count,
            cursor: InlineLexicalCursor::new(
                opener.revision,
                source_end,
                opener.end..paragraph_end,
            )?,
            closed: false,
            failure: None,
        }))
    }

    pub fn next_byte(&self) -> usize {
        self.cursor.next_byte()
    }
    pub fn committed_byte(&self) -> usize {
        self.opener.start
    }

    pub fn cancel(&mut self) {
        self.cursor.cancel();
        self.failure = Some(InlineError::Cancelled);
    }

    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<CodeSpanStep, InlineError> {
        if let Some(error) = self.failure {
            // 失效与取消均保留首因；错误不是可用另一frame重试的暂时NeedInput。
            return Err(error);
        }
        if self.closed {
            return Err(InlineError::Closed);
        }
        if !((MIN_INLINE_STEP_WORK + 1)..=MAX_INLINE_STEP_WORK).contains(&budget) {
            self.failure = Some(InlineError::InvalidBudget);
            return Err(InlineError::InvalidBudget);
        }
        // 在进入child前为返回分类保留一个transition，不能在child吃满fuel后偷偷继续配对。
        let step = match self.cursor.advance(frame, budget - 1) {
            Ok(step) => step,
            Err(error) => {
                self.failure = Some(error);
                return Err(error);
            }
        };
        let mut work = step.work;
        work.transitions += 1;
        Ok(match step.status {
            InlineStepStatus::Candidate(InlineCandidate::CodeDelimiter(closer))
                if code_matches(self.search_count, closer.raw_count) =>
            {
                // 与原Parser一样忽略closer自身的escaped标记，只比较其原始run长度。
                self.closed = true;
                CodeSpanStep::Candidate {
                    work,
                    candidate: CodeSpanCandidate {
                        open: self.opener,
                        close: closer,
                    },
                }
            }
            InlineStepStatus::Complete => {
                self.closed = true;
                CodeSpanStep::Unmatched {
                    work,
                    replay_from: self.opener.end,
                }
            }
            InlineStepStatus::NeedInput => CodeSpanStep::NeedInput { work },
            // 不同长度的run/实体不改变当前目标；它们只作为lookahead，不发布为最终inline。
            InlineStepStatus::Progress | InlineStepStatus::Candidate(_) => {
                CodeSpanStep::Progress { work }
            }
        })
    }
}
