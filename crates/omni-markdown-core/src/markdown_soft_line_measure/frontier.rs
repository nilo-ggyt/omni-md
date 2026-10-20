//! 只有实际producer认证的词前缀可用非负后缀界；所有断点仍查原GCB与UAX14索引。
use super::*;
use crate::{MarkdownGlyphCluster, MarkdownGraphemeAnswer, MarkdownGraphemeQuery};

pub(super) struct WordClusterProbe {
    cluster: MarkdownGlyphCluster,
    grapheme_checked: bool,
    cursor: Option<MarkdownLayoutBreakCursor>,
    shaping_seam: bool,
}

/// 整行只保一个安全GCB候选及最新可容纳GCB末端；跨真实前缀不保旧Job或每item证明数组。
pub(super) struct WordSafeState {
    word_end: usize,
    font: MarkdownLayoutFontAuthority,
    start_safe: bool,
    first_end: Option<usize>,
    latest_fitting_end: Option<usize>,
    fitting: Option<WordSafeFit>,
    incomplete_grapheme: bool,
}

struct WordSafeFit {
    end: usize,
    width: i64,
    flags: u32,
}

fn emergency(start: usize, fit: WordSafeFit, overflowing: bool) -> MarkdownSoftLineMeasureChoice {
    MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak {
        source_utf16: start..fit.end,
        x_advance_26_6: fit.width,
        boundary_flags: fit.flags,
        overflowing_grapheme: overflowing,
    }
}

impl MarkdownSoftLineMeasureJob {
    /// true表示本量子已耗尽、待真实依赖或已选择；false继续原自然机会状态机。
    pub(super) fn advance_word_frontier(
        &mut self,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
        step: &mut MarkdownSoftLineMeasureStep,
    ) -> Result<bool> {
        let Some(word_end) = provider.frontier_word_end(&self.source, &self.font)? else {
            return Ok(false);
        };
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        if payload
            .candidate
            .is_some_and(|boundary| boundary.utf16 == payload.scan)
        {
            // 自然机会可能恰好落在前一个真实字形分片末端。后继分片只补齐它的
            // cluster flags，必须先交回原测量提交这个候选；先扫描长词会丢掉已到达
            // 的断点与累计宽度，继而错误请求跨分片重排。
            return Ok(false);
        }
        if word_end < self.item.end || word_end > payload.source.utf16_len() {
            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
        } // 末片允许等于真实词尾；只有当前实际provider范围有glyph，词末坐标不授额外读取。
        if payload
            .word_safe
            .as_ref()
            .is_none_or(|state| state.word_end != word_end || !state.font.same_payload(&self.font))
        {
            let incomplete_grapheme = payload
                .word_safe
                .as_ref()
                .is_some_and(|state| state.incomplete_grapheme);
            payload.word_safe = Some(WordSafeState {
                word_end,
                font: self.font.clone(),
                start_safe: false,
                first_end: None,
                latest_fitting_end: None,
                fitting: None,
                incomplete_grapheme,
            }); // 源已同票固定；换字体或闭合词项会撤旧紧急候选，不能跨别的字形依赖重签。
        }
        loop {
            self.guard_provider(provider)?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            if payload.scan == self.item.end {
                payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedNextItem {
                    source_utf16: self.start..self.item.end,
                    x_advance_26_6: payload.advance,
                    last_fitting: payload.fitting.as_ref().map(|fit| fit.measured.clone()),
                });
                step.answer = MarkdownSoftLineMeasureAnswer::Selected(
                    payload
                        .selected
                        .clone()
                        .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                );
                return Ok(true); // 真正消费完当前前缀才领下一项；没有把词尾坐标签成glyph尾部。
            }
            if payload.word_probe.is_none() {
                if step.clusters == budget.clusters.min(MARKDOWN_SOFT_LINE_CLUSTERS) {
                    return Ok(true);
                }
                step.clusters += 1;
                let cluster = match provider.cluster(&self.source, &self.font, payload.scan)? {
                    MarkdownGlyphClusterAnswer::Pending => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(true);
                    }
                    MarkdownGlyphClusterAnswer::Known(Some(value)) => value,
                    MarkdownGlyphClusterAnswer::Known(None) => {
                        return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                    }
                };
                if cluster.source_utf16.start != payload.scan
                    || cluster.source_utf16.end <= payload.scan
                    || cluster.source_utf16.end > self.item.end
                {
                    return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                }
                if cluster.missing_glyphs != 0 {
                    payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedsFontFallback {
                        cluster_utf16: cluster.source_utf16,
                        missing_glyphs: cluster.missing_glyphs,
                    });
                    step.answer = MarkdownSoftLineMeasureAnswer::Selected(
                        payload
                            .selected
                            .clone()
                            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                    );
                    return Ok(true); // 全域静态闭包与实际glyph必须一致，不能用.notdef宽度签紧急行。
                }
                let safe = payload
                    .word_safe
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
                if payload.scan == self.start {
                    safe.start_safe = cluster.flags & UNSAFE_TO_BREAK == 0;
                } else if safe.start_safe
                    && !safe.incomplete_grapheme
                    && cluster.flags & UNSAFE_TO_BREAK == 0
                {
                    // 当前真实cluster是前一个GCB的后继。其原flags才证明该切口可复用，
                    // 不能拿前缀flags并集、item末端或缺少后继记录来推测安全。
                    let fit = WordSafeFit {
                        end: payload.scan,
                        width: payload.advance,
                        flags: cluster.flags,
                    };
                    if fit.width <= self.width {
                        safe.fitting = Some(fit);
                    } else if safe.fitting.is_none() && safe.first_end == Some(fit.end) {
                        payload.selected = Some(emergency(self.start, fit, true));
                        step.answer = MarkdownSoftLineMeasureAnswer::Selected(
                            payload
                                .selected
                                .clone()
                                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                        );
                        return Ok(true); // 第一个完整GCB本来超宽也完整保留；不是拆字素或假适配。
                    }
                }
                payload.word_probe = Some(WordClusterProbe {
                    cluster,
                    grapheme_checked: false,
                    cursor: None,
                    shaping_seam: false,
                });
            }
            let probe = payload
                .word_probe
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            let end = probe.cluster.source_utf16.end;
            if !probe.grapheme_checked {
                if step.break_queries == budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES)
                {
                    return Ok(true);
                }
                step.break_queries += 1;
                match breaks.grapheme_query(&self.source, end, MarkdownGraphemeQuery::IsBoundary)? {
                    MarkdownGraphemeAnswer::Pending => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(true);
                    }
                    MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == end => {}
                    MarkdownGraphemeAnswer::Known(None)
                        if end == self.item.end
                            && provider.bidi_shaping_end(&self.source, &self.font)?
                                == Some(end) =>
                    {
                        probe.shaping_seam = true;
                    }
                    MarkdownGraphemeAnswer::Known(_) => {
                        return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                    }
                }
                probe.grapheme_checked = true; // 原索引不可变；quota1后继不重复计量或把未知当边界。
            }
            if probe.shaping_seam {
                let probe = payload
                    .word_probe
                    .take()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
                payload.advance = payload
                    .advance
                    .checked_add(probe.cluster.x_advance_26_6)
                    .ok_or(MarkdownSoftLineMeasureError::Overflow)?;
                payload.scan = end;
                payload
                    .word_safe
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
                    .incomplete_grapheme = true;
                continue; // 只累计这个真实分项，不能在内部 seam 更新 fitting、首字素或 emergency。
            }
            if step.break_queries == budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES) {
                return Ok(true);
            }
            if probe.cursor.is_none() {
                probe.cursor = Some(breaks.cursor(
                    MarkdownLayoutBreakUnit::Utf16,
                    end,
                    MarkdownLayoutBreakQuery::At,
                )?);
            }
            step.break_queries += 1;
            let boundary = match breaks.query(
                probe
                    .cursor
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                1,
            )? {
                MarkdownLayoutBreakAnswer::PendingSource => {
                    step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                    return Ok(true);
                }
                MarkdownLayoutBreakAnswer::Yielded => return Ok(true),
                MarkdownLayoutBreakAnswer::Known(value) => value,
            };
            let probe = payload
                .word_probe
                .take()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            payload.advance = payload
                .advance
                .checked_add(probe.cluster.x_advance_26_6)
                .ok_or(MarkdownSoftLineMeasureError::Overflow)?;
            payload.scan = end;
            let safe = payload
                .word_safe
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            safe.incomplete_grapheme = false; // 此前 IsBoundary 与 At 已核完真正外 GCB，才能恢复断点候选。
            if safe.first_end.is_none() {
                safe.first_end = Some(end);
            } // 只有刚完成原GCB/At检查才记首字素末端；quota暂停时不提前签候选。
            if payload.advance <= self.width {
                // 即使这个最新切口依赖后继kern，也要保留其真实可容纳事实。安全候选可能更早，
                // 不能为了复用原glyph而改变断点；只有同一末端可直接复用，否则仍交真实prefix重形。
                safe.latest_fitting_end = Some(end);
            }
            if let Some(boundary) = boundary {
                if boundary.utf16 != end || payload.candidate.is_some_and(|old| old.utf16 != end) {
                    return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                }
                payload.candidate = Some(boundary);
                payload.cursor = None;
                return Ok(false); // 真实UAX机会交回原算法；此处不捏造Allowed/Mandatory或Dictionary。
            }
            if payload.advance > self.width {
                payload.selected = Some(match payload.fitting.take() {
                    Some(fitting) => selected(fitting),
                    None => match safe
                        .fitting
                        .take()
                        .filter(|fit| Some(fit.end) == safe.latest_fitting_end)
                    {
                        Some(fitting) => emergency(self.start, fitting, false),
                        None if safe.first_end == Some(end) => {
                            continue; // 尚缺首字素后继flags；下一量子/真实前缀核完后再签overflow。
                        }
                        None => MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix {
                            source_utf16: self.start..self.item.end,
                            word_end_utf16: word_end,
                            measured_prefix_26_6: payload.advance,
                        },
                    },
                });
                step.answer = MarkdownSoftLineMeasureAnswer::Selected(
                    payload
                        .selected
                        .clone()
                        .ok_or(MarkdownSoftLineMeasureError::Poisoned)?,
                );
                return Ok(true); // 此前每个GCB处都是真NoBreak，且未读同Font词后缀已证明非负。
            }
        }
    }
}
