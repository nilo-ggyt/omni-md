// Copyright 2015 Google Inc. All rights reserved.
// 分段候选共享原punctuation/实体/代码规则；MIT许可见LICENSE。

use super::{
    InlineError, InlineWork, MAX_INLINE_FRAME_BYTES, MAX_INLINE_STEP_WORK, MIN_INLINE_STEP_WORK,
};
use crate::block_frontier::SourceFrame;
use crate::inline_scan::{EntityScanner, EntityTransition, EntityValue};
use crate::scanners::is_ascii_punctuation;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只能由实际连续源扫描创建；revision仍须由外层Weak root/source identity再次绑定。
pub struct CodeDelimiterCandidate {
    pub(crate) revision: u64,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) raw_count: usize,
    pub(crate) escaped: bool,
}

impl CodeDelimiterCandidate {
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }
    pub fn raw_count(self) -> usize {
        self.raw_count
    }
    pub fn preceded_by_backslash(self) -> bool {
        self.escaped
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityCandidate {
    start: usize,
    end: usize,
    value: EntityValue,
}

impl EntityCandidate {
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }
    pub fn decoded(self) -> EntityValue {
        self.value
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineCandidate {
    CodeDelimiter(CodeDelimiterCandidate),
    Entity(EntityCandidate),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineStepStatus {
    Progress,
    NeedInput,
    Candidate(InlineCandidate),
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineStep {
    pub work: InlineWork,
    pub status: InlineStepStatus,
}

#[derive(Clone, Copy, Debug)]
enum State {
    Normal,
    Backslash,
    Code {
        start: usize,
        count: usize,
        escaped: bool,
    },
    Entity {
        start: usize,
        scanner: EntityScanner,
    },
}

#[derive(Debug)]
/// 状态只含标量与固定实体scratch；不保存正文、行、code内容、AST或分隔符全局索引。
pub struct InlineLexicalCursor {
    revision: u64,
    source_end: usize,
    range_end: usize,
    next: usize,
    state: State,
    failed: Option<InlineError>,
    complete: bool,
}

impl InlineLexicalCursor {
    /// range由实际block producer选择；本入口只扫描候选，不凭range声明段落或最终样式。
    /// source_end是完整source长度，只有它能核对frame.real_eof；range.end不是frame尾。
    pub fn new(revision: u64, source_end: usize, range: Range<usize>) -> Result<Self, InlineError> {
        if range.start > range.end || range.end > source_end {
            return Err(InlineError::InvalidRange);
        }
        Ok(Self {
            revision,
            source_end,
            range_end: range.end,
            next: range.start,
            state: State::Normal,
            failed: None,
            complete: false,
        })
    }

    pub fn next_byte(&self) -> usize {
        self.next
    }

    /// 实际 EOL 已使词法回到 Normal；只接受同源前缀，不能在 tick/entity 中间跳字节。
    pub(crate) fn skip_prefix(
        &mut self,
        prefix: &crate::global_frontier::VerifiedInlinePrefix,
    ) -> Result<(), InlineError> {
        if self.failed.is_some()
            || self.complete
            || !matches!(self.state, State::Normal)
            || prefix.revision != self.revision
            || prefix.source_end != self.source_end
            || prefix.range.start != self.next
            || prefix.range.end > self.range_end
        {
            return Err(InlineError::InvalidFrame);
        }
        self.next = prefix.range.end;
        Ok(())
    }

    pub fn cancel(&mut self) {
        // 取消立即撤销scratch访问权限；状态不持heap，外层drop续体后释放其固定存储。
        // 后续即使同revision也不能复活已撤权候选扫描，不声称这些字节已经安全清零。
        self.state = State::Normal;
        self.failed = Some(InlineError::Cancelled);
    }

    /// 每step最多64KiB源读取/转换/静态lookup比较上界；只返回一条候选，绝不积累输出队列。
    ///
    /// # Errors
    /// 非法frame、非法fuel或计数溢出会永久封住续体并保首因；只有NeedInput可由后继frame继续。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<InlineStep, InlineError> {
        let result = self.advance_inner(frame, budget);
        if let Err(error) = result {
            // 所有错误都封住续体；不接受新frame或更大预算把首错改称完成。
            self.state = State::Normal;
            self.failed = Some(error);
        }
        result
    }

    fn advance_inner(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<InlineStep, InlineError> {
        if let Some(error) = self.failed {
            // 首错优先于新参数；后继frame不能重新领取已失败续体的权限。
            return Err(error);
        }
        if self.complete {
            return Err(InlineError::Closed);
        }
        if !(MIN_INLINE_STEP_WORK..=MAX_INLINE_STEP_WORK).contains(&budget) {
            return Err(InlineError::InvalidBudget);
        }
        // 完整frame准入在任何状态转换前完成；允许UTF8/CRLF在frame间切分，但不接受越界源。
        let frame_end = frame.byte_start.checked_add(frame.bytes.len());
        if frame.source_revision != self.revision
            || frame.byte_start != self.next
            || frame.bytes.len() > MAX_INLINE_FRAME_BYTES
            || !frame_end.is_some_and(|end| end <= self.source_end)
            || (frame.real_eof && frame_end != Some(self.source_end))
        {
            return Err(InlineError::InvalidFrame);
        }
        let mut work = InlineWork::default();
        loop {
            if self.next == self.range_end {
                if work.total() == budget {
                    return Ok(InlineStep {
                        work,
                        status: InlineStepStatus::Progress,
                    });
                }
                work.transitions += 1;
                // 只有显式逻辑range边界能结束一个run；用完当前frame绝不走这里。
                if let State::Code {
                    start,
                    count,
                    escaped,
                } = self.state
                {
                    self.state = State::Normal;
                    return Ok(InlineStep {
                        work,
                        status: InlineStepStatus::Candidate(InlineCandidate::CodeDelimiter(
                            CodeDelimiterCandidate {
                                revision: self.revision,
                                start,
                                end: self.next,
                                raw_count: count,
                                escaped,
                            },
                        )),
                    });
                }
                self.state = State::Normal;
                self.complete = true;
                return Ok(InlineStep {
                    work,
                    status: InlineStepStatus::Complete,
                });
            }
            let offset = self.next - frame.byte_start;
            if offset == frame.bytes.len() {
                // 包括未闭合实体、转义、tick run在内的状态均保留，等待真实后继源字节。
                return Ok(InlineStep {
                    work,
                    status: InlineStepStatus::NeedInput,
                });
            }
            if budget - work.total() < 2 {
                return Ok(InlineStep {
                    work,
                    status: InlineStepStatus::Progress,
                });
            }
            // 在真正解引用源byte前先准入peek+转换；剩余fuel不足时不能偷偷读下一个字节。
            let byte = frame.bytes[offset];
            work.byte_reads += 1;
            work.transitions += 1;
            let lookup_bound = match &self.state {
                State::Entity { scanner, .. } => scanner.lookup_work_bound(byte),
                _ => 0,
            };
            if budget - work.total() < lookup_bound {
                // 这次peek仍收费，但状态/源cursor未动；下step完整预算足以执行同一固定lookup。
                return Ok(InlineStep {
                    work,
                    status: InlineStepStatus::Progress,
                });
            }
            work.entity_lookup_byte_work_bound += lookup_bound;
            match &mut self.state {
                State::Normal => {
                    let at = self.next;
                    self.next += 1;
                    self.state = match byte {
                        b'\\' => State::Backslash,
                        b'`' => State::Code {
                            start: at,
                            count: 1,
                            escaped: false,
                        },
                        b'&' => State::Entity {
                            start: at,
                            scanner: EntityScanner::new(),
                        },
                        // ASCII标记以外字节不解码/复制；UTF8切frame不会创造语法token。
                        _ => State::Normal,
                    };
                }
                State::Backslash => {
                    if byte == b'`' {
                        // 原firstpass把转义后的整个run标记MaybeCode(count,true)，不把slash计入range。
                        self.state = State::Code {
                            start: self.next,
                            count: 1,
                            escaped: true,
                        };
                        self.next += 1;
                    } else {
                        self.state = State::Normal;
                        if is_ascii_punctuation(byte) {
                            // 原punctuation转义吞掉下一byte；转义`&`不能触发实体解码。
                            self.next += 1;
                        }
                        // 非punctuation当前byte不消费；下一转换按普通词法重放，也照常收费。
                    }
                }
                State::Code {
                    start,
                    count,
                    escaped,
                } => {
                    if byte == b'`' {
                        *count = count.checked_add(1).ok_or(InlineError::Overflow)?;
                        self.next += 1;
                    } else {
                        let candidate = CodeDelimiterCandidate {
                            revision: self.revision,
                            start: *start,
                            end: self.next,
                            raw_count: *count,
                            escaped: *escaped,
                        };
                        self.state = State::Normal;
                        // 只peek了非tick；它仍是下次起点，不跳过可能的另一个语法token。
                        return Ok(InlineStep {
                            work,
                            status: InlineStepStatus::Candidate(InlineCandidate::CodeDelimiter(
                                candidate,
                            )),
                        });
                    }
                }
                State::Entity { start, scanner } => match scanner.push(byte) {
                    EntityTransition::Continue => self.next += 1,
                    EntityTransition::Matched(value) => {
                        self.next += 1;
                        let candidate = EntityCandidate {
                            start: *start,
                            end: self.next,
                            value,
                        };
                        self.state = State::Normal;
                        return Ok(InlineStep {
                            work,
                            status: InlineStepStatus::Candidate(InlineCandidate::Entity(candidate)),
                        });
                    }
                    EntityTransition::Rejected => {
                        // 失败实体已读前缀只含无语法作用alnum/#/x，当前拒绝byte必须原位重放。
                        self.state = State::Normal;
                    }
                },
            }
        }
    }
}
