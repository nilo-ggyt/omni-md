// Copyright 2015 Google Inc. All rights reserved.
// 原 capture 与续行前缀的有限消费；MIT 许可见 LICENSE。

//! 与主 code 游标共用一个块、spine 和累计预算；不保存行目录或第二份正文。

use super::*;
use crate::global_frontier::{
    InlinePlainContainers, ReferencePrefixPurpose, ReferencePrefixScanner, ReferencePrefixStep,
    VerifiedInlinePrefix, VerifiedInlineSemanticSeed,
};
use crate::structural_spine::{StructuralCursorStep, StructuralKind, StructuralSpineCursor};

pub(super) struct ScopedCode {
    // 原 seed 唯一保留闭合块与祖先来源；角色目录仅在初始化量子活跃，随后逐续行临时建立。
    // 它不持 mmap、String 或原文档；源寿命仍由外层 SourceRequest 和原 Doc 管理。
    pub(super) seed: std::sync::Arc<VerifiedInlineSemanticSeed>,
    scope: Option<StructuralSpineCursor>,
    scope_seen: bool,
    containers: InlinePlainContainers,
    // 此位置来自刚消费的真实换行，bool 只区分尚待确认的 CRLF；不是行号或预测高度。
    continuation: Option<(usize, bool)>,
    pub(super) prefix: Option<ReferencePrefixScanner>,
    prefix_start: usize,
    // 原配额按真实取得目录的累计增量收费；归还目录不重置计量，后继不能重新领取 fuel。
    allocated: usize,
}
impl std::fmt::Debug for ScopedCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedCode")
            .field("containers", &self.containers)
            .field("continuation", &self.continuation)
            .finish()
    }
}
impl ScopedCode {
    pub(super) fn new(seed: VerifiedInlineSemanticSeed) -> Self {
        Self {
            scope: Some(seed.spine.cursor()),
            seed: std::sync::Arc::new(seed),
            scope_seen: false,
            containers: InlinePlainContainers::default(),
            continuation: None,
            prefix: None,
            prefix_start: 0,
            allocated: 0,
        }
    }
}
impl InlineCodeCursor {
    /// 累积实际取得的目录 payload；Core 将每次增量记入原请求，不把 scope 工作区藏进固定句柄。
    pub fn allocated_workspace_bytes(&self) -> usize {
        self.scoped.as_ref().map_or(0, |scope| scope.allocated)
    }

    /// 这些种类只来自原 capture；成功 publication 才能把它们转成可查询样式。
    pub fn containers(&self) -> InlinePlainContainers {
        self.scoped
            .as_ref()
            .map_or(InlinePlainContainers::default(), |scope| scope.containers)
    }

    /// 每步只读一页或一个真实角色；脚注/定义等未接投影的祖先不能降为 Quote/List。
    pub(super) fn advance_scope(&mut self) -> Result<InlineCodeStep, Error> {
        let scope = self.scoped.as_mut().ok_or(Error::InvalidFrame)?;
        // 目录和原 spine 都是真实共享资源；本步只收实际新增 payload 与 ledger 增量。
        // 角色不匹配会保留未知，后继仍由主游标的失败/取消路径释放同一 owner。
        let before = scope.seed.spine.work_used();
        let cursor = scope.scope.as_mut().ok_or(Error::InvalidFrame)?;
        let allocated = cursor.allocated_workspace_bytes();
        let step = cursor.next()?;
        scope.allocated += cursor.allocated_workspace_bytes() - allocated;
        let mut work = GlobalWork {
            byte_reads: 0,
            transitions: CONTROL + 1,
        };
        match step {
            StructuralCursorStep::Building => {}
            StructuralCursorStep::Entry { index, entry } => {
                // 最后一项必须是原 Paragraph 本身，之前的 Quote/List/Item 才能成为继承事实。
                // 相同 marker 文本不能补出角色，定义与脚注也不能落入普通列表分支。
                if index == scope.seed.spine.len() - 1 {
                    if entry.kind != StructuralKind::Paragraph
                        || entry.source_start != scope.seed.start
                    {
                        return Err(Error::InvalidFrame);
                    }
                    scope.scope_seen = true;
                } else {
                    match entry.kind {
                        StructuralKind::Quote => scope.containers.quote = true,
                        StructuralKind::List { marker, .. } => match marker {
                            b'-' | b'+' | b'*' => scope.containers.unordered_list = true,
                            b'.' | b')' => scope.containers.ordered_list = true,
                            _ => return Err(Error::InvalidFrame),
                        },
                        StructuralKind::Item { .. } => {}
                        // 标题投影不绘制脚注容器样式；来源仍由原seed及原Footnote前缀规则完整证明。
                        StructuralKind::Footnote { .. } if self.heading => {}
                        _ => self.phase = Phase::General,
                    }
                }
            }
            StructuralCursorStep::End => {
                if !scope.scope_seen {
                    return Err(Error::InvalidFrame);
                }
                scope.scope = None;
                self.phase = Phase::Normal;
                work.transitions += 128; // 真正归还本活动角色目录，不逐行重新构造正文语义任务。
                                         // 首行前缀已在原 opening capture 中证明；只发布它的 canonical 范围。
                                         // 后续行必须另经原 PrefixScanner，不能套用首行的字节宽度。
                if scope.seed.physical_start < scope.seed.start {
                    let proof = VerifiedInlinePrefix {
                        revision: self.revision,
                        source_end: self.source_end,
                        range: scope.seed.physical_start..scope.seed.start,
                        block_source: scope.seed.source_range(),
                        purpose: None,
                    };
                    work.transitions += scope.seed.spine.work_used() - before;
                    return Ok(InlineCodeStep::Prefix { proof, work });
                }
            }
        }
        work.transitions += scope.seed.spine.work_used() - before;
        Ok(if matches!(self.phase, Phase::General) {
            InlineCodeStep::NeedsGeneralInline { work }
        } else {
            InlineCodeStep::Progress { work }
        })
    }

    /// 只能在实际主扫描/原 probe/trim 消费 CR 或 LF 后调用；位置不是预测的下一行坐标。
    pub(super) fn arm_continuation(&mut self, at: usize, eol: u8) {
        if at >= self.end {
            return;
        }
        if let Some(scope) = self.scoped.as_mut() {
            scope.continuation = Some((at, eol == b'\r'));
        }
    }

    /// 前缀解析是原 scope 的续体；CRLF 分帧时先让原消费者读 LF，再从其后建立真实 Anchor。
    pub(super) fn advance_prefix(
        &mut self,
        frame: &SourceFrame<'_>,
        budget: usize,
    ) -> Result<Option<InlineCodeStep>, Error> {
        let Some(scope) = self.scoped.as_mut() else {
            return Ok(None);
        };
        // 新前缀与已运行前缀不能同时活跃。先建立续体后归还给调用者，下一量子再读 Anchor。
        // trim 的 closer 前端是更窄的真实边界；触到它时无需为代码外的内容创建前缀任务。
        if let Some((at, after_cr)) = scope.continuation.take() {
            let limit = self
                .trim
                .as_ref()
                .map_or(self.end, |trim| trim.candidate.raw_body_range().end);
            if at >= limit {
                return Ok(None);
            }
            let mut work = GlobalWork {
                byte_reads: 0,
                transitions: CONTROL,
            };
            if after_cr {
                let Some(&byte) = frame.bytes.first() else {
                    scope.continuation = Some((at, after_cr));
                    return Ok(Some(InlineCodeStep::NeedInput { work }));
                };
                work.byte_reads += 1;
                work.transitions += 1;
                if byte == b'\n' {
                    return Ok(Some(InlineCodeStep::Progress { work }));
                } // LF 尚未被词法/trim 消费；下一量子仍走原消费者，不能绕过它的换行状态。
            }
            // Scanner 再读原 EOL，并沿同一 capture 匹配所有祖先；当前猜测位置本身不授予跳过。
            // 仅存这一行的活动游标，完成后马上释放，滚动不会积累一张全段落行目录。
            scope.prefix_start = at;
            // 原 make_code_span 只跳容器；code 中额外空白和 Tab 是正文，必须参与真实 trim。
            // 普通段落才执行 Space4/AllSpace；选择来自当前实际阶段，不接受外部语法布尔量。
            let purpose = if matches!(self.phase, Phase::Probe | Phase::Trim | Phase::Html) {
                ReferencePrefixPurpose::ContainerOnly
            } else {
                ReferencePrefixPurpose::Paragraph
            };
            scope.prefix = Some(scope.seed.continuation_prefix_scanner(at, purpose)?);
            return Ok(Some(InlineCodeStep::Progress { work }));
        }
        let Some(prefix) = scope.prefix.as_mut() else {
            return Ok(None);
        };
        // Scanner 内部的 Space4/AllSpace 和角色 ledger 已收费；这里仅转交其真实工作。
        // 分片结束会保留续体，既不虚构 EOF，也不把尚未完成的 prefix 当作零长度成功。
        let before = prefix.allocated_workspace_bytes();
        let step = prefix.advance(
            SourceFrame {
                source_revision: frame.source_revision,
                byte_start: frame.byte_start,
                bytes: frame.bytes,
                real_eof: frame.real_eof,
            },
            GlobalSourceAuthority {
                current_revision: self.revision,
                cancelled: false,
            },
            budget,
        )?;
        scope.allocated += prefix.allocated_workspace_bytes() - before;
        let (work, end) = match step {
            ReferencePrefixStep::Progress { work, .. } => {
                return Ok(Some(InlineCodeStep::Progress { work }));
            }
            ReferencePrefixStep::NeedInput { work, .. } => {
                return Ok(Some(InlineCodeStep::NeedInput { work }));
            }
            ReferencePrefixStep::Complete { prefix_bytes, work } => {
                (work, scope.prefix_start + prefix_bytes)
            }
        };
        if end > self.end {
            return Err(Error::InvalidFrame);
        }
        // 只有 Complete 能签发本票；低层 probe 还会核同 revision/source_end 与精确 next。
        // 该范围完全位于原块内，source 坐标不会被去前缀后的逻辑宽度替代。
        let proof = VerifiedInlinePrefix {
            revision: self.revision,
            source_end: self.source_end,
            range: scope.prefix_start..end,
            block_source: scope.seed.source_range(),
            purpose: Some(
                if matches!(self.phase, Phase::Probe | Phase::Trim | Phase::Html) {
                    ReferencePrefixPurpose::ContainerOnly
                } else {
                    ReferencePrefixPurpose::Paragraph
                },
            ),
        };
        scope.prefix = None;
        if matches!(self.phase, Phase::Probe) {
            self.probe
                .as_mut()
                .ok_or(Error::InvalidFrame)?
                .skip_prefix(&proof)
                .map_err(inline_error)?;
            // 前探只找 closer，不签可见前缀。匹配后由真实 Trim 发布；失配则原 Normal 重放发布。
            return Ok(Some(InlineCodeStep::Progress { work }));
        }
        // Normal 和 Trim 已真实消费换行，仍保原前一个 Unicode scalar/逻辑空格状态。
        // 跳过结构前缀不能把 >、* 或 tab 注入 flanking，也不能改变 code 的 all-spaces 判定。
        if matches!(self.phase, Phase::Html) {
            if !self
                .html_scanner
                .as_mut()
                .ok_or(Error::InvalidFrame)?
                .continue_line(proof.range.len())
            {
                return Err(Error::InvalidFrame);
            }
            self.next = end;
        } else if matches!(self.phase, Phase::Trim) {
            self.trim.as_mut().ok_or(Error::InvalidFrame)?.next = end;
        } else if matches!(self.phase, Phase::Normal | Phase::Backslash) {
            self.next = end;
        } else {
            return Err(Error::InvalidFrame);
        }
        Ok(Some(if proof.range.is_empty() {
            InlineCodeStep::Progress { work }
        } else {
            InlineCodeStep::Prefix { proof, work }
        }))
    }

    /// 只把真实 EOL 交给原 probe 后暂停；前扫每个字节也计费，不把 frame 裁边冒充 EOF。
    pub(super) fn probe_frame<'a>(
        &self,
        frame: &SourceFrame<'a>,
        budget: usize,
        work: &mut GlobalWork,
    ) -> Result<(SourceFrame<'a>, Option<(usize, u8)>), Error> {
        let mut count = frame.bytes.len();
        let mut boundary = None;
        if self.scoped.is_some() {
            // 原 probe 可能在 entity/tick 中暂存；裁边只限制可借字节，不改变其原 range_end。
            // 预留一次最坏实体 lookup 后再逐字节找 EOL，每个前扫字节和转换均进入本量子。
            let reserve = crate::inline_frontier::MIN_INLINE_STEP_WORK + 1;
            count = count.min(budget.saturating_sub(CONTROL + reserve) / 4);
            if count == 0 && !frame.bytes.is_empty() {
                return Err(Error::InvalidBudget);
            }
            for (offset, &byte) in frame.bytes[..count].iter().enumerate() {
                work.byte_reads += 1;
                work.transitions += 1;
                if matches!(byte, b'\r' | b'\n') {
                    count = offset + 1;
                    boundary = Some((frame.byte_start + count, byte));
                    break;
                }
            }
        }
        // 只有真实源末尾且本次没有裁掉任何字节才保留 EOF；行尾和视口尾都没有此权限。
        // 子 probe 尚未走到该 EOL 时不会启动 prefix，下次仍从原实际 next 再借小帧。
        Ok((
            SourceFrame {
                source_revision: frame.source_revision,
                byte_start: frame.byte_start,
                bytes: &frame.bytes[..count],
                real_eof: frame.real_eof && count == frame.bytes.len(),
            },
            boundary,
        ))
    }
}
