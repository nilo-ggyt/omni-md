//! 视觉词组消费原 L1/L2 相邻字素及原分类器；仅一个簇和一个弱视觉查询可跨量子存活。

use super::*;
use crate::markdown_line_composer::engine::word::{Category, Cluster};
use crate::{MarkdownLineComposeBudget, MarkdownWordDeleteAnswer, MarkdownWordDeleteStep};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownComposedVisualWordAnswer {
    Pending,
    PrefixNotCovered,
    NeedsFoldExpansion(Range<usize>),
    Found {
        utf16: usize,
        side: MarkdownComposedCaretSide,
        line: Range<usize>,
        ordinal: usize,
    },
}

pub struct MarkdownComposedVisualWordStep {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
    pub answer: MarkdownComposedVisualWordAnswer,
}

/// 已分类进度只归原 writer/Source；弱票不保活 Reader、字体、Source 或另一段 UBA。
pub struct MarkdownComposedVisualWordQuery {
    lineage: Weak<()>,
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    position: usize,
    affinity: MarkdownComposedLineAffinity,
    side: Option<MarkdownComposedCaretSide>,
    right: bool,
    forward: Option<bool>,
    category: Option<Category>,
    visual: Option<MarkdownComposedVisualQuery>,
    cluster: Option<Cluster>,
    candidate: Option<MarkdownComposedVisualAnswer>,
    last: Option<MarkdownComposedVisualAnswer>,
    last_fonts: [Option<MarkdownLineFontRegistration>; 5],
    candidate_fonts: [Option<MarkdownLineFontRegistration>; 5],
    answer: Option<MarkdownComposedVisualWordAnswer>,
    failed: bool,
}

impl MarkdownComposedLines {
    /// 捕获真实屏幕方向；逻辑 Forward/Backward 不使用此入口，也不从段 base 猜字符次序。
    pub fn visual_word_query(
        &mut self,
        utf16: usize,
        affinity: MarkdownComposedLineAffinity,
        side: Option<MarkdownComposedCaretSide>,
        right: bool,
    ) -> Result<MarkdownComposedVisualWordQuery> {
        let visual = self.visual_query(utf16, affinity, side, right)?;
        Ok(MarkdownComposedVisualWordQuery {
            lineage: self.lineage.clone(),
            output: Arc::downgrade(&self.identity),
            source: self.authority.clone(),
            position: utf16,
            affinity,
            side,
            right,
            forward: None,
            category: None,
            visual: Some(visual),
            cluster: None,
            candidate: None,
            last: None,
            last_fonts: std::array::from_fn(|_| None),
            candidate_fonts: std::array::from_fn(|_| None),
            answer: None,
            failed: false,
        })
    }

    /// 每回合只做原目录查询或一个原簇的正文分类；追加 prefix 只重建未完成的弱目录查询。
    pub fn step_visual_word(
        &mut self,
        query: &mut MarkdownComposedVisualWordQuery,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedVisualWordStep> {
        let result = self.advance_visual_word(query, budget);
        if result.is_err() {
            query.failed = true;
        }
        if matches!(result, Err(Error::InvalidRequest)) {
            self.require(&query.source)?;
            return result;
        }
        self.finish(result)
    }

    /// 原正文、writer、字体与取消先于任何缓存；同 Source 的另一宽度/样式管线不能接续旧路径。
    fn advance_visual_word(
        &mut self,
        query: &mut MarkdownComposedVisualWordQuery,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedVisualWordStep> {
        self.require(&query.source)?;
        if query.failed {
            return Err(Error::Poisoned);
        }
        if !Weak::ptr_eq(&query.lineage, &self.lineage) {
            return Err(Error::SourceMismatch);
        }
        query.require_fonts(&self.payload.as_ref().ok_or(Error::Poisoned)?.fonts)?;
        if budget.bytes == 0
            || budget.scalars == 0
            || budget.records == 0
            || budget.metadata_units == 0
        {
            return Err(Error::InvalidRequest);
        }
        let budget = MarkdownLineComposeBudget {
            bytes: budget.bytes.min(4096),
            scalars: budget.scalars.min(256),
            records: budget.records.min(256),
            metadata_units: budget.metadata_units.min(256),
        };
        if !Weak::ptr_eq(&query.output, &Arc::downgrade(&self.identity)) {
            query.visual = None;
            query.output = Arc::downgrade(&self.identity);
        } // 已接受的 caret/类别仍由同 writer 的完整原行证明；旧 Part 票没有转给新 prefix。
        let mut step = MarkdownComposedVisualWordStep {
            bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            answer: MarkdownComposedVisualWordAnswer::Pending,
        };
        if let Some(answer) = &query.answer {
            step.answer = answer.clone();
        } else if let Some(cluster) = &mut query.cluster {
            let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
            let mut actual = MarkdownWordDeleteStep {
                bytes: 0,
                scalars: 0,
                records: 0,
                metadata_units: 0,
                answer: MarkdownWordDeleteAnswer::Yielded,
            };
            let complete = cluster.read(source, budget, &mut actual)?;
            step.bytes = actual.bytes;
            step.scalars = actual.scalars;
            if complete {
                let category = cluster.category();
                query.cluster = None;
                query.accept(category)?;
            } // 同一 UTF8 残余可跨一字节预算；未完成簇不会提前提供类别或提交候选 caret。
        } else {
            if query.visual.is_none() {
                query.visual = Some(self.visual_query(
                    query.position,
                    query.affinity,
                    query.side,
                    query.right,
                )?);
                query
                    .visual
                    .as_mut()
                    .expect("刚创建原视觉查询")
                    .flow_forward = query.forward;
            }
            let visual = query.visual.as_mut().expect("原视觉词组只保一个相邻查询");
            let actual = self.step_visual(visual, budget.records.min(budget.metadata_units))?;
            query.forward = visual.flow_forward;
            step.records = actual.records;
            step.metadata_units = actual.records;
            if let Some(hidden) = &visual.crossed_fold {
                query.candidate_fonts = visual.word_registrations();
                query.answer = Some(MarkdownComposedVisualWordAnswer::NeedsFoldExpansion(
                    hidden.clone(),
                ));
            } else {
                match actual.answer {
                    MarkdownComposedVisualAnswer::Pending => {}
                    MarkdownComposedVisualAnswer::PrefixNotCovered => {
                        step.answer = MarkdownComposedVisualWordAnswer::PrefixNotCovered;
                    }
                    found @ MarkdownComposedVisualAnswer::Found { .. } => {
                        query.candidate_fonts = visual.word_registrations();
                        let hard_break = visual.crossed_break;
                        let pair = visual
                            .gcb
                            .as_ref()
                            .and_then(|gcb| gcb.origin.zip(gcb.neighbor));
                        query.candidate = Some(found);
                        query.visual = None;
                        if hard_break {
                            // 原 ParagraphBreak 控制只覆盖 LF/CR/CRLF/LS/PS，均属于原 ECMAScript 空白类别。
                            query.accept(Category::Space)?;
                        } else if let Some(((_, origin), (ordinal, neighbor))) = pair {
                            query.cluster = Some(Cluster::new(ordinal, origin, neighbor)?);
                        } else {
                            let terminal =
                                query.candidate.take().ok_or(Error::InconsistentCoverage)?;
                            query.last_fonts = std::mem::take(&mut query.candidate_fonts);
                            query.answer = Some(word_answer(terminal)?);
                        } // 没有实际跨字素/控制的 Found 只可能是真正 BOF/EOF，不从数值距离造一个正文范围。
                    }
                }
            }
        }
        self.require(&query.source)?;
        query.require_fonts(&self.payload.as_ref().ok_or(Error::Poisoned)?.fonts)?;
        if let Some(answer) = &query.answer {
            step.answer = answer.clone();
        }
        Ok(step)
    }
}

impl MarkdownComposedVisualWordQuery {
    /// 同一目录结果的弱票在源分类暂停及终态重查时仍有效；槽回收不能给旧视觉次序重新签名。
    fn require_fonts(&self, catalog: &MarkdownLineFontCatalog) -> Result<()> {
        for registration in self
            .last_fonts
            .iter()
            .chain(&self.candidate_fonts)
            .flatten()
        {
            catalog.require_registration(registration)?;
        }
        Ok(())
    }

    /// 保留原三类别与 leading-space 规则；停止时交上次已接受的真实视觉 caret，而非逻辑 min/max。
    fn accept(&mut self, category: Category) -> Result<()> {
        let candidate = self.candidate.take().ok_or(Error::InconsistentCoverage)?;
        if self
            .category
            .is_some_and(|old| old != Category::Space && old != category)
        {
            let previous = self.last.take().ok_or(Error::InconsistentCoverage)?;
            self.answer = Some(word_answer(previous)?);
            self.candidate_fonts = std::array::from_fn(|_| None);
            return Ok(());
        }
        let MarkdownComposedVisualAnswer::Found {
            utf16, side, line, ..
        } = &candidate
        else {
            return Err(Error::InconsistentCoverage);
        };
        self.position = *utf16;
        self.side = Some(*side);
        self.affinity = if !line.is_empty() && *utf16 == line.end && *utf16 != 0 {
            MarkdownComposedLineAffinity::Upstream
        } else {
            MarkdownComposedLineAffinity::Downstream
        };
        self.category = Some(category);
        self.last = Some(candidate);
        self.last_fonts = std::mem::take(&mut self.candidate_fonts);
        Ok(())
    }
}

/// 只移交原 Found 的位置、真实侧与行身份；Pending/缺前缀从不被包装为词组终点。
fn word_answer(found: MarkdownComposedVisualAnswer) -> Result<MarkdownComposedVisualWordAnswer> {
    match found {
        MarkdownComposedVisualAnswer::Found {
            utf16,
            side,
            line,
            ordinal,
        } => Ok(MarkdownComposedVisualWordAnswer::Found {
            utf16,
            side,
            line,
            ordinal,
        }),
        MarkdownComposedVisualAnswer::Pending | MarkdownComposedVisualAnswer::PrefixNotCovered => {
            Err(Error::InconsistentCoverage)
        }
    }
}
