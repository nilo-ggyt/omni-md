// Copyright 2015 Google Inc. All rights reserved.
// 续行prefix复用固定0.13.4原容器/空白操作；完整MIT条款见LICENSE。

//! 已验证definition的有限重读：容器去前缀与label/title归一化是不同用途，不相互偷换。

use super::*;
use crate::CowStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferencePrefixPurpose {
    ContainerOnly,
    Label,
    Whitespace,
    Title,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferencePrefixStep {
    Progress {
        next_byte: usize,
        work: GlobalWork,
    },
    NeedInput {
        next_byte: usize,
        work: GlobalWork,
    },
    Complete {
        prefix_bytes: usize,
        work: GlobalWork,
    },
}
enum ScopePhase {
    Anchor,
    AfterCr,
    Containers,
    Space(LineStartStream),
    TitleSpace(LineStartStream),
    Complete(usize),
}

/// 只强持有限spine metadata；没有source映射、正文/URL/title或任意第三方slice的长期借用。
pub struct ReferencePrefixScanner {
    revision: u64,
    source_end: usize,
    origin: usize,
    spine: StructuralSpineSnapshot,
    options: Options,
    purpose: ReferencePrefixPurpose,
    phase: ScopePhase,
    prefix: Option<ContainerPrefix>,
    failure: Option<GlobalFrontierError>,
}
impl std::fmt::Debug for ReferencePrefixScanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReferencePrefixScanner")
            .field("revision", &self.revision)
            .field("line_start", &self.origin)
            .field("depth", &self.spine.len())
            .field("purpose", &self.purpose)
            .finish()
    }
}
impl VerifiedGlobalReference {
    /// 调用者仍须以同一实际snapshot/root取字节；本方法核候选范围，后续再核真实CR/LF行边界。
    /// Label/Whitespace包含原Space(4)，Title再包含AllSpace；ContainerOnly保留额外indent给原Parser。
    /// 失败/未完成不会返回可用prefix，调用者不能用零skip默认值伪造已完成解析。
    pub fn prefix_scanner(
        &self,
        line_start: usize,
        purpose: ReferencePrefixPurpose,
    ) -> Result<ReferencePrefixScanner, GlobalFrontierError> {
        if line_start <= self.candidate.span_start || line_start >= self.candidate.span_end {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        Ok(ReferencePrefixScanner {
            revision: self.candidate.source_revision,
            source_end: self.source_end,
            origin: line_start,
            spine: self.spine.clone(),
            options: self.options,
            purpose,
            phase: ScopePhase::Anchor,
            prefix: None,
            failure: None,
        })
    }

    /// 仅在有限raw label与全部scope prefix已准备好后委托原Unicode/escape oracle。
    /// continuation_prefix必须查先前分段probe的结果；不可在闭包中同步遍历任意深spine×所有行。
    pub fn normalized_label<'a>(
        &self,
        raw_with_closer: &'a str,
        continuation_prefix: impl Fn(&[u8]) -> Option<usize>,
    ) -> Result<CowStr<'a>, crate::block_frontier::FrontierError> {
        self.candidate
            .normalized_label(raw_with_closer, continuation_prefix)
    }
}
impl ReferencePrefixScanner {
    pub fn next_byte(&self) -> usize {
        match &self.phase {
            ScopePhase::Anchor => self.origin - 1,
            ScopePhase::AfterCr => self.origin,
            ScopePhase::Containers => self
                .prefix
                .as_ref()
                .expect("Containers阶段拥有游标")
                .next_byte(),
            ScopePhase::Space(operation) | ScopePhase::TitleSpace(operation) => {
                operation.next_byte()
            }
            ScopePhase::Complete(prefix) => self.origin + prefix,
        }
    }
    /// 固定workspace/状态收尾预留1024转换；剩余工作严格交给共享可恢复原语，最小step为2048。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<ReferencePrefixStep, GlobalFrontierError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if authority.cancelled {
            self.failure = Some(GlobalFrontierError::Cancelled);
            return Err(GlobalFrontierError::Cancelled);
        }
        if authority.current_revision != self.revision {
            self.failure = Some(GlobalFrontierError::SourceSuperseded);
            return Err(GlobalFrontierError::SourceSuperseded);
        }
        if budget < 2048 {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        crate::source_frontier::admitted_bytes(
            &frame,
            self.revision,
            self.source_end,
            self.next_byte(),
            budget,
        )?;
        match self.advance_inner(frame, authority, budget) {
            Ok(step) => Ok(step),
            Err(error) => {
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
    ) -> Result<ReferencePrefixStep, GlobalFrontierError> {
        if authority.cancelled {
            return Err(GlobalFrontierError::Cancelled);
        }
        if authority.current_revision != self.revision {
            return Err(GlobalFrontierError::SourceSuperseded);
        }
        if budget < 2048 {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        crate::source_frontier::admitted_bytes(
            &frame,
            self.revision,
            self.source_end,
            self.next_byte(),
            budget,
        )?;
        let phase = std::mem::replace(&mut self.phase, ScopePhase::Anchor);
        let title_phase = matches!(phase, ScopePhase::TitleSpace(_));
        let mut work = GlobalWork {
            byte_reads: 0,
            transitions: 1024,
        };
        let mut need = false;
        let fuel = (budget - 1024) / 4;
        let before = self.spine.work_used();
        match phase {
            ScopePhase::Anchor => {
                match frame.bytes.first() {
                    Some(b'\n') => self.start_containers(),
                    Some(b'\r') => self.phase = ScopePhase::AfterCr,
                    Some(_) | None if frame.real_eof => {
                        return Err(GlobalFrontierError::InvalidFrame)
                    }
                    None => {
                        need = true;
                        self.phase = ScopePhase::Anchor;
                    }
                    _ => return Err(GlobalFrontierError::InvalidFrame),
                }
                if !frame.bytes.is_empty() {
                    work.byte_reads = 1;
                    work.transitions += 1;
                }
            }
            ScopePhase::AfterCr => {
                if frame.bytes.first() == Some(&b'\n') {
                    return Err(GlobalFrontierError::InvalidFrame);
                }
                if frame.bytes.is_empty() && !frame.real_eof {
                    need = true;
                    self.phase = ScopePhase::AfterCr;
                } else {
                    work.byte_reads = usize::from(!frame.bytes.is_empty());
                    work.transitions += 1;
                    self.start_containers();
                }
            }
            ScopePhase::Containers => {
                let (step, child) = self
                    .prefix
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .advance(&frame, fuel, fuel)?;
                work.byte_reads = child.byte_reads;
                work.transitions += child.transitions;
                match step {
                    PrefixProgress::Complete { state, .. } => {
                        self.prefix = None;
                        if self.purpose == ReferencePrefixPurpose::ContainerOnly {
                            self.phase = ScopePhase::Complete(state.ix);
                        } else {
                            self.phase = ScopePhase::Space(LineStartStream::new(
                                self.revision,
                                self.origin,
                                self.source_end,
                                state,
                                LineStartOperation::Space(4),
                            )?);
                        }
                    }
                    PrefixProgress::Progress => self.phase = ScopePhase::Containers,
                    PrefixProgress::NeedInput { .. } => {
                        need = true;
                        self.phase = ScopePhase::Containers;
                    }
                }
            }
            ScopePhase::Space(mut operation) | ScopePhase::TitleSpace(mut operation) => {
                let title = title_phase;
                let step = operation.advance(
                    LineStartFrame {
                        source_revision: frame.source_revision,
                        byte_start: frame.byte_start,
                        bytes: frame.bytes,
                        real_eof: frame.real_eof,
                    },
                    fuel,
                    fuel,
                )?;
                work.byte_reads = step.work.byte_reads;
                work.transitions += step.work.transitions;
                match step.progress {
                    LineStartProgress::Complete { state, .. } => {
                        if self.purpose == ReferencePrefixPurpose::Title && !title {
                            self.phase = ScopePhase::TitleSpace(LineStartStream::new(
                                self.revision,
                                self.origin,
                                self.source_end,
                                state,
                                LineStartOperation::AllSpace,
                            )?);
                        } else {
                            self.phase = ScopePhase::Complete(state.ix);
                        }
                    }
                    LineStartProgress::Progress { .. } | LineStartProgress::NeedInput { .. } => {
                        need = matches!(step.progress, LineStartProgress::NeedInput { .. });
                        self.phase = if title {
                            ScopePhase::TitleSpace(operation)
                        } else {
                            ScopePhase::Space(operation)
                        };
                    }
                }
            }
            ScopePhase::Complete(prefix) => self.phase = ScopePhase::Complete(prefix),
        }
        work.transitions += self.spine.work_used() - before;
        debug_assert!(work.byte_reads <= budget && work.transitions <= budget);
        if let ScopePhase::Complete(prefix_bytes) = self.phase {
            Ok(ReferencePrefixStep::Complete { prefix_bytes, work })
        } else if need {
            Ok(ReferencePrefixStep::NeedInput {
                next_byte: self.next_byte(),
                work,
            })
        } else {
            Ok(ReferencePrefixStep::Progress {
                next_byte: self.next_byte(),
                work,
            })
        }
    }
    fn start_containers(&mut self) {
        self.prefix = Some(ContainerPrefix::new(
            self.revision,
            self.origin,
            self.source_end,
            &self.spine,
            self.options,
        ));
        self.phase = ScopePhase::Containers;
    }
}
