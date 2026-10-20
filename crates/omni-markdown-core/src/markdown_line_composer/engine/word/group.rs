//! 逻辑词组移动与硬行内选词共享同一个原GCB游标；本入口不声称提供Bidi视觉遍历。

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownWordGroupAction {
    Backward,
    Forward,
    /// bias只选择当前位置的左/右相邻组；真正行首/行尾沿实际相邻换行转向存在的本行侧。
    Select {
        backward_bias: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownWordGroupAnswer {
    Yielded,
    /// 真实已准备段没有无控制零级证明；该逻辑词组不提供 RTL 视觉遍历，也不毒化健康 Source。
    UnsupportedVisualOrder,
    /// 保留原 Step 实际工作；当前逻辑组不能跨尚未展开的隐藏 Source 范围继续读正文。
    NeedsFoldExpansion(Range<usize>),
    /// 原Source真实逻辑范围；移动以方向端点为head，Select使用完整范围，未授予Native提交权。
    Ready(Range<usize>),
}

pub struct MarkdownWordGroupStep {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
    pub answer: MarkdownWordGroupAnswer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownWordMoveScope {
    WholeUniformZero,
    /// 每个实际 GCB 必须再消费完整原 B 段证书；这项本身不授任何位置的零级权限。
    VisibleParagraphScopes,
    Unsupported,
}

/// 原 UBA 准备的真实工作量；None 仍在准备，Source 方向职责不会被可见子段偷换。
pub struct MarkdownWordMovePreparation {
    pub bytes: usize,
    pub scalars: usize,
    pub metadata_units: usize,
    pub scope: Option<MarkdownWordMoveScope>,
}

enum Phase {
    Move,
    Seed { backward: bool, fallback: bool },
    Left,
    Right,
    Complete(Range<usize>),
    Unsupported,
}

/// 两侧依次推进同一游标，阶段转换本身不再花第二份字节/记录量子。
pub(in crate::markdown_line_composer::engine) struct WordGroup {
    head: usize,
    category: Option<Category>,
    start: usize,
    phase: Phase,
    cursor: WordDelete,
    restore_position: Option<usize>,
    deferred_answer: Option<MarkdownWordGroupAnswer>,
}

impl WordGroup {
    fn new(action: MarkdownWordGroupAction, head: usize, length: usize) -> Self {
        let (direction, mode, phase) = match action {
            // 逻辑前/后移动从原head起，跳全部leading空白后消费一个类别；不是删除的单空格规则。
            MarkdownWordGroupAction::Backward => (
                MarkdownWordDeleteDirection::Backward,
                CursorMode::Move,
                Phase::Move,
            ),
            // 向后继方向仍使用同一移动规则，不把连续TAB/换行当成删除的独立一步。
            MarkdownWordGroupAction::Forward => (
                MarkdownWordDeleteDirection::Forward,
                CursorMode::Move,
                Phase::Move,
            ),
            MarkdownWordGroupAction::Select { backward_bias } => {
                // 点击侧只决定首个类别；全源端点强制转向存在的一侧，硬行端点随后由真实换行核准。
                let backward = head != 0 && (head == length || backward_bias);
                (
                    if backward {
                        MarkdownWordDeleteDirection::Backward
                    } else {
                        MarkdownWordDeleteDirection::Forward
                    },
                    CursorMode::Seed,
                    Phase::Seed {
                        backward,
                        fallback: false,
                    },
                )
            }
        };
        Self {
            head,
            category: None,
            start: head,
            phase,
            cursor: WordDelete::group(direction, head, mode),
            restore_position: None,
            deferred_answer: None,
        }
    }

    fn advance(
        &mut self,
        source: &MarkdownLayoutTextSource,
        coordinator: &mut MarkdownLineItemCoordinator,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordGroupStep> {
        if self.deferred_answer.is_some() {
            let position = self
                .restore_position
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let (actual, restored) = coordinator.restore_word_layout_context(position, budget)?;
            let answer = if restored {
                self.restore_position = None;
                self.deferred_answer
                    .take()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            } else {
                MarkdownWordGroupAnswer::Yielded
            };
            return Ok(MarkdownWordGroupStep {
                bytes: actual.bytes,
                scalars: actual.scalars,
                records: actual.records,
                metadata_units: actual.metadata_units,
                answer,
            });
        } // 两个实际段槽仍由原 Bidi owner 管理；恢复量子不再兼花 GCB/分类预算。
        if matches!(self.phase, Phase::Unsupported) {
            return Ok(self.defer_scoped_answer(MarkdownWordGroupStep {
                bytes: 0,
                scalars: 0,
                records: 0,
                metadata_units: 0,
                answer: MarkdownWordGroupAnswer::UnsupportedVisualOrder,
            }));
        } // 外层 Composer 已核原 Source 与取消；能力拒绝不重新读取未知方向的正文。
        let step = self
            .cursor
            .advance(source, coordinator, authority, budget)?;
        if self.cursor.visual_rejected {
            self.phase = Phase::Unsupported;
            return Ok(self.defer_scoped_answer(MarkdownWordGroupStep {
                bytes: step.bytes,
                scalars: step.scalars,
                records: step.records,
                metadata_units: step.metadata_units,
                answer: MarkdownWordGroupAnswer::UnsupportedVisualOrder,
            }));
        } // 拒绝仍交实际本步工作；不能把同一游标已做的段准备或 I/O 擦成零。
        let mut answer = match &step.answer {
            MarkdownWordDeleteAnswer::NeedsFoldExpansion(hidden) => {
                MarkdownWordGroupAnswer::NeedsFoldExpansion(hidden.clone())
            }
            _ => MarkdownWordGroupAnswer::Yielded,
        };
        if let MarkdownWordDeleteAnswer::Ready(range) = step.answer {
            match &self.phase {
                Phase::Move => {
                    // 单方向移动已到真实组界；只交源范围，Native仍须持原permit更新选择。
                    self.phase = Phase::Complete(range.clone());
                    answer = MarkdownWordGroupAnswer::Ready(range);
                }
                Phase::Seed { backward, fallback } => {
                    // 硬行首尾不能选到相邻行；首侧确无本行GCB时只尝试另一侧一次。
                    if range.is_empty() || self.cursor.seed_newline {
                        if !fallback {
                            let backward = !backward;
                            self.phase = Phase::Seed {
                                backward,
                                fallback: true,
                            };
                            self.cursor = WordDelete::group(
                                if backward {
                                    MarkdownWordDeleteDirection::Backward
                                } else {
                                    MarkdownWordDeleteDirection::Forward
                                },
                                self.head,
                                CursorMode::Seed,
                            );
                        } else {
                            let range = self.head..self.head;
                            self.phase = Phase::Complete(range.clone());
                            answer = MarkdownWordGroupAnswer::Ready(range);
                        } // 两侧都是换行/端点才是真空行，不能选取相邻行里的一个词。
                    } else {
                        // 两侧扩张都锁定这个实际完整GCB的类别，不能左边挑词、右边另挑标点。
                        let category = self
                            .cursor
                            .category
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        self.category = Some(category);
                        self.phase = Phase::Left;
                        self.cursor = WordDelete::group(
                            MarkdownWordDeleteDirection::Backward,
                            self.head,
                            CursorMode::Expand(category),
                        );
                    }
                }
                Phase::Left => {
                    // 左端已固定，归还旧半边游标后用同一Source向右；每回合仍只有一份预算。
                    self.start = range.start;
                    self.phase = Phase::Right;
                    self.cursor = WordDelete::group(
                        MarkdownWordDeleteDirection::Forward,
                        self.head,
                        CursorMode::Expand(
                            self.category
                                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                        ),
                    );
                }
                Phase::Right => {
                    // 两侧都由同一个head和同一类别证实，组合完整硬行内选词范围。
                    let range = self.start..range.end;
                    self.phase = Phase::Complete(range.clone());
                    answer = MarkdownWordGroupAnswer::Ready(range);
                }
                Phase::Complete(range) => {
                    answer = MarkdownWordGroupAnswer::Ready(range.clone()); // 终态仍经过同原Source/GCB健康门。
                }
                Phase::Unsupported => unreachable!("能力拒绝已经在推进原游标前返回"),
            }
        }
        Ok(self.defer_scoped_answer(MarkdownWordGroupStep {
            bytes: step.bytes,
            scalars: step.scalars,
            records: step.records,
            metadata_units: step.metadata_units,
            answer,
        }))
    }

    /// 只有 scoped 终点先让原布局段恢复；原 Root 快路和所有逻辑组没有额外阶段或读取。
    fn defer_scoped_answer(&mut self, mut step: MarkdownWordGroupStep) -> MarkdownWordGroupStep {
        if self.restore_position.is_some()
            && !matches!(step.answer, MarkdownWordGroupAnswer::Yielded)
        {
            self.deferred_answer = Some(step.answer);
            step.answer = MarkdownWordGroupAnswer::Yielded;
        }
        step
    }
}

impl MarkdownLineComposer {
    /// 按词视觉移动先确认原 Coordinator 的实际职责；每次只消费原一份准备量子。
    ///
    /// # Errors
    /// 原 Source、取消和实际 Bidi 存储错误沿原失败关闭；能力结果只声明后续职责，不签未分析位置。
    pub fn prepare_word_move(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordMovePreparation> {
        let result = (|| {
            self.guard()?;
            if budget.bytes == 0
                || budget.scalars == 0
                || budget.records == 0
                || budget.metadata_units == 0
            {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let budget = MarkdownLineComposeBudget {
                bytes: budget.bytes.min(4096),
                scalars: budget.scalars.min(256),
                records: budget.records.min(256),
                metadata_units: budget.metadata_units.min(256),
            };
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let (step, scope) = payload.coordinator.prepare_word_move(budget)?;
            Ok(MarkdownWordMovePreparation {
                bytes: step.bytes,
                scalars: step.scalars,
                metadata_units: step.metadata_units,
                scope,
            })
        })();
        self.finish(result)
    }

    /// 在原Source的完整GCB位置开始一次逻辑移动/选词；不创建新索引、Reader或正文副本。
    ///
    /// # Errors
    /// 原Source/取消先于范围错误；现有词组动作尚未取消时不覆盖它。
    pub fn begin_word_group(
        &mut self,
        action: MarkdownWordGroupAction,
        head_utf16: usize,
    ) -> Result<()> {
        self.begin_group(action, head_utf16, false)
    }

    /// 原视觉移动在完整 Root 证书下保持快路；显式折叠职责则逐 GCB 核原可见段，绝不据逻辑序猜 RTL。
    pub fn begin_visual_word_group(
        &mut self,
        direction: MarkdownWordDeleteDirection,
        head_utf16: usize,
    ) -> Result<()> {
        let action = match direction {
            MarkdownWordDeleteDirection::Backward => MarkdownWordGroupAction::Backward,
            MarkdownWordDeleteDirection::Forward => MarkdownWordGroupAction::Forward,
        };
        self.begin_group(action, head_utf16, true)
    }

    /// 两种实际消费者共享同一个 Source/GCB owner；调用方传视觉意图，权限只能由原 Coordinator 给出。
    fn begin_group(
        &mut self,
        action: MarkdownWordGroupAction,
        head_utf16: usize,
        visual: bool,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if payload.word_delete.is_some() || head_utf16 > payload.source.utf16_len() {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        let scoped = if visual {
            match payload.coordinator.word_move_scope()? {
                Some(MarkdownWordMoveScope::WholeUniformZero) => false,
                Some(MarkdownWordMoveScope::VisibleParagraphScopes) => true,
                _ => return Err(MarkdownLineComposeError::InvalidRequest),
            }
        } else {
            false
        };
        let mut group = WordGroup::new(action, head_utf16, payload.source.utf16_len());
        group.cursor.visible_word = scoped;
        if scoped {
            group.restore_position = Some(payload.coordinator.word_layout_position()?);
        }
        payload.word_delete = Some(WordJob::Group(group));
        Ok(())
    }

    /// 每次仅推进当前一侧的一份原4KiB/256标量/GCB记录量子，巨大词或单簇继续让出。
    ///
    /// # Errors
    /// 非原GCB、实际源/索引失败和取消沿同一Composer真实失败关闭；不拿后继Source续算。
    pub fn step_word_group(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordGroupStep> {
        let result = (|| {
            self.guard()?;
            if budget.bytes == 0
                || budget.scalars == 0
                || budget.records == 0
                || budget.metadata_units == 0
            {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let budget = MarkdownLineComposeBudget {
                bytes: budget.bytes.min(4096),
                scalars: budget.scalars.min(256),
                records: budget.records.min(256),
                metadata_units: budget.metadata_units.min(256),
            };
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let Some(WordJob::Group(group)) = payload.word_delete.as_mut() else {
                return Err(MarkdownLineComposeError::InvalidRequest);
            };
            group.advance(
                &payload.source,
                &mut payload.coordinator,
                &self.authority,
                budget,
            )
        })();
        self.finish(result)
    }

    /// 仅取消本group游标；原Source、字体与布局依赖仍归原Composer关闭，不影响另一个删除动作。
    pub fn cancel_word_group(&mut self) {
        if let Some(payload) = self.payload.as_mut()
            && matches!(payload.word_delete, Some(WordJob::Group(_)))
        {
            payload.word_delete = None;
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod move_tests;

#[cfg(test)]
mod visible_tests;
