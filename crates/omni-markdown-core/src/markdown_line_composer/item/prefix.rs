//! 原Script/GCB稳定前缀的按需衔接；开放范围不替代真实paragraph或完整依赖状态。
use super::*;
use crate::markdown_scripts::{MarkdownScriptPrefixAnswer, MarkdownScriptPrefixKind};

/// 仅完整Uniform无控制证明允许本轮开放Script；GCB和字体仍分别核验。
pub(super) fn unicode_prefix_allowed(
    payload: &Payload,
    authority: &MarkdownLayoutTextAuthority,
) -> Result<bool> {
    if !payload.frontier_enabled {
        return Ok(false);
    }
    let bidi = payload
        .bidi
        .as_ref()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    let end = payload
        .cursor
        .saturating_add(1)
        .min(payload.source.utf16_len());
    Ok(bidi
        .uniform_control_free_span(authority, payload.cursor..end)?
        .is_some())
}

/// 一个起点最多保一份固定坐标探针；各元数据查询独占一量子，quota1也不偷做后继查询。
pub(super) struct OpenPrefixProbe {
    run: crate::MarkdownScriptRun,
    // 新多段的开放准备至少累计到原闭项额度或真实B端点，不能以最早两个GCB反复试Font。
    through: usize,
    paragraph_end: Option<usize>,
    end: Option<crate::MarkdownGraphemeBoundary>,
    first: Option<crate::MarkdownGraphemeBoundary>,
    phase: u8,
}

/// 返回原Script事实与是否开放；未知只请求原子任务下一量子，不扫描临时子串。
pub(super) fn script_at(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
) -> Result<Option<crate::MarkdownScriptRun>> {
    payload.open_script = false;
    let allowed = unicode_prefix_allowed(payload, authority)?;
    if allowed && payload.closed_script_at == Some(payload.cursor) {
        let span = payload
            .bidi
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .uniform_control_free_span(
                authority,
                payload.cursor..payload.cursor.saturating_add(1),
            )?;
        if let Some(span) = span {
            // 原整形计划已要求真实闭项；同Script只证明原Latn上下文到同源B正文末端。
            // 非EOF仍真实消费B/CRLF自然闭合，绝不以当前256准备终点封闭SCX。
            payload.scripts.prepare_ascii_suffix(
                authority,
                payload.cursor,
                span.content_end(),
                span.paragraph_range().end,
            )?;
        }
    }
    if !allowed || payload.closed_script_at == Some(payload.cursor) {
        return match payload
            .scripts
            .run_at(authority, MarkdownScriptUnit::Utf16, payload.cursor)?
        {
            MarkdownScriptAnswer::Known(Some(run)) => {
                payload.closed_script_at = None;
                Ok(Some(run))
            }
            MarkdownScriptAnswer::Pending => {
                payload.script_source_demand = true;
                Ok(None)
            }
            _ => Err(MarkdownLineComposeError::InconsistentCoverage),
        };
    }
    if payload.open_probe.is_none() {
        let (run, open) = match payload.scripts.stable_prefix_at(
            authority,
            MarkdownScriptUnit::Utf16,
            payload.cursor,
        )? {
            MarkdownScriptPrefixAnswer::Known(Some(prefix)) => (
                prefix.facts()?,
                prefix.kind()? == MarkdownScriptPrefixKind::StableOpenPrefix,
            ),
            MarkdownScriptPrefixAnswer::Pending => {
                payload.script_source_demand = true;
                return Ok(None);
            }
            MarkdownScriptPrefixAnswer::Known(None) => {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
        };
        if !open {
            return Ok(Some(run));
        }
        let paragraph_end = if payload
            .bidi
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .uses_prepared_paragraphs()
        {
            payload
                .bidi
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                .uniform_control_free_span(
                    authority,
                    payload.cursor..payload.cursor.saturating_add(1),
                )?
                .map(|span| span.paragraph_range().end)
        } else {
            None
        };
        let through = paragraph_end
            .map(|end| end.min(payload.cursor.saturating_add(closed_item::MAX_CLOSED_UTF16)))
            .unwrap_or(payload.cursor);
        if run.end_utf16 < through {
            payload.script_source_demand = true;
            return Ok(None); // 原Script续体仍按调用方量子推进；准备目标不是SCX或Font边界。
        }
        payload.open_probe = Some(OpenPrefixProbe {
            run,
            through,
            paragraph_end,
            end: None,
            first: None,
            phase: 0,
        });
        return Ok(None); // 本量子只读原Script目录；下一量子再读取GCB覆盖事实。
    }
    let probe = payload
        .open_probe
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    match probe.phase {
        0 => {
            let facts = payload.breaks.grapheme_prefix_facts(authority)?;
            let Some(boundary) = facts
                .last_committed_boundary
                .filter(|value| value.utf16 > payload.cursor && value.utf16 >= probe.through)
            else {
                // 用真实Next留下GCB需求的工作留给下一量子，不能从GB2坐标推记录。
                probe.phase = 1;
                return Ok(None);
            };
            probe.end = Some(if boundary.utf16 < probe.run.end_utf16 {
                boundary
            } else {
                crate::MarkdownGraphemeBoundary {
                    byte: probe.run.end_byte,
                    utf16: probe.run.end_utf16,
                }
            });
            probe.phase = 1;
        }
        1 => {
            if probe.end.is_none() && probe.through > payload.cursor {
                match payload.breaks.grapheme_query(
                    authority,
                    probe.through,
                    MarkdownGraphemeQuery::Next,
                )? {
                    MarkdownGraphemeAnswer::Known(_) => probe.phase = 0,
                    MarkdownGraphemeAnswer::Pending => {}
                }
                return Ok(None); // 真实Next可跨长GCB/CRLF；未知末端只推进原GCB，不伪签准备目标。
            }
            match payload.breaks.grapheme_query(
                authority,
                payload.cursor,
                MarkdownGraphemeQuery::Next,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value)) => {
                    probe.first = Some(value);
                    probe.phase = if probe.end.is_some() { 2 } else { 0 };
                }
                MarkdownGraphemeAnswer::Pending => {}
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            }
        }
        2 => {
            let first = probe
                .first
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let second = match payload.breaks.grapheme_query(
                authority,
                first.utf16,
                MarkdownGraphemeQuery::Next,
            )? {
                MarkdownGraphemeAnswer::Known(Some(value)) => value,
                MarkdownGraphemeAnswer::Pending => return Ok(None),
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            let end = probe
                .end
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            if second.utf16 > end.utf16 {
                // SCX范围短于完整支持域；推进原Script后重新取得证明，不擅自扩大旧范围。
                payload.open_probe = None;
                payload.script_source_demand = true;
                return Ok(None);
            }
            let mut bounded = probe.run;
            bounded.end_utf16 = end.utf16;
            bounded.end_byte = end.byte;
            let open = probe
                .paragraph_end
                .is_none_or(|paragraph_end| bounded.end_utf16 < paragraph_end);
            payload.open_probe = None;
            // 稳定SCX已覆盖真实B端点时，原ItemScan可由该段/内联控制自然封闭；
            // 仅达到准备额度仍是开放范围，不能进入ClosedItem或制造字体边界。
            payload.open_script = open;
            return Ok(Some(bounded));
        }
        _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
    }
    Ok(None)
}

impl MarkdownLineItemCoordinator {
    pub(in crate::markdown_line_composer) fn take_grapheme_demand(&mut self) -> Result<bool> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            Ok(payload
                .breaks
                .take_grapheme_source_demand(&self.authority)?)
        })();
        self.finish(result)
    }

    /// GCB查询阻塞后只推进原GCB。原LB、Script、消费者游标均保留，预算不重复使用。
    pub(in crate::markdown_line_composer) fn advance_pending_graphemes(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineItemStep> {
        self.advance_pending_break_dependency(budget, true)
    }

    pub(super) fn advance_pending_break_dependency(
        &mut self,
        budget: MarkdownLineComposeBudget,
        grapheme: bool,
    ) -> Result<MarkdownLineItemStep> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !payload.frontier_enabled
                || (!payload.scripts_ready
                    && !payload
                        .bidi
                        .as_ref()
                        .is_some_and(MarkdownBidiParagraph::uses_prepared_paragraphs)
                    && !unicode_prefix_allowed(payload, &self.authority)?)
            {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            // 原B控制符与尾空段也可推进真实等待的依赖；各查询仍独立保Pending和原Source票。
            let progress = payload.breaks.step_dependency(
                &self.authority,
                grapheme,
                MarkdownLineBreakBudget {
                    bytes: budget.bytes.min(4096),
                    scalars: budget.scalars,
                    records: budget.records,
                },
            )?;
            payload.breaks_ready = progress.ready;
            Ok(MarkdownLineItemStep {
                bytes: progress.bytes,
                context_bytes: 0,
                scalars: progress.scalars,
                records: progress.records,
                metadata_units: 0,
                shapes: 0,
                answer: MarkdownLineItemAnswer::Yielded,
            })
        })();
        self.finish(result)
    }

    pub(super) fn advance_pending_scripts(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineItemStep> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            payload.script_source_demand = false;
            let progress = payload.scripts.step(
                &self.authority,
                MarkdownScriptBudget {
                    bytes: budget.bytes.min(4096),
                    scalars: budget.scalars,
                    records: budget.records,
                    stack_ops: budget.records,
                },
            )?;
            payload.scripts_ready = progress.status == crate::MarkdownScriptStatus::Ready;
            Ok(MarkdownLineItemStep {
                bytes: progress.bytes,
                context_bytes: 0,
                scalars: progress.scalars,
                records: progress.records,
                metadata_units: 0,
                shapes: 0,
                answer: MarkdownLineItemAnswer::Yielded,
            })
        })();
        self.finish(result)
    }
}
