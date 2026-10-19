//! 自然小项或首计划拒绝后的真实Font段；相邻同Font字素合并重形，绝不切旧字形数组。

use super::*;

/// 只接原自然闭项或首Positioning拒绝；最多持当前Font段和一个后继，不随文档长度增长。
pub(in crate::markdown_line_composer::item) struct FontRuns {
    source: MarkdownLayoutTextAuthority,
    scope: Arc<()>,
    cancellation: MarkdownShapingCancellation,
    original: MarkdownLayoutFont,
    registration: Option<MarkdownLineFontRegistration>,
    style: MarkdownLineComposeStyle,
    facts: MarkdownLineItemFacts,
    phase: Phase,
    probe: Option<Probe>,
    run: Option<Run>,
    emit: Option<Run>,
    endpoints: [Option<MarkdownCertifiedGraphemeEndpoint>; 2],
    awaiting: Option<(Range<usize>, MarkdownLayoutFontAuthority)>,
    child: Option<std::sync::Weak<()>>,
    // 首项计划拒绝时只有真实不同Font可封口；开放SCX端点和预算都不是自然右界。
    frontier_head: bool,
}

/// 只对应同一原项的字形检查、GCB试次和Font段交付，不承接调度或Source重捕。
enum Phase {
    Scan {
        ordinal: usize,
        count: usize,
        missing: Option<usize>,
    },
    Boundary(usize),
    Previous(usize),
    Probe,
    Emit,
    AwaitChild,
}

/// 范围来自原GCB的Next及两张端点票；尚未完整取得时不能签发字体请求。
struct Probe {
    range: Range<usize>,
    endpoints: [Option<MarkdownCertifiedGraphemeEndpoint>; 2],
}

/// 当前范围只随真实GCB响应推进；封口只认Font变化或自然端点，Font不按名字重建。
struct Run {
    range: Range<usize>,
    font: MarkdownLayoutFont,
    registration: Option<MarkdownLineFontRegistration>,
}

impl FontRuns {
    pub(in crate::markdown_line_composer::item) fn new(
        closed: &ClosedItem,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
    ) -> Self {
        Self {
            source: closed.source.clone(),
            scope: Arc::new(()),
            cancellation: closed.cancellation.clone(),
            original,
            registration,
            style,
            facts,
            phase: Phase::Scan {
                ordinal: 0,
                count: 0,
                missing: None,
            },
            probe: None,
            run: None,
            emit: None,
            endpoints: [None, None],
            awaiting: None,
            child: None,
            frontier_head: false,
        }
    }

    /// 仅原首项Positioning拒绝可进入；共享原GCB和Font，尚不创建完整输入或任何字形。
    pub(in crate::markdown_line_composer::item) fn frontier_head(
        source: MarkdownLayoutTextAuthority,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        cancellation: MarkdownShapingCancellation,
    ) -> Self {
        let start = facts.source_utf16.start;
        Self {
            source,
            scope: Arc::new(()),
            cancellation,
            original,
            registration,
            style,
            facts,
            phase: Phase::Probe,
            probe: Some(Probe {
                range: start..start,
                endpoints: [None, None],
            }),
            run: None,
            emit: None,
            endpoints: [None, None],
            awaiting: None,
            child: None,
            frontier_head: true,
        }
    }

    /// 原Job真实Ready之后才领取glyph计数；分段子Job不再次递归触发同一字体选择。
    fn inspect(&mut self, item: &mut MarkdownLineItem) -> Result<bool> {
        if let Phase::Scan { count, .. } = &mut self.phase {
            *count = item.job.profile()?.glyph_count;
            return Ok(true);
        }
        Ok(false)
    }

    /// 当前段到自然末端即可输出，否则只探测紧邻的完整GCB，绝不按预算制造Font边界。
    fn following(&mut self) {
        let run = self.run.as_ref().expect("原闭项仍持当前真实Font段");
        if run.range.end == self.facts.source_utf16.end && !self.frontier_head {
            self.emit = self.run.take();
            self.phase = Phase::Emit;
        } else {
            self.probe = Some(Probe {
                range: run.range.end..run.range.end,
                endpoints: [None, None],
            });
            self.phase = Phase::Probe;
        }
    }
}

pub(in crate::markdown_line_composer::item) fn inspect_shaped(
    payload: &mut Payload,
) -> Result<bool> {
    match (&mut payload.closed_fonts, &mut payload.item) {
        (Some(fonts), Some(item)) => fonts.inspect(item),
        _ => Ok(false), // 旧完整Job与原Frontier child均保持原整形合同。
    }
}

/// 只在原协调器Ready被真实消费时登记child，不能用数字游标冒充分页字形数组已归还。
pub(in crate::markdown_line_composer::item) fn lend_font_item(
    payload: &mut Payload,
    item: &MarkdownLineItem,
) -> Result<()> {
    let Some(fonts) = payload.closed_fonts.as_mut() else {
        return Ok(());
    };
    let (range, font) = fonts
        .awaiting
        .as_ref()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    if !matches!(fonts.phase, Phase::AwaitChild)
        || item.facts.source_utf16 != *range
        || !item.font.same_payload(font)
    {
        return Err(MarkdownLineComposeError::SourceMismatch);
    }
    fonts.child = Some(item.job.index_identity());
    Ok(())
}

/// 每个响应必须仍是同一个样式与完整GCB；仅实际Font身份可以发生变化。
pub(in crate::markdown_line_composer::item) fn provide_font_style(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    response: MarkdownLineStyleResponse,
) -> Result<()> {
    let fonts = payload.closed_fonts.as_mut().expect("原闭项持有字体试次");
    let probe = fonts.probe.as_ref().expect("原NeedStyle来自一个完整GCB");
    if fonts.source != *authority
        || response.coverage != probe.range
        || response.style.points.to_bits() != fonts.style.points.to_bits()
        || response.style.language != fonts.style.language
        || response.style.features != fonts.style.features
    {
        return Err(MarkdownLineComposeError::InvalidRequest);
    } // 字体选择不能顺带替换原Script、UBA、段落或有限样式。
    if let Some(registration) = &response.registration {
        let actual = registration.lease()?;
        if !response.font.permits(&actual.font().authority()) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
    }
    let first = fonts.run.is_none();
    if first && !fonts.frontier_head && response.font.permits(&fonts.original.authority()) {
        // 显式同Font响应没有解决真实glyph0。保留原完整结果供原Measure报告缺字，
        // 不伪造成功，也不无限重发相同试次；旧纯Core缺字oracle仍能观察原结果。
        payload.closed_fonts = None;
        payload.stage = Stage::Ready;
    } else {
        let range = fonts.probe.take().expect("原完整字素仍在").range;
        if first {
            // 直到确实选择另一实际Font，原完整Job才归还；前缀必须重新整形，不能切其数组。
            if !fonts.frontier_head {
                drop(payload.item.take());
            } // 首计划尚无Job；自然小项才需要归还先前实际全项整形结果。
            if !fonts.frontier_head && fonts.facts.source_utf16.start < range.start {
                fonts.emit = Some(Run {
                    range: fonts.facts.source_utf16.start..range.start,
                    font: fonts.original.clone(),
                    registration: fonts.registration.clone(),
                });
                fonts.phase = Phase::Emit;
            }
            fonts.run = Some(Run {
                range,
                font: response.font,
                registration: response.registration,
            });
            if fonts.emit.is_none() {
                fonts.following();
            }
        } else if response
            .font
            .permits(&fonts.run.as_ref().expect("当前Font段仍在").font.authority())
        {
            fonts.run.as_mut().expect("当前Font段仍在").range.end = range.end;
            fonts.following(); // 同实际Font相邻字素必须合并，不能一字素一条样式项。
        } else {
            fonts.emit = fonts.run.take();
            if !fonts.frontier_head {
                fonts.run = Some(Run {
                    range,
                    font: response.font,
                    registration: response.registration,
                });
            } // 真Font变化只封闭首段；长后文回原ItemStart，不把付费后继GCB拆成新短样式项。
            fonts.phase = Phase::Emit; // 自然小项保留唯一后继；首Frontier只交付已被真实Font变化封闭的前段。
        }
        payload.stage = Stage::ClosedFonts;
    }
    payload.request.take();
    payload.font_probe_scope.take(); // 响应消费后原试字票立即失效，后继不能复用旧nonce。
    Ok(())
}

/// glyph记录与GCB目录分别使用调用方原records/metadata量子；从不复制整项glyph Vec。
pub(in crate::markdown_line_composer::item) fn advance_fonts(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
) -> Result<MarkdownLineItemStep> {
    let mut step = MarkdownLineItemStep {
        bytes: 0,
        context_bytes: 0,
        scalars: 0,
        records: 0,
        metadata_units: 0,
        shapes: 0,
        answer: MarkdownLineItemAnswer::Yielded,
    };
    let fonts = payload
        .closed_fonts
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    let previous = matches!(fonts.phase, Phase::Previous(_));
    match &mut fonts.phase {
        Phase::Scan {
            ordinal,
            count,
            missing,
        } => {
            let item = payload.item.as_mut().expect("原完整Job尚未发布");
            while *ordinal < *count && step.records < budget.records {
                let glyph = item
                    .glyph(*ordinal)?
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                *ordinal += 1;
                step.records += 1;
                if glyph.glyph_id == 0 {
                    *missing = Some(missing.map_or(glyph.source_utf16_cluster, |old| {
                        old.min(glyph.source_utf16_cluster)
                    }));
                } // RTL视觉数组顺序不代表源序；取逻辑最早缺字cluster后再查真实GCB。
            }
            payload.frontier_work.metadata_glyphs = payload
                .frontier_work
                .metadata_glyphs
                .checked_add(step.records)
                .ok_or(MarkdownLineComposeError::Overflow)?;
            if *ordinal == *count {
                if let Some(offset) = *missing {
                    fonts.phase = Phase::Boundary(offset);
                } else {
                    payload.closed_fonts = None;
                    payload.stage = Stage::Ready;
                    step.answer = MarkdownLineItemAnswer::ItemReady(item.facts.clone());
                } // 无缺字就交付原完整Job，不改变连字、上下文或字形索引。
            }
        }
        Phase::Boundary(offset) | Phase::Previous(offset) => {
            let offset = *offset;
            step.metadata_units = 1;
            let answer = payload.breaks.grapheme_query(
                authority,
                offset,
                if previous {
                    MarkdownGraphemeQuery::Previous
                } else {
                    MarkdownGraphemeQuery::IsBoundary
                },
            )?;
            match answer {
                MarkdownGraphemeAnswer::Pending => {} // 仍由原Break Job后继量子准备，不另建目录。
                MarkdownGraphemeAnswer::Known(Some(boundary))
                    if boundary.utf16 <= offset
                        && boundary.utf16 >= fonts.facts.source_utf16.start =>
                {
                    fonts.probe = Some(Probe {
                        range: boundary.utf16..boundary.utf16,
                        endpoints: [None, None],
                    });
                    fonts.phase = Phase::Probe;
                }
                MarkdownGraphemeAnswer::Known(None) if !previous => {
                    fonts.phase = Phase::Previous(offset)
                }
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            } // HB cluster仅定位缺字，完整范围必须由原UAX29目录给出。
        }
        Phase::Probe => return prepare_probe(payload, authority, budget, step),
        Phase::Emit => return prepare_run(payload, authority, budget, step),
        Phase::AwaitChild => {
            let child = fonts
                .child
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            if child.strong_count() != 0 {
                step.answer = MarkdownLineItemAnswer::NeedsFrontier(
                    MarkdownLineFrontierReason::PendingConsumer,
                );
            } else {
                fonts.child = None;
                fonts.awaiting = None;
                if fonts.run.is_some() {
                    fonts.following();
                } else {
                    if fonts.frontier_head {
                        drop(payload.frontier.take()); // 首段原child已实际归还，此时才结束原失败计划及Font强边。
                    } else if payload.cursor != fonts.facts.source_utf16.end {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    }
                    payload.closed_fonts = None;
                    payload.stage = Stage::ItemStart;
                } // 最后一段真实释放后才归还原Font/有限样式，不保活历史Job或Source。
            }
        }
    }
    Ok(step)
}

/// 完整相邻字素及两张端点票取得后才暴露原NeedStyle，任何Pending都不签发范围。
fn prepare_probe(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let fonts = payload.closed_fonts.as_mut().expect("原闭项字体owner仍活");
    let probe = fonts.probe.as_mut().expect("真实待探字素仍在");
    if fonts.frontier_head
        && (probe.range.start == fonts.facts.source_utf16.end
            || fonts
                .run
                .as_ref()
                .is_some_and(|run| run.range.len() > MAX_CLOSED_UTF16))
    {
        step.answer =
            MarkdownLineItemAnswer::NeedsFrontier(MarkdownLineFrontierReason::UnsupportedPlan);
        return Ok(step); // 未出现真实不同Font，不能把开放SCX上界或有限闭段上限伪装为右边界。
    }
    if probe.range.is_empty() {
        step.metadata_units += 1;
        match payload.breaks.grapheme_query(
            authority,
            probe.range.start,
            MarkdownGraphemeQuery::Next,
        )? {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(end))
                if end.utf16 > probe.range.start && end.utf16 <= fonts.facts.source_utf16.end =>
            {
                probe.range.end = end.utf16
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    for (index, offset) in [probe.range.start, probe.range.end].into_iter().enumerate() {
        if probe.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, offset)?;
        match answer {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 == offset => {
                probe.endpoints[index] =
                    Some(endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?);
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        range: probe.range.clone(),
        paragraph: fonts.facts.paragraph_utf16.clone(),
        script: fonts.facts.script,
        level: fonts.facts.level,
        continuation_font: Some(fonts.original.authority()),
    };
    payload.request = Some(request.clone());
    payload.font_probe_scope = Some(Arc::new(()));
    payload.stage = Stage::Style;
    step.answer = MarkdownLineItemAnswer::NeedStyle(request);
    Ok(step)
}

/// 两边均由原自然项或真实Font选择封闭；每个完整Font段走原上下文准备与真实shape。
fn prepare_run(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let fonts = payload.closed_fonts.as_mut().expect("原Font分段仍活");
    let run = fonts.emit.as_ref().expect("原已封闭Font段仍在");
    for (index, offset) in [run.range.start, run.range.end].into_iter().enumerate() {
        if fonts.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, offset)?;
        match answer {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(value)) if value.utf16 == offset => {
                fonts.endpoints[index] =
                    Some(endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?);
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    let endpoints = fonts
        .endpoints
        .each_mut()
        .map(|value| value.take().expect("原Font段两张GCB票齐备"));
    let first = endpoints[0].boundary(authority)?;
    let last = endpoints[1].boundary(authority)?;
    if fonts.source != *authority
        || !endpoints[0].same_scope(&endpoints[1])
        || first.utf16 != payload.cursor
        || first.utf16 != run.range.start
        || last.utf16 != run.range.end
        || first.utf16 < fonts.facts.source_utf16.start
        || last.utf16 > fonts.facts.source_utf16.end
        || first.utf16 >= last.utf16
        || last.utf16 - first.utf16 > MAX_CLOSED_UTF16
        || last.byte - first.byte > MAX_CLOSED_BYTES
    {
        return Err(MarkdownLineComposeError::InconsistentCoverage);
    } // 私有原闭项owner与原GCB共同授范围；裸Range、HB cluster或预算断点都不能取得此票。
    let run = fonts.emit.take().expect("原真实Font段尚未移交");
    let font = run.font.authority();
    if let Some(registration) = &run.registration {
        let actual = registration.lease()?;
        if !run.font.permits(&actual.font().authority()) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
    }
    let closed = ClosedItem {
        source: authority.clone(),
        font: font.clone(),
        scope: Arc::downgrade(&fonts.scope),
        endpoints,
        cancellation: fonts.cancellation.clone(),
    };
    let mut facts = fonts.facts.clone();
    facts.source_utf16 = run.range.clone();
    facts.beginning_of_text = run.range.start == facts.paragraph_utf16.start;
    facts.end_of_text = run.range.end == facts.paragraph_utf16.end;
    let options = MarkdownShapingOptions {
        direction: facts.direction,
        script: facts.script,
        language: fonts.style.language.clone(),
        points: facts.points,
        beginning_of_text: facts.beginning_of_text,
        end_of_text: facts.end_of_text,
        features: fonts.style.features.clone(),
    };
    let shaping = crate::markdown_shaping::PendingClosedShaping::new(
        payload.source.clone(),
        authority,
        run.font,
        &font,
        closed,
        options,
    )?;
    fonts.awaiting = Some((run.range, font.clone()));
    fonts.phase = Phase::AwaitChild;
    payload.closed_input = Some(PendingItem {
        shaping,
        font,
        registration: run.registration,
        replay_style: fonts.style.clone(),
        facts,
    });
    payload.stage = Stage::ClosedInput;
    Ok(step) // 这里只交权；原邻接context和中心正文在后继独立bytes/scalars量子实际读取。
}

#[cfg(test)]
mod initial_tests;
