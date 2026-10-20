//! 原协调器的显式前沿策略；只消费封闭证书，不以公开短Range创建glyph任务。
use super::*;
use crate::markdown_layout_breaks::MarkdownCertifiedGraphemeEndpoint;
use crate::markdown_shaping::frontier::{
    MarkdownShapingFrontierAnswer as Answer, MarkdownShapingFrontierContinuation,
    MarkdownShapingFrontierError, MarkdownShapingFrontierJob, MarkdownShapingFrontierReason,
    MarkdownShapingTerminalGrapheme,
};
#[cfg(feature = "s2-table-window-prototype")]
mod style_boundary;
#[cfg(feature = "s2-table-window-prototype")]
pub(super) use style_boundary::{has_style_boundary_probe, provide_boundary_style};

/// 未认证能力保持真实待决；这些原因不表示缺字、整段Ready或默认排版降级。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineFrontierReason {
    /// 原字体真实计划的窄证书拒绝；保留下层类型，调用者才能区分定位、查表与缺字。
    Plan(harfrust::AsciiFrontierRejection),
    /// 同一回退字体继续覆盖后继字素，尚未遇到真实字体边界且本次有界输入已用尽。
    FontRunBeyondBudget,
    NonAscii,
    WordBeyondBudget,
    NotGraphemeBoundary,
    PendingConsumer,
    UncertifiedWord,
    NegativeAdvance,
    NonSimpleGrapheme,
}

/// 只计原前沿的实际工作；全源UBA/Script/GCB元数据阶段仍由原step事实独立计量。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownLineFrontierWork {
    pub source_bytes: usize,
    pub input_bytes: usize,
    pub context_bytes: usize,
    pub shape_calls: usize,
    pub parts: usize,
    pub metadata_glyphs: usize,
    pub plan_records: usize,
    pub boundary_records: usize,
}

/// 仅一个未解析原item；续票和实际child弱身份不随已输出片数增长。
pub(super) struct PendingFrontier {
    owner: MarkdownShapingFrontierJob,
    original: MarkdownLayoutFont,
    font: MarkdownLayoutFontAuthority,
    registration: Option<MarkdownLineFontRegistration>,
    replay_style: MarkdownLineComposeStyle,
    facts: MarkdownLineItemFacts,
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    continuation: Option<MarkdownShapingFrontierContinuation>,
    child: Option<std::sync::Weak<()>>,
    complete: bool,
    open_script: bool,
    natural_tail: bool,
    terminal: Option<PendingTerminal>,
    declined_same_font: bool,
    declined_font_head: bool,
    // 原平台base只保弱注册；实际part完成后才对其有限GCB窗口审核字体选择，不预扫整个FontRun。
    preferred_registration: Option<MarkdownLineFontRegistration>,
    #[cfg(feature = "s2-table-window-prototype")]
    style_boundary: Option<style_boundary::StyleBoundary>,
}

/// 一个真实阻塞GCB开启的回退字体段；只存端点、已选Font及最多一个后继GCB，不存正文。
struct PendingTerminal {
    proof: MarkdownShapingTerminalGrapheme,
    endpoints: [Option<MarkdownCertifiedGraphemeEndpoint>; 2],
    response: Option<MarkdownLineStyleResponse>,
    probe: Option<PendingFontProbe>,
    next: Option<SelectedFontGrapheme>,
    prefix_returned: bool,
    boundary_ready: bool,
    installed: bool,
    beyond_limit: bool,
}

/// 只有原Next查询给出的一个完整相邻GCB；两张端点票分量子领取。
struct PendingFontProbe {
    range: Range<usize>,
    endpoints: [Option<MarkdownCertifiedGraphemeEndpoint>; 2],
}

/// 真实第三Font选择同时封闭前段和开启后段；等待前段原child归还后才消费。
struct SelectedFontGrapheme {
    response: MarkdownLineStyleResponse,
    endpoints: [MarkdownCertifiedGraphemeEndpoint; 2],
}

impl PendingTerminal {
    fn new(proof: MarkdownShapingTerminalGrapheme) -> Self {
        let boundary_ready = proof.source_range().end == proof.item_end();
        Self {
            proof,
            endpoints: [None, None],
            response: None,
            probe: None,
            next: None,
            prefix_returned: false,
            boundary_ready,
            installed: false,
            beyond_limit: false,
        }
    }
}
impl PendingFrontier {
    /// 首 Font 探测未取得真实字体右界后，保留同一失败计划及原 SCX/词证明续体。
    pub(in crate::markdown_line_composer::item) fn decline_font_head(&mut self) {
        self.declined_font_head = true;
    }

    pub(super) fn new(
        owner: MarkdownShapingFrontierJob,
        response: MarkdownLineStyleResponse,
        replay_style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        open_script: bool,
        natural_tail: bool,
    ) -> Self {
        let font = response.font.authority();
        Self {
            owner,
            original: response.font,
            font,
            registration: response.registration,
            replay_style,
            facts,
            shaping_seams: response.request.shaping_seams.clone(),
            continuation: None,
            child: None,
            complete: false,
            open_script,
            natural_tail,
            terminal: None,
            declined_same_font: false,
            declined_font_head: false,
            preferred_registration: response.preferred_registration,
            #[cfg(feature = "s2-table-window-prototype")]
            style_boundary: response
                .root_leaf_boundary
                .map(style_boundary::StyleBoundary::new),
        }
    }
}

/// 真实端点分别收费，拿齐后才签一次NeedStyle；没有新GCB索引或第二Source。
fn prepare_terminal_style(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let pending = payload.frontier.as_mut().expect("原Frontier仍持暂停末GCB");
    let terminal = pending.terminal.as_mut().expect("原末GCB票仍在唯一槽内");
    let range = terminal.proof.source_range();
    for (index, offset) in [range.start, range.end].into_iter().enumerate() {
        if terminal.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        payload.frontier_work.boundary_records = payload
            .frontier_work
            .boundary_records
            .checked_add(1)
            .ok_or(MarkdownLineComposeError::Overflow)?;
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, offset)?;
        match answer {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(actual)) if actual.utf16 == offset => {
                terminal.endpoints[index] =
                    Some(endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?);
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    } // 只有原两端票都已取得，Native字体worker才可能领取这个完整GCB。
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        range,
        scope: StyleScope::Text(TextStyleScope {
            paragraph: pending.facts.paragraph_utf16.clone(),
            script: pending.facts.script,
            level: pending.facts.level,
        }),
        shaping_seams: [None, None],
        continuation_font: Some(pending.font.clone()),
        continuation_bytes: Some(
            terminal.endpoints[0]
                .as_ref()
                .expect("原末 GCB 起点已认证")
                .boundary(authority)?
                .byte
                ..terminal.endpoints[1]
                    .as_ref()
                    .expect("原末 GCB 终点已认证")
                    .boundary(authority)?
                    .byte,
        ),
    };
    payload.request = Some(request.clone());
    payload.font_probe_scope = Some(Arc::new(()));
    payload.stage = Stage::Style;
    step.answer = MarkdownLineItemAnswer::NeedStyle(request);
    Ok(step)
}

/// 字体响应只改变真实Font选择；同Font中部仍没有上下文闭合证明，保留原能力拒绝。
pub(super) fn provide_terminal_style(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    response: MarkdownLineStyleResponse,
) -> Result<()> {
    let pending = payload.frontier.as_mut().expect("原Frontier持有字体暂停");
    let terminal = pending.terminal.as_mut().expect("原字体交接票仍在唯一槽内");
    let expected = terminal.probe.as_ref().map_or_else(
        || terminal.proof.source_range(),
        |probe| probe.range.clone(),
    );
    if response.coverage != expected
        || response.style.presentation != pending.replay_style.presentation
        || response.style.points.to_bits() != pending.replay_style.points.to_bits()
        || response.style.language != pending.replay_style.language
        || response.style.features != pending.replay_style.features
    {
        return Err(MarkdownLineComposeError::InvalidRequest);
    } // 字体试次不能顺带改样式，否则旧前缀不再属于同一个已认证输入。
    if let Some(registration) = &response.registration {
        let registered = registration.lease()?;
        if !response.font.permits(&registered.font().authority()) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
    }
    let original_selected = response.font.permits(&pending.font);
    if let Some(probe) = terminal.probe.take() {
        if original_selected {
            terminal.boundary_ready = true;
            // 原Font完整试次重新成功，当前回退段到此真实结束；后继原字素尚未消费。
        } else if response.font.permits(
            &terminal
                .response
                .as_ref()
                .expect("回退段持有已选Font")
                .font
                .authority(),
        ) {
            let [_, end] = probe.endpoints;
            terminal.endpoints[1] = end;
            let first = terminal.endpoints[0]
                .as_ref()
                .expect("段首原端点")
                .boundary(authority)?;
            let last = terminal.endpoints[1]
                .as_ref()
                .expect("试次前已取得段末票")
                .boundary(authority)?;
            terminal.beyond_limit = last.utf16 - first.utf16 > closed_item::MAX_CLOSED_UTF16
                || last.byte - first.byte > closed_item::MAX_CLOSED_BYTES;
            terminal.boundary_ready = last.utf16 == pending.facts.source_utf16.end;
            // 相同回退Font必须合并，不能用固定预算制造字体边界；过大真实段保持未支持。
        } else {
            terminal.next = Some(SelectedFontGrapheme {
                response,
                endpoints: probe
                    .endpoints
                    .map(|value| value.expect("完整相邻GCB原端点")),
            });
            terminal.boundary_ready = true;
            // 真正第三Font选择给出当前右边界；只暂存这一个已付费后继试次。
        }
    } else if original_selected && expected.end != pending.facts.source_utf16.end {
        pending.terminal = None;
        pending.declined_same_font = true;
        // 中部同Font成功不代表局部计划支持它；不造字体边界，也不无限重发同一试次。
    } else {
        pending
            .owner
            .resume_terminal_grapheme(authority, &pending.font, &terminal.proof, original_selected)
            .map_err(map_error)?;
        if original_selected {
            pending.terminal = None; // 原Font自然尾片沿已通过的原plan路径完成。
        } else {
            terminal.response = Some(response); // 先交付旧Font的实际残余，随后寻找真实右边界。
        }
    }
    payload.request.take();
    payload.font_probe_scope.take(); // 响应接受即撤原试次，后继Font不续用旧nonce。
    payload.stage = Stage::Frontier;
    Ok(())
}

/// 相邻GCB只在回退区逐个试次；原Font一旦重新成功，就恢复原来的批量排版。
fn advance_font_run(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    mut step: MarkdownLineItemStep,
) -> Result<MarkdownLineItemStep> {
    let pending = payload.frontier.as_mut().expect("唯一Font交接owner");
    let terminal = pending.terminal.as_mut().expect("真实Font段仍待消费");
    if terminal.beyond_limit {
        step.answer =
            MarkdownLineItemAnswer::NeedsFrontier(MarkdownLineFrontierReason::FontRunBeyondBudget);
        return Ok(step); // 有限完整输入上限不是真Font边界，绝不能截断这个真实连续Font段。
    }
    if terminal.boundary_ready {
        install_terminal_font(payload, authority)?;
        return Ok(step);
    }
    if terminal.probe.is_none() {
        let start = terminal.endpoints[1]
            .as_ref()
            .expect("已选段末端点")
            .boundary(authority)?
            .utf16;
        step.metadata_units += 1;
        payload.frontier_work.boundary_records = payload
            .frontier_work
            .boundary_records
            .checked_add(1)
            .ok_or(MarkdownLineComposeError::Overflow)?;
        let next = payload
            .breaks
            .grapheme_query(authority, start, MarkdownGraphemeQuery::Next)?;
        let end = match next {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(boundary))
                if boundary.utf16 > start && boundary.utf16 <= pending.facts.source_utf16.end =>
            {
                boundary.utf16
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        };
        terminal.probe = Some(PendingFontProbe {
            range: start..end,
            endpoints: [None, None],
        });
    }
    let probe = terminal.probe.as_mut().expect("原Next查询已确定相邻GCB");
    for (index, offset) in [probe.range.start, probe.range.end].into_iter().enumerate() {
        if probe.endpoints[index].is_some() {
            continue;
        }
        if step.metadata_units == budget.metadata_units {
            return Ok(step);
        }
        step.metadata_units += 1;
        payload.frontier_work.boundary_records = payload
            .frontier_work
            .boundary_records
            .checked_add(1)
            .ok_or(MarkdownLineComposeError::Overflow)?;
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, offset)?;
        match answer {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(actual)) if actual.utf16 == offset => {
                probe.endpoints[index] =
                    Some(endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?);
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    } // 两个原端点齐备后才发Native试次；没有正文读取、替代索引或推测行高。
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        range: probe.range.clone(),
        scope: StyleScope::Text(TextStyleScope {
            paragraph: pending.facts.paragraph_utf16.clone(),
            script: pending.facts.script,
            level: pending.facts.level,
        }),
        shaping_seams: [None, None],
        continuation_font: Some(pending.font.clone()),
        continuation_bytes: Some(
            probe.endpoints[0]
                .as_ref()
                .expect("原相邻 GCB 起点已认证")
                .boundary(authority)?
                .byte
                ..probe.endpoints[1]
                    .as_ref()
                    .expect("原相邻 GCB 终点已认证")
                    .boundary(authority)?
                    .byte,
        ),
    };
    payload.request = Some(request.clone());
    payload.font_probe_scope = Some(Arc::new(()));
    payload.stage = Stage::Style;
    step.answer = MarkdownLineItemAnswer::NeedStyle(request);
    Ok(step)
}

/// 左右都是真实Font边界或自然项端点，才装配这一完整Font段，保留原Frontier等待child归还。
fn install_terminal_font(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
) -> Result<()> {
    payload.source.health()?;
    let pending = payload.frontier.as_mut().expect("原Font残余owner仍活");
    let terminal = pending.terminal.as_mut().expect("真实Font边界票仍在");
    let response = terminal.response.take().expect("已接受的新Font响应");
    let endpoints = [
        terminal.endpoints[0].take().expect("原段首票"),
        terminal.endpoints[1].take().expect("原段末票"),
    ];
    let first = endpoints[0].boundary(authority)?;
    let last = endpoints[1].boundary(authority)?;
    let range = first.utf16..last.utf16;
    if payload.cursor != range.start
        || !terminal.boundary_ready
        || range.end > pending.facts.source_utf16.end
    {
        return Err(MarkdownLineComposeError::InconsistentCoverage);
    } // 旧中文已经原样交付；完整回退段不得跨过原SCX、样式或UBA范围。
    let font = response.font.authority();
    let closed = closed_item::ClosedItem::from_terminal_font_change(
        &terminal.proof,
        endpoints,
        authority,
        &font,
    )?;
    let mut facts = pending.facts.clone();
    facts.source_utf16 = range.clone();
    facts.beginning_of_text = range.start == facts.paragraph_utf16.start;
    facts.end_of_text = range.end == facts.paragraph_utf16.end;
    let style = response.style;
    let options = MarkdownShapingOptions {
        direction: facts.direction,
        script: facts.script,
        language: style.language.clone(),
        points: facts.points,
        beginning_of_text: facts.beginning_of_text,
        end_of_text: facts.end_of_text,
        features: style.features.clone(),
    };
    let shaping = crate::markdown_shaping::PendingClosedShaping::new(
        payload.source.clone(),
        authority,
        response.font,
        &font,
        closed,
        options,
    )?;
    terminal.installed = true;
    payload.closed_input = Some(closed_item::PendingItem {
        shaping,
        font,
        registration: response.registration,
        replay_style: style,
        facts,
        shaping_seams: [None, None],
    });
    payload.stage = Stage::ClosedInput;
    Ok(()) // 原owner继续持同一计划；新Font段真实归还后才允许移动其私有游标。
}

/// take_item已核实际Ready/EOF，此处登记同一个新Font child，不能用坐标回执代替真实归还。
pub(super) fn lend_font_item(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    item: &mut MarkdownLineItem,
) -> Result<()> {
    let Some(pending) = payload.frontier.as_mut() else {
        return Ok(());
    };
    let Some(terminal) = pending
        .terminal
        .as_ref()
        .filter(|terminal| terminal.installed)
    else {
        return Ok(());
    };
    let continuation = pending
        .owner
        .lend_font_run(authority, &pending.font, &terminal.proof, &mut item.job)
        .map_err(map_error)?;
    pending.child = Some(item.job.index_identity());
    pending.continuation = Some(continuation);
    pending.complete = item.facts.source_utf16.end == pending.facts.source_utf16.end;
    Ok(())
}

/// 前段真实child归还后，已付费第三Font试次才能成为下一段；普通原Font继续批量运行。
fn finish_font_run(payload: &mut Payload, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
    let pending = payload.frontier.as_mut().expect("归还Font段仍属于原owner");
    let mut terminal = pending.terminal.take().expect("原Font交接唯一槽");
    if let Some(next) = terminal.next.take() {
        let proof = pending
            .owner
            .following_font_grapheme(
                authority,
                &pending.font,
                &terminal.proof,
                [&next.endpoints[0], &next.endpoints[1]],
            )
            .map_err(map_error)?;
        let mut following = PendingTerminal::new(proof);
        following.endpoints = next.endpoints.map(Some);
        following.response = Some(next.response);
        following.prefix_returned = true;
        pending.terminal = Some(following);
    } // 没有后继回退Font时清空交接，原Font/plan/Script/Source全部继续沿原owner。
    Ok(())
}

impl MarkdownLineItemCoordinator {
    /// 显式候选才启用已认证前沿；原new继续作为完整item对照，不能混称惰性通过。
    pub fn new_with_frontier(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        let mut result = Self::new(source, authority, cancellation)?;
        result.enable_frontier()?;
        Ok(result)
    }
    pub(in crate::markdown_line_composer) fn enable_frontier(&mut self) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::Bidi) || payload.cursor != 0 {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        payload.frontier_enabled = true; // 首个step前选择一次，不允许运行中把旧完整Job补签成前沿。
        Ok(())
    }
    pub fn frontier_work(&self) -> Result<MarkdownLineFrontierWork> {
        self.guard()?;
        Ok(self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .frontier_work)
    }
}

pub(super) fn map_error(error: MarkdownShapingFrontierError) -> MarkdownLineComposeError {
    match error {
        MarkdownShapingFrontierError::Shaping(error) => MarkdownLineComposeError::Shaping(error),
        MarkdownShapingFrontierError::Breaks(error) => MarkdownLineComposeError::Break(error),
        MarkdownShapingFrontierError::ContinuationMismatch => {
            MarkdownLineComposeError::SourceMismatch
        }
        MarkdownShapingFrontierError::Poisoned => MarkdownLineComposeError::Poisoned,
        MarkdownShapingFrontierError::Advance(error) => {
            MarkdownLineComposeError::FrontierMetadata(Box::new(error))
        }
    }
}
fn reason(value: MarkdownShapingFrontierReason) -> MarkdownLineFrontierReason {
    match value {
        MarkdownShapingFrontierReason::Plan(reason) => MarkdownLineFrontierReason::Plan(reason),
        MarkdownShapingFrontierReason::NonAscii => MarkdownLineFrontierReason::NonAscii,
        MarkdownShapingFrontierReason::WordBeyondBudget => {
            MarkdownLineFrontierReason::WordBeyondBudget
        }
        MarkdownShapingFrontierReason::NotGraphemeBoundary => {
            MarkdownLineFrontierReason::NotGraphemeBoundary
        }
        MarkdownShapingFrontierReason::UncertifiedWord => {
            MarkdownLineFrontierReason::UncertifiedWord
        }
        MarkdownShapingFrontierReason::NegativeAdvance => {
            MarkdownLineFrontierReason::NegativeAdvance
        }
        MarkdownShapingFrontierReason::NonSimpleGrapheme => {
            MarkdownLineFrontierReason::NonSimpleGrapheme
        }
    }
}

pub(super) fn advance(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    cancellation: &MarkdownLineComposeCancellation,
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
    #[cfg(feature = "s2-table-window-prototype")]
    if has_style_boundary_probe(payload) {
        return style_boundary::prepare(payload, authority, budget, step);
    }
    if payload.frontier.as_ref().is_some_and(|pending| {
        pending
            .terminal
            .as_ref()
            .is_some_and(|tail| tail.response.is_none() && !tail.installed)
    }) {
        return prepare_terminal_style(payload, authority, budget, step);
    } // 查询端点占独立量子，不能在刚读完阻塞GCB的同一个已满预算里补做索引工作。
    {
        let pending = payload
            .frontier
            .as_mut()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        if pending.continuation.is_some() {
            if pending
                .child
                .as_ref()
                .is_none_or(|child| child.strong_count() != 0)
            {
                step.answer = MarkdownLineItemAnswer::NeedsFrontier(
                    MarkdownLineFrontierReason::PendingConsumer,
                );
                return Ok(step); // consumer未真实归还child时不调用会永久失败的resume，更不新建第二Job。
            }
            pending
                .owner
                .resume(
                    authority,
                    &pending.font,
                    pending
                        .continuation
                        .take()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                )
                .map_err(map_error)?;
            pending.child = None;
            if pending.terminal.as_ref().is_some_and(|run| run.installed) {
                finish_font_run(payload, authority)?;
            } else if let Some(run) = &mut pending.terminal {
                run.prefix_returned = true;
            } // 原Font残余和回退Font段都必须先归还实际child，不能只凭游标跳过Store。
            let pending = payload.frontier.as_mut().expect("原Frontier不在归还中丢失");
            if pending.complete {
                if (!pending.open_script && payload.cursor != pending.facts.source_utf16.end)
                    || (pending.open_script && payload.cursor >= pending.facts.source_utf16.end)
                {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                drop(payload.frontier.take());
                payload.stage = Stage::ItemStart;
                return Ok(step); // 闭合item到真末端，开放scope只到中心末端；二者都先归还实际child。
            }
        }
    }
    let pending = payload
        .frontier
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    if pending
        .terminal
        .as_ref()
        .is_some_and(|run| run.prefix_returned && !run.installed)
    {
        return advance_font_run(payload, authority, budget, step);
    }
    let progress = pending
        .owner
        .step(
            authority,
            &pending.font,
            &mut payload.breaks,
            MarkdownShapingBudget {
                bytes: budget.bytes.min(4096),
                scalars: budget.scalars,
            },
            budget.metadata_units,
        )
        .map_err(map_error)?;
    step.bytes = progress
        .source_bytes
        .checked_add(progress.input_bytes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    step.scalars = progress.scalars; // 真UTF8解码与child输入逐scalar计费，不以中文字节数冒充标量数。
    step.shapes = progress.shapes;
    step.context_bytes = progress.context_bytes;
    step.metadata_units = progress
        .metadata_glyphs
        .checked_add(progress.plan_records)
        .and_then(|units| units.checked_add(progress.boundary_records))
        .filter(|units| *units <= budget.metadata_units)
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    let work = &mut payload.frontier_work;
    work.source_bytes = work
        .source_bytes
        .checked_add(progress.source_bytes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.input_bytes = work
        .input_bytes
        .checked_add(progress.input_bytes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.context_bytes = work
        .context_bytes
        .checked_add(progress.context_bytes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.shape_calls = work
        .shape_calls
        .checked_add(progress.shapes)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.metadata_glyphs = work
        .metadata_glyphs
        .checked_add(progress.metadata_glyphs)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.plan_records = work
        .plan_records
        .checked_add(progress.plan_records)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    work.boundary_records = work
        .boundary_records
        .checked_add(progress.boundary_records)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    match progress.answer {
        Answer::Pending => {}
        Answer::NeedsFrontier(value) => {
            let at_unconsumed_head = payload.cursor == pending.facts.source_utf16.start
                && pending.child.is_none()
                && pending.terminal.is_none()
                && !pending.declined_font_head
                && (pending.facts.paragraph_utf16.end - payload.cursor
                    > closed_item::MAX_CLOSED_UTF16
                    || (!pending.open_script && !pending.natural_tail));
            if at_unconsumed_head
                && matches!(
                    value,
                    MarkdownShapingFrontierReason::Plan(
                        harfrust::AsciiFrontierRejection::Positioning
                            | harfrust::AsciiFrontierRejection::CrossInputSubtable
                            | harfrust::AsciiFrontierRejection::LookupBehavior
                    ) | MarkdownShapingFrontierReason::NonAscii
                )
            {
                let ascii_suffix = if pending.open_script {
                    let span = payload
                        .bidi
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .uniform_control_free_span(
                            authority,
                            payload.cursor..payload.cursor.saturating_add(1),
                        )?;
                    if let Some(span) = span {
                        payload.scripts.prepare_ascii_suffix(
                            authority,
                            payload.cursor,
                            span.content_end(),
                            span.paragraph_range().end,
                        )?
                    } else {
                        false
                    }
                } else {
                    false
                }; // 无原同源B正文范围就保FontRuns，不能从Source长度猜分隔符位置。
                if ascii_suffix {
                    match payload.scripts.run_at(
                        authority,
                        MarkdownScriptUnit::Utf16,
                        payload.cursor,
                    )? {
                        MarkdownScriptAnswer::Pending => payload.script_source_demand = true,
                        MarkdownScriptAnswer::Known(Some(_)) => {
                            // 原Script完整闭run真实存在后才撤原开放计划；同一Font/Style由原请求链重新装配。
                            payload.closed_script_at = Some(payload.cursor);
                            drop(payload.frontier.take());
                            payload.open_script = false;
                            payload.stage = Stage::Dependencies;
                        }
                        _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
                    }
                    return Ok(step); // 实际后缀证明在下一原Script量子执行，不偷用本次整形剩余额度。
                }
                // 后缀不是ASCII、上下文未结算或没有同源端点时仍用原FontRuns；emoji首Font不能沿用到巨项。
                // ASCII 字体的局部资格也可能先被后面的中文拒绝：起始空格选中原 Font，
                // 不代表同一样式交集中后继中文也使用它。NonAscii 同样只允许原完整 GCB
                // 字体试次；必须找到真实 Font 变化才封闭前段，无变化仍保留原精确拒绝。
                // Positioning 或跨输入 GSUB 拒绝都没有赋予任意切分权限。首 GCB 的实际 Font
                // 不证明整个 SCX 均可用，只在相邻完整 GCB 确认另一真实 Font 后封闭首 Font 段。
                // 同 Font 的组合字/连字不能因此被拆开；未找到变化只退役探测，继续原 SCX/词域证明。
                // 原owner仍持Source/Font/取消及资源scope，直到这个短段child真正归还才结束。
                // 原自然小项仍走已有闭项合同；只有尚未消费的非自然短样式项也可试真实Font边界。
                // 例如“空格、中文、空格”首字素选中Mono，并不证明该Font覆盖中间中文；原计划可能
                // 在缺字处理前就拒绝跨输入查表。这里只开放同GCB试次机会，不授予短样式末端闭合权：
                // 只有后继确实选出另一原Font才交付，直到coverage末端仍相同则原样退役并保精确拒绝。
                payload.closed_fonts = Some(closed_item::FontRuns::frontier_head(
                    authority.clone(),
                    pending.original.clone(),
                    pending.registration.clone(),
                    pending.replay_style.clone(),
                    pending.facts.clone(),
                    cancellation.shaping(),
                    pending.shaping_seams.clone(),
                ));
                payload.stage = Stage::ClosedFonts;
                return Ok(step);
            }
            #[cfg(feature = "s2-table-window-prototype")]
            if style_boundary::begin(payload)? {
                // 原head量子已经使用了本次计划预算；右侧GCB从下一step独立收费，
                // 不把同一个metadata配额先交旧owner再偷偷交第二次目录查询。
                return Ok(step);
            }
            let pending = payload.frontier.as_mut().expect("原失败计划仍在同owner内");
            if pending.open_script {
                // 开放SCX没有资格退到有限context的ASCII计划；保持原run算法直到真实闭合，
                // 再请求对应完整交集的样式。旧响应和资源真实归还，不扩充其Font覆盖权限。
                payload.closed_script_at = Some(payload.cursor);
                drop(payload.frontier.take());
                payload.open_script = false;
                payload.stage = Stage::Dependencies;
            } else {
                if pending.natural_tail
                    && !pending.declined_same_font
                    && let Some(proof) = pending
                        .owner
                        .terminal_grapheme(authority, &pending.font)
                        .map_err(map_error)?
                {
                    pending.terminal = Some(PendingTerminal::new(proof));
                } else {
                    step.answer = MarkdownLineItemAnswer::NeedsFrontier(reason(value));
                } // 只对原完整SCX中的实际阻塞GCB重试；缩短样式和已拒同Font中部仍不升权。
            }
        } // 未认证计划/正文保持待决，没有完整item fallback隐藏首行glyph工作。
        Answer::PartReady => {
            let part = pending
                .owner
                .take_part(authority, &pending.font)
                .map_err(map_error)?;
            let range = part.certified_range();
            let item_complete = part.is_item_complete();
            let complete = part.is_scope_complete();
            if part.original_range() != pending.facts.source_utf16
                || range.start != payload.cursor
                || range.is_empty()
                || range.end > pending.facts.source_utf16.end
                || item_complete != (range.end == pending.facts.source_utf16.end)
                || (pending.open_script && item_complete)
            {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            let audit = closed_item::FontRuns::for_frontier_part(
                &part,
                pending.original.clone(),
                pending.registration.clone(),
                pending.replay_style.clone(),
                pending.facts.clone(),
                pending.preferred_registration.clone(),
            )?;
            let (mut job, continuation) = part.into_parts();
            let profile = job.profile()?;
            job.glyph(authority, &pending.font, profile.glyph_count)?;
            let mut facts = pending.facts.clone();
            facts.source_utf16 = range;
            facts.beginning_of_text = facts.source_utf16.start == facts.paragraph_utf16.start;
            facts.end_of_text = facts.source_utf16.end == facts.paragraph_utf16.end;
            pending.child = Some(job.index_identity());
            pending.continuation = Some(continuation);
            pending.complete = complete;
            let mut shaping_seams = [None, None];
            for (index, seam) in pending.shaping_seams.iter().enumerate() {
                let Some(seam) = seam else { continue };
                let (_, at) = seam.boundary(authority)?;
                let endpoint = if index == 0 {
                    facts.source_utf16.start
                } else {
                    facts.source_utf16.end
                };
                if at == endpoint {
                    shaping_seams[index] = Some(seam.clone());
                }
            } // 原 Source 错配必须返回 typed 错误，不能靠 .ok() 悄悄抹去内部 shaping 票。
            payload.item = Some(MarkdownLineItem {
                job: crate::markdown_line_composer::worker_resources::ShapeResourceOwner::new(
                    job,
                    &payload.resources,
                ),
                source: payload.source.clone(),
                authority: authority.clone(),
                font: pending.font.clone(),
                registration: pending.registration.clone(),
                replay_style: pending.replay_style.clone(),
                facts: facts.clone(),
                shaping_seams,
            });
            work.parts = work
                .parts
                .checked_add(1)
                .ok_or(MarkdownLineComposeError::Overflow)?;
            payload.stage = Stage::Ready;
            step.answer = MarkdownLineItemAnswer::ItemReady(facts);
            if let Some(audit) = audit {
                // 只暂借刚封存的同一个原Job；没有第二字形图。未发现Font变化时原continuation
                // 原封交回；真实变化后的旧parent必须等重形child归还才退役，不能resume到整part末端。
                payload.closed_fonts = Some(audit);
                payload.stage = Stage::ClosedFonts;
                step.answer = MarkdownLineItemAnswer::Yielded;
            }
        } // 只移交实际Ready子任务；Store完成后才能归还私有Continuation并推进下一前沿。
        Answer::PendingConsumer | Answer::Complete => {
            return Err(MarkdownLineComposeError::InconsistentCoverage);
        } // 这两个状态只能通过上面的原ticket交接完成，不能绕过独立child真实释放。
    }
    Ok(step)
}
