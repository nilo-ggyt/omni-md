//! 词组删除沿原GCB和冻结piece逐量子查找；不以视口、分块或软换行代替真实边界。

use super::*;
use crate::{MarkdownDocumentBufferError, MarkdownGraphemeBoundary, MarkdownLineBreakBudget};
use std::ops::Range;

mod group;
pub use group::{
    MarkdownWordGroupAction, MarkdownWordGroupAnswer, MarkdownWordGroupStep,
    MarkdownWordMovePreparation, MarkdownWordMoveScope,
};

/// 同一Composer只拥有一个词组动作；移动/选词与删除共享原GCB，不能并行创建第二套索引。
pub(super) enum WordJob {
    Delete(WordDelete),
    Group(group::WordGroup),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownWordDeleteDirection {
    Backward,
    Forward,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownWordDeleteAnswer {
    Yielded,
    /// 同一量子已到真实 opaque 端点；调用方须展开原 fold 并重建读图，不能据此删除隐区。
    NeedsFoldExpansion(Range<usize>),
    /// 只交原Source内的真实删除范围；调用方仍须消费原输入permit，不能据此重签当前正文。
    Ready(Range<usize>),
}

pub struct MarkdownWordDeleteStep {
    pub bytes: usize,
    pub scalars: usize,
    /// GCB记录工作量；Dense的整数记录访问也占量子，不冒充实际pread次数。
    pub records: usize,
    pub metadata_units: usize,
    pub answer: MarkdownWordDeleteAnswer,
}

impl MarkdownWordDeleteStep {
    fn yielded() -> Self {
        Self {
            bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            answer: MarkdownWordDeleteAnswer::Yielded,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(in crate::markdown_line_composer) enum Category {
    Space,
    Word,
    Other,
}

#[derive(Clone, Copy)]
/// 真实消费者共享GCB游标但保各自停止规则；普通删除不继承词类别，移动不继承单空格特例。
enum CursorMode {
    Delete,
    /// 普通删除只消费相邻的一个真实GCB，不读取词类别，也不依赖软换行几何。
    Grapheme,
    Move,
    Seed,
    Expand(Category),
}

/// 二分只保原稳定ordinal区间；没有读到的边界不得从坐标或EOF长度推算出来。
struct Seek {
    low: usize,
    high: usize,
    candidate: Option<(usize, MarkdownGraphemeBoundary)>,
}

/// 单个GCB也可远大于4KiB；这里只累计类别和一个UTF8半标量，不保留整簇字节。
pub(in crate::markdown_line_composer) struct Cluster {
    next_ordinal: usize,
    next: MarkdownGraphemeBoundary,
    byte: usize,
    end: MarkdownGraphemeBoundary,
    utf16: usize,
    pending: [u8; 4],
    used: usize,
    needed: usize,
    word: bool,
    space: bool,
    newline: bool,
    single_space: bool,
}

impl Cluster {
    pub(in crate::markdown_line_composer) fn new(
        next_ordinal: usize,
        current: MarkdownGraphemeBoundary,
        next: MarkdownGraphemeBoundary,
    ) -> Result<Self> {
        let (start, end) = if current.byte < next.byte {
            (current, next)
        } else {
            (next, current)
        };
        if start.byte >= end.byte || start.utf16 >= end.utf16 {
            return Err(MarkdownLineComposeError::InconsistentCoverage);
        } // 原相邻GCB必须在byte和UTF16两轴同时严格递增，损坏元数据不能驱动正文借用。
        Ok(Self {
            next_ordinal,
            next,
            byte: start.byte,
            end,
            utf16: start.utf16,
            pending: [0; 4],
            used: 0,
            needed: 0,
            word: false,
            space: true,
            newline: false,
            single_space: end.byte - start.byte == 1,
        })
    }

    fn push(&mut self, byte: u8, step: &mut MarkdownWordDeleteStep) -> Result<()> {
        self.byte += 1;
        step.bytes += 1;
        if self.used == 0 {
            self.needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
        } // 每簇起点来自真实GCB；非法前导字节保留失败，不补替换字符或改动来源。
        self.pending[self.used] = byte;
        self.used += 1;
        if self.used != self.needed {
            return Ok(()); // 跨piece/量子的半标量继续留在同一簇，不提前分类或补终止。
        }
        let scalar = std::str::from_utf8(&self.pending[..self.used])
            .ok()
            .and_then(|value| value.chars().next())
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        // 保留原编辑器三类group规则；分类完整GCB，emoji或mark不能按UTF16单元拆开。
        self.word |= scalar.is_alphabetic() || scalar.is_numeric() || scalar == '_';
        self.space &= ecmascript_space(scalar);
        self.newline |= matches!(scalar, '\r' | '\n');
        self.single_space &= scalar == ' ';
        self.utf16 += scalar.len_utf16();
        self.used = 0;
        step.scalars += 1;
        Ok(())
    }

    pub(in crate::markdown_line_composer) fn read(
        &mut self,
        source: &MarkdownLayoutTextSource,
        budget: MarkdownLineComposeBudget,
        step: &mut MarkdownWordDeleteStep,
    ) -> Result<bool> {
        let end = self
            .byte
            .saturating_add(budget.bytes - step.bytes)
            .min(self.end.byte);
        let mut stopped = false;
        let mut failure = None;
        let result = source.visit_byte_range(self.byte..end, |piece| {
            for byte in piece {
                if step.scalars == budget.scalars {
                    stopped = true;
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                } // 先看配额再解引用，未消费的下个scalar不隐藏到本步读取量之外。
                if let Err(error) = self.push(*byte, step) {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
                if stopped || failure.is_some() => {}
            Err(error) => return Err(error.into()),
        } // 只有本回调的暂停sentinel可消解；实际Source首错仍交原Composer finish优先处理。
        if let Some(error) = failure {
            return Err(error);
        }
        if self.byte == self.end.byte {
            if self.used != 0 || self.utf16 != self.end.utf16 {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            } // 原GCB两个坐标必须与实际逐标量内容吻合，短读不能签出完整类别。
            return Ok(true);
        }
        Ok(false)
    }

    pub(in crate::markdown_line_composer) fn category(&self) -> Category {
        if self.space {
            Category::Space
        } else if self.word {
            Category::Word
        } else {
            Category::Other
        }
    }
}

/// ECMAScript空白包含BOM而不含NEL，不能以Rust较宽的is_whitespace悄悄改变原group。
fn ecmascript_space(scalar: char) -> bool {
    matches!(scalar, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

pub(super) struct WordDelete {
    mode: CursorMode,
    direction: MarkdownWordDeleteDirection,
    head: usize,
    at: Option<(usize, MarkdownGraphemeBoundary)>,
    seek: Option<Seek>,
    cluster: Option<Cluster>,
    started: bool,
    category: Option<Category>,
    dependency: bool,
    ready: Option<Range<usize>>,
    ascii_ready: Option<bool>,
    ascii_run: Option<crate::markdown_document_buffer::MarkdownAsciiWordRun>,
    ascii_endpoint: Option<usize>,
    ascii_skip: bool,
    seed_newline: bool,
    visible_word: bool,
    visible_proved: bool,
    visible_proof_next: Option<usize>,
    visual_rejected: bool,
}

impl WordDelete {
    fn new(direction: MarkdownWordDeleteDirection, head: usize) -> Self {
        Self {
            mode: CursorMode::Delete,
            direction,
            head,
            at: None,
            seek: None,
            cluster: None,
            started: false,
            category: None,
            dependency: false,
            ready: None,
            ascii_ready: None,
            ascii_run: None,
            ascii_endpoint: None,
            ascii_skip: false,
            seed_newline: false,
            visible_word: false,
            visible_proved: false,
            visible_proof_next: None,
            visual_rejected: false,
        }
    }

    /// 新逻辑group只改变本游标的停止规则；Source、GCB依赖和量子仍由原Payload唯一持有。
    fn group(direction: MarkdownWordDeleteDirection, head: usize, mode: CursorMode) -> Self {
        let mut cursor = Self::new(direction, head);
        cursor.mode = mode;
        if let CursorMode::Expand(category) = mode {
            cursor.category = Some(category); // 两侧扩选必须沿首个真实GCB的类别，不能各自挑一个词。
        }
        cursor
    }

    fn finish(&mut self, step: &mut MarkdownWordDeleteStep) -> Result<()> {
        let position = self
            .at
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .1
            .utf16;
        let range = self.head.min(position)..self.head.max(position);
        self.ready = Some(range.clone());
        step.answer = MarkdownWordDeleteAnswer::Ready(range);
        Ok(())
    }

    /// 只有原全源Dense GCB可借原树摘要跳alnum；混合边叶仍逐标量收费，停止字符交回原规则。
    fn ascii_alnum(
        &mut self,
        source: &MarkdownLayoutTextSource,
        breaks: &mut crate::MarkdownLayoutBreakJob,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineComposeBudget,
        step: &mut MarkdownWordDeleteStep,
    ) -> Result<bool> {
        if self.visible_word
            || matches!(self.mode, CursorMode::Seed | CursorMode::Grapheme)
            || self.ascii_skip
            || self
                .category
                .is_some_and(|category| category != Category::Word)
        {
            return Ok(false); // Space/Other已锁类别时不能跨过下一个词；underscore仍由原GCB分类。
        }
        if self.ascii_ready.is_none() {
            if step.metadata_units == budget.metadata_units {
                return Ok(true);
            }
            self.ascii_ready = Some(breaks.ascii_prefix_ready(authority)?);
            step.metadata_units += 1;
        }
        if self.ascii_ready != Some(true) {
            return Ok(false); // Unicode/CRLF或未认证Dense均保持原GCB路径，不能只凭局部ASCII叶跳簇。
        }
        let current = self
            .at
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .1;
        if self.ascii_endpoint.is_none() {
            if self.ascii_run.is_none() {
                if step.metadata_units == budget.metadata_units {
                    return Ok(true);
                }
                self.ascii_run = Some(source.begin_ascii_word_run(
                    current.utf16,
                    self.direction == MarkdownWordDeleteDirection::Forward,
                )?);
                step.metadata_units += 1; // Snapshot建立游标真实读取一次原root字符域和范围事实。
            }
            let run = self
                .ascii_run
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let progress = source.advance_ascii_word_run(
                run,
                budget.bytes - step.bytes,
                budget.scalars - step.scalars,
                budget.metadata_units - step.metadata_units,
            )?;
            step.bytes += progress.bytes;
            step.scalars += progress.scalars;
            step.metadata_units += progress.metadata;
            if !progress.complete {
                return Ok(true); // 保存同一原节点/边叶游标，budget1也不必从root重新走完旧路径。
            }
            self.ascii_run = None; // 实际节点Arc栈在本次源权限内归还，不拖到后继Source。
            if progress.position == current.utf16 {
                self.ascii_skip = true;
                return Ok(false); // 当前真实字符不是alnum，只检查过它，原GCB仍拥有消费与分类权。
            }
            self.ascii_endpoint = Some(progress.position);
        }
        if step.records == budget.records || step.metadata_units == budget.metadata_units {
            return Ok(true);
        }
        let position = self
            .ascii_endpoint
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let (_, endpoint) = breaks.certified_grapheme_endpoint(authority, position)?;
        step.records += 1;
        step.metadata_units += 1;
        let endpoint = endpoint
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .boundary(authority)?;
        if endpoint.utf16 != position || endpoint.byte != position {
            return Err(MarkdownLineComposeError::InconsistentCoverage);
        }
        // ordinal只在原整源Dense整数表示与这张实际端点票同时成立时等于UTF16位置。
        self.at = Some((position, endpoint));
        self.started = true;
        self.category = Some(Category::Word);
        self.ascii_endpoint = None;
        self.ascii_skip = true; // 下一停止字符必须由原Cluster解释，不在同一位置重启查询。
        Ok(false)
    }

    fn advance(
        &mut self,
        source: &MarkdownLayoutTextSource,
        coordinator: &mut MarkdownLineItemCoordinator,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordDeleteStep> {
        if self.visible_word
            && !self.visible_proved
            && let Some(cluster) = &self.cluster
        {
            let position = self.visible_proof_next.unwrap_or(cluster.utf16);
            let (actual, proof) =
                coordinator.prepare_word_cluster(position..cluster.end.utf16, budget)?;
            if let Some((permitted, through)) = proof {
                if through <= position || through > cluster.end.utf16 {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                } // 完整 B 段只签真实覆盖终点；不能停在当前点或越过原 GCB 后替它猜范围。
                self.visual_rejected = !permitted;
                self.visible_proved = permitted && through == cluster.end.utf16;
                self.visible_proof_next = (permitted && !self.visible_proved).then_some(through);
            }
            return Ok(MarkdownWordDeleteStep {
                bytes: actual.bytes,
                scalars: actual.scalars,
                records: actual.records,
                metadata_units: actual.metadata_units,
                answer: MarkdownWordDeleteAnswer::Yielded,
            });
        } // 端点已由原 GCB 证明；借 Breaks 前先完成同一 Item 的 B 段准备，不并借两个可变角色。
        let folds = coordinator.fold_plan();
        let breaks = coordinator.word_breaks()?;
        if self.dependency {
            // 未知边界只推进原GCB一次；本次不再消费类别，避免依赖和调用方各花一份配额。
            let progress = breaks.step_dependency(
                authority,
                true,
                MarkdownLineBreakBudget {
                    bytes: budget.bytes,
                    scalars: budget.scalars,
                    records: budget.records.min(budget.metadata_units),
                },
            )?;
            self.dependency = false;
            return Ok(MarkdownWordDeleteStep {
                bytes: progress.bytes,
                scalars: progress.scalars,
                records: progress.records,
                metadata_units: progress.records,
                answer: MarkdownWordDeleteAnswer::Yielded,
            });
        }
        let prefix = breaks.grapheme_prefix_facts(authority)?;
        let mut step = MarkdownWordDeleteStep::yielded();
        if let Some(range) = &self.ready {
            step.answer = MarkdownWordDeleteAnswer::Ready(range.clone());
            return Ok(step); // 终态也已经过原GCB健康门，取消或真实IO失败不能借旧结果复活。
        }
        if self.at.is_none() && self.seek.is_none() {
            if prefix
                .last_committed_boundary
                .is_none_or(|last| last.utf16 < self.head)
            {
                if prefix.complete {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                self.dependency = true;
                return Ok(step); // 前缀还未到原head；完整源长度不是末ordinal证明。
            }
            self.seek = Some(Seek {
                low: 0,
                high: prefix.known_boundary_count,
                candidate: None,
            });
        }
        loop {
            if let Some(seek) = self.seek.as_mut() {
                if seek.low == seek.high {
                    let (ordinal, boundary) = seek
                        .candidate
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                    if ordinal != seek.low {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    } // 已读取候选必须就是二分落点；索引矛盾不能当作普通用户坐标错误。
                    if boundary.utf16 != self.head {
                        if !matches!(self.mode, CursorMode::Grapheme) {
                            return Err(MarkdownLineComposeError::InvalidRequest);
                        } // 词组动作仍要求真实GCB；普通删除保留原Scene对任意合法scalar caret的语义。
                        self.at = Some((ordinal, boundary));
                        self.seek = None;
                        if self.direction == MarkdownWordDeleteDirection::Forward {
                            // 原Next在簇内部取lower_bound本身；不可多跳一簇，也不擅自扩大到簇左端。
                            self.finish(&mut step)?;
                            return Ok(step);
                        }
                        continue; // 原Previous取lower_bound的前驱；后续仍只收费一条真实边界记录。
                    }
                    self.at = Some((ordinal, boundary));
                    self.seek = None;
                    continue;
                }
                if step.records == budget.records || step.metadata_units == budget.metadata_units {
                    return Ok(step);
                } // 二分的每条原记录也计入同一量子，不把logN查询藏成一次操作。
                let ordinal = seek.low + (seek.high - seek.low) / 2;
                let mut record = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                if !breaks.read_grapheme_prefix(authority, ordinal..ordinal + 1, &mut record)? {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                } // seek只访问开始时已经提交的不可变前缀，不能把其消失解释为等待更多正文。
                step.records += 1;
                step.metadata_units += 1;
                let boundary = record[0];
                if boundary.utf16 < self.head {
                    seek.low = ordinal + 1;
                } else {
                    seek.high = ordinal;
                    seek.candidate = Some((ordinal, boundary));
                }
                continue;
            }
            if self.cluster.is_none() {
                if self.ascii_alnum(source, breaks, authority, budget, &mut step)? {
                    return Ok(step);
                }
                let (ordinal, current) = self
                    .at
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let next = match self.direction {
                    MarkdownWordDeleteDirection::Backward => ordinal.checked_sub(1),
                    MarkdownWordDeleteDirection::Forward => ordinal.checked_add(1),
                };
                let Some(next) = next else {
                    self.finish(&mut step)?;
                    return Ok(step); // 原首ordinal没有前驱，是实际BOF而不是视口左界。
                };
                if next >= prefix.known_boundary_count {
                    if prefix.complete {
                        self.finish(&mut step)?;
                    } else {
                        self.dependency = true;
                    }
                    return Ok(step); // 只有完整GCB表才授予真EOF；未知后继继续同一个原job。
                }
                if step.records == budget.records || step.metadata_units == budget.metadata_units {
                    return Ok(step);
                }
                let mut record = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                if !breaks.read_grapheme_prefix(authority, next..next + 1, &mut record)? {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                step.records += 1;
                step.metadata_units += 1;
                if (self.direction == MarkdownWordDeleteDirection::Backward)
                    != (record[0].byte < current.byte)
                {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                } // 原ordinal前后必须对应真实源同方向，损坏记录不能把Backward变成向后删正文。
                if let Some(plan) = &folds
                    && let Some(span) = plan.spans().iter().find(|span| {
                        span.start.utf16 == current.utf16.min(record[0].utf16)
                            && span.end.utf16 == current.utf16.max(record[0].utf16)
                    })
                {
                    step.answer = MarkdownWordDeleteAnswer::NeedsFoldExpansion(
                        span.start.utf16..span.end.utf16,
                    );
                    return Ok(step);
                } // 明确交回原展开意图；不按“一个字符”删除整段，也不借 Cluster::read 物化隐区。
                if matches!(self.mode, CursorMode::Grapheme) {
                    // 相邻ordinal已经由同一GCB目录认证；emoji、CRLF和任意长combining簇
                    // 均直接使用它的两个真实端点，禁止重新按scalar或视口截取删除范围。
                    self.at = Some((next, record[0]));
                    self.finish(&mut step)?;
                    return Ok(step);
                }
                self.cluster = Some(Cluster::new(next, current, record[0])?);
                if self.visible_word {
                    self.visible_proved = false;
                    self.visible_proof_next = None;
                    return Ok(step);
                } // opaque 已先处理；下一回合消费真实 B 段证书后才能读取/分类这个原簇。
            }
            if step.bytes == budget.bytes || step.scalars == budget.scalars {
                return Ok(step);
            } // 已读的真实簇端点留在同一个续体；不重读正文，也不把本步末尾当group终点。
            let cluster = self
                .cluster
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            if !cluster.read(source, budget, &mut step)? {
                return Ok(step);
            }
            let cluster = self
                .cluster
                .take()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            self.visible_proved = false;
            self.visible_proof_next = None;
            match self.mode {
                CursorMode::Seed => {
                    // 首簇只用于固定真实类别和硬换行事实；即使巨大单GCB也必须读完原簇才完成。
                    self.category = Some(cluster.category());
                    self.seed_newline = cluster.newline;
                    self.at = Some((cluster.next_ordinal, cluster.next));
                    self.finish(&mut step)?;
                    return Ok(step);
                }
                CursorMode::Move => {
                    let category = cluster.category();
                    if self
                        .category
                        .is_some_and(|previous| previous != Category::Space && previous != category)
                    {
                        self.finish(&mut step)?;
                        return Ok(step); // 任意leading空白之后只消费一个真实Word/Other group。
                    }
                    self.category = Some(category);
                    self.started = true;
                    self.at = Some((cluster.next_ordinal, cluster.next));
                    self.ascii_skip = false;
                    continue;
                }
                CursorMode::Expand(category) => {
                    if cluster.newline || cluster.category() != category {
                        self.finish(&mut step)?;
                        return Ok(step); // 选词属于同一硬行；Space组也不能吞掉CR/LF/CRLF。
                    }
                    self.started = true;
                    self.at = Some((cluster.next_ordinal, cluster.next));
                    self.ascii_skip = false;
                    continue;
                }
                CursorMode::Grapheme => unreachable!("单GCB动作在取得相邻原边界时已经结束"),
                CursorMode::Delete => {} // 原删除的单ASCII空格与独立换行规则继续由下方原路径处理。
            }
            if cluster.newline {
                if !self.started {
                    self.at = Some((cluster.next_ordinal, cluster.next));
                } // 只在动作起点就是行边界时删原完整CR/LF/CRLF；先前group不跨真实换行。
                self.finish(&mut step)?;
                return Ok(step);
            }
            let category = cluster.category();
            if self.category.is_some_and(|previous| previous != category) {
                self.finish(&mut step)?;
                return Ok(step); // 下一真实簇只用于确定终点，保持它及其后正文原样。
            }
            if self.started || !cluster.single_space {
                self.category = Some(category);
            } // 仅首个ASCII单空格不锁类别；第二空格/TAB按Space锁定，保留原删除手感。
            self.started = true;
            self.at = Some((cluster.next_ordinal, cluster.next));
            self.ascii_skip = false; // 已经消费了停止字符，可在下一真实GCB尝试后续alnum段。
        }
    }
}

impl MarkdownLineComposer {
    /// 在本worker原冻结源上开始一次词组删除；仅固定方向/位置，不扫描、复制正文或分配资源。
    ///
    /// # Errors
    /// 原Source/取消先于范围错误；未消费的旧词组动作不得被新请求静默覆盖。
    pub fn begin_word_delete(
        &mut self,
        direction: MarkdownWordDeleteDirection,
        head_utf16: usize,
    ) -> Result<()> {
        self.begin_delete(direction, head_utf16, CursorMode::Delete)
    }

    /// 开始普通前删或退格；原GCB端点取相邻完整簇，簇内部scalar caret保留原严格Next/Previous语义。
    ///
    /// # Errors
    /// 原Source、取消、未完成动作和范围错误与词组删除完全相同，不建立第二份源或索引。
    pub fn begin_grapheme_delete(
        &mut self,
        direction: MarkdownWordDeleteDirection,
        head_utf16: usize,
    ) -> Result<()> {
        self.begin_delete(direction, head_utf16, CursorMode::Grapheme)
    }

    /// 两种删除只在实际停止规则上不同；认证和唯一原动作槽沿同一个入口装配。
    fn begin_delete(
        &mut self,
        direction: MarkdownWordDeleteDirection,
        head_utf16: usize,
        mode: CursorMode,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if payload.word_delete.is_some() || head_utf16 > payload.source.utf16_len() {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        if matches!(mode, CursorMode::Grapheme) {
            // 原NSTextInputClient允许GCB内部的scalar caret，但代理对内部不能进入不可逆删除。
            // 这里复用原源的UTF16→byte合同，不猜测Unicode边界或创建局部正文副本。
            payload.source.byte_offset_for_utf16(head_utf16)?;
        }
        let mut cursor = WordDelete::new(direction, head_utf16);
        cursor.mode = mode;
        payload.word_delete = Some(WordJob::Delete(cursor));
        Ok(())
    }

    /// 只推进本次词组动作的原GCB或正文分类；每步共享4KiB/256标量/256记录上限。
    ///
    /// # Errors
    /// 实际源、索引、取消和非GCB位置都保留原typed首因；失败沿原Composer释放边界关闭。
    pub fn step_word_delete(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownWordDeleteStep> {
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
            let word = payload
                .word_delete
                .as_mut()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            let WordJob::Delete(word) = word else {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }; // 另一个真实group动作不能被错入口消费或冒充删除。
            word.advance(
                &payload.source,
                &mut payload.coordinator,
                &self.authority,
                budget,
            )
        })();
        self.finish(result)
    }

    /// 原输入撤权时只归还该临时标量续体；字体/排版和同一Source仍归原worker生命周期。
    pub fn cancel_word_delete(&mut self) {
        if let Some(payload) = self.payload.as_mut()
            && matches!(payload.word_delete, Some(WordJob::Delete(_)))
        {
            payload.word_delete = None;
        }
    }
}

#[cfg(test)]
mod fold_tests;
#[cfg(test)]
mod tests;
