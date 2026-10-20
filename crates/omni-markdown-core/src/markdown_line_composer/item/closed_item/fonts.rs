//! 自然小项或首计划拒绝后的真实Font段；相邻同Font字素合并重形，绝不切旧字形数组。

use super::*;

/// 只接原自然闭项或首定位/跨输入计划拒绝；最多持当前Font段和一个后继，不随文档长度增长。
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
    endpoints: [Option<CertifiedShapingEndpoint>; 2],
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    awaiting: Option<(Range<usize>, MarkdownLayoutFontAuthority)>,
    child: Option<std::sync::Weak<()>>,
    // 首项计划拒绝时只有真实不同Font可封口；开放SCX端点和预算都不是自然右界。
    frontier_head: bool,
    audit: Option<FontAudit>,
}

/// 审核只借原自然项或刚封存的安全part；后者的末端不是新Font的自然边界。
enum FontAuditOrigin {
    Natural,
    Frontier,
}
struct FontAudit {
    // palette仍是注册的唯一强owner；在途审核不延长Native字体，也不创建第二字体目录。
    preferred: MarkdownLineFontRegistration,
    origin: FontAuditOrigin,
    range: Range<usize>,
}
impl FontAudit {
    fn new(
        preferred: Option<MarkdownLineFontRegistration>,
        selected: &MarkdownLayoutFont,
        origin: FontAuditOrigin,
        range: Range<usize>,
    ) -> Result<Option<Self>> {
        let Some(preferred) = preferred else {
            return Ok(None);
        };
        let primary = preferred.lease()?;
        if selected.permits(&primary.font().authority()) {
            return Ok(None); // 真选中原base时无需为整个窗口增加同一份选择审核。
        }
        Ok(Some(Self {
            preferred,
            origin,
            range,
        }))
    }
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
    ReuseOriginal,
    UncertifiedSupport,
}

/// 范围来自原GCB的Next及两张端点票；尚未完整取得时不能签发字体请求。
struct Probe {
    range: Range<usize>,
    endpoints: [Option<CertifiedShapingEndpoint>; 2],
}

/// 当前范围只随真实GCB响应推进；封口只认Font变化或自然端点，Font不按名字重建。
struct Run {
    range: Range<usize>,
    font: MarkdownLayoutFont,
    registration: Option<MarkdownLineFontRegistration>,
}

impl FontRuns {
    /// 已交付 child 的原 Font owner 可以等下一正常 step 归还；选择改变不需要重造它。
    /// 但能力要覆盖其完整自然项及仍保留的有限后继，不能只检查刚发布软行的末端。
    /// 所有范围来自这个 owner 已接纳的真实响应；这里不领取 GCB、不读取正文或补签字体。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(in crate::markdown_line_composer::item) fn retains_selection_styles(
        &self,
        source: &MarkdownLayoutTextAuthority,
        proven_end: usize,
    ) -> Result<bool> {
        if &self.source != source {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if self.cancellation.requested() {
            return Err(MarkdownLineComposeError::Cancelled);
        }
        let inside = |range: &Range<usize>| range.start <= range.end && range.end <= proven_end;
        if !matches!(self.phase, Phase::AwaitChild)
            || self
                .child
                .as_ref()
                .is_none_or(|child| child.strong_count() != 0)
            || !inside(&self.facts.source_utf16)
            || self.run.as_ref().is_some_and(|run| !inside(&run.range))
            || self.emit.as_ref().is_some_and(|run| !inside(&run.range))
            || self
                .probe
                .as_ref()
                .is_some_and(|probe| !inside(&probe.range))
            || self
                .audit
                .as_ref()
                .is_some_and(|audit| !inside(&audit.range))
            || self
                .awaiting
                .as_ref()
                .is_none_or(|(range, _)| !inside(range))
        {
            return Ok(false);
        }
        // 只借仍活着的原注册核实际 Font payload；不注册同名替身，也不把旧弱票升级为新代次。
        if let Some(registration) = &self.registration
            && !self.original.permits(&registration.authority()?)
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        for run in [&self.run, &self.emit].into_iter().flatten() {
            if let Some(registration) = &run.registration
                && !run.font.permits(&registration.authority()?)
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
        }
        if let Some(audit) = &self.audit {
            audit.preferred.lease()?;
        }
        Ok(true)
    }

    pub(in crate::markdown_line_composer::item) fn new(
        closed: &ClosedItem,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        preferred: Option<MarkdownLineFontRegistration>,
    ) -> Result<Self> {
        let audit = FontAudit::new(
            preferred,
            &original,
            FontAuditOrigin::Natural,
            facts.source_utf16.clone(),
        )?;
        Ok(Self {
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
            shaping_seams: std::array::from_fn(|index| closed.endpoints[index].seam().cloned()),
            awaiting: None,
            child: None,
            frontier_head: false,
            audit,
        })
    }

    /// 只能借真实封存part构造审核范围；不接受调用方坐标或把安全末端升级为新Font末端。
    pub(in crate::markdown_line_composer::item) fn for_frontier_part(
        part: &crate::markdown_shaping::frontier::MarkdownShapingFrontierPart,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        mut facts: MarkdownLineItemFacts,
        preferred: Option<MarkdownLineFontRegistration>,
    ) -> Result<Option<Self>> {
        let Some(audit) = FontAudit::new(
            preferred,
            &original,
            FontAuditOrigin::Frontier,
            part.shaping_range(),
        )?
        else {
            return Ok(None);
        };
        facts.source_utf16 = part.certified_range();
        // 审核上界来自已经完成的原安全窗口，而非整个SCX/样式run；4MiB同Font正文
        // 每次也只审这一固定part。原Job仍在父item槽，累计Source/metadata配额没有重置。
        let start = audit.range.start;
        Ok(Some(Self {
            source: part.source_authority().clone(),
            scope: Arc::new(()),
            cancellation: part.cancellation(),
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
            shaping_seams: part.shaping_seams().clone(),
            awaiting: None,
            child: None,
            frontier_head: false,
            audit: Some(audit),
        }))
    }

    /// 仅原首项定位或跨输入计划拒绝可进入；共享原GCB和Font，尚不创建完整输入或任何字形。
    pub(in crate::markdown_line_composer::item) fn frontier_head(
        source: MarkdownLayoutTextAuthority,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        cancellation: MarkdownShapingCancellation,
        shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
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
            shaping_seams,
            awaiting: None,
            child: None,
            frontier_head: true,
            audit: None,
        }
    }

    /// 仅原相邻语义票加完整右GCB的实际不同Font，才可调用此内部转移。
    /// 原head试次已证明左短项内没有更早Font边界；右字素未消费，等左child归还再重新请求。
    #[cfg(feature = "s2-table-window-prototype")]
    pub(in crate::markdown_line_composer::item) fn style_boundary(
        source: MarkdownLayoutTextAuthority,
        original: MarkdownLayoutFont,
        registration: Option<MarkdownLineFontRegistration>,
        style: MarkdownLineComposeStyle,
        facts: MarkdownLineItemFacts,
        cancellation: MarkdownShapingCancellation,
        shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    ) -> Self {
        let emit = Run {
            range: facts.source_utf16.clone(),
            font: original.clone(),
            registration: registration.clone(),
        };
        let mut fonts = Self::frontier_head(
            source,
            original,
            registration,
            style,
            facts,
            cancellation,
            shaping_seams,
        );
        fonts.probe = None;
        fonts.emit = Some(emit);
        fonts.phase = Phase::Emit;
        fonts
    }

    /// 原Job真实Ready之后才领取glyph计数；分段子Job不再次递归触发同一字体选择。
    fn inspect(&mut self, item: &mut MarkdownLineItem) -> Result<bool> {
        if let Phase::Scan { count, .. } = &mut self.phase {
            *count = item.job.profile()?.glyph_count;
            if let Some(audit) = &self.audit {
                let start = audit.range.start;
                self.probe = Some(Probe {
                    range: start..start,
                    endpoints: [None, None],
                });
                self.phase = Phase::Probe;
            } // 原Ready Job仍在唯一item槽；优先字体审核不为首GCB或整个窗口重新shape。
            return Ok(true);
        }
        Ok(false)
    }

    /// 当前段到自然末端即可输出，否则只探测紧邻的完整GCB，绝不按预算制造Font边界。
    fn following(&mut self) {
        let run = self.run.as_ref().expect("原闭项仍持当前真实Font段");
        let end = self
            .audit
            .as_ref()
            .map_or(self.facts.source_utf16.end, |audit| audit.range.end);
        if run.range.end == end && !self.frontier_head {
            if self.audit.is_some() {
                self.phase = Phase::ReuseOriginal;
                return; // 全窗口实际仍选同Font，原glyph与continuation无需任何改写。
            }
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

    /// 只返回原 item 精确两端的真实 UBA 票；Font/GCB 中间坐标不能从范围自行补签。
    fn seam_at(&self, offset: usize) -> Result<Option<ResolvedBidiShapingSeam>> {
        seam_at(&self.shaping_seams, &self.source, offset)
    }

    fn seams_for(&self, range: &Range<usize>) -> Result<[Option<ResolvedBidiShapingSeam>; 2]> {
        Ok([self.seam_at(range.start)?, self.seam_at(range.end)?])
    }
}

fn seam_at(
    seams: &[Option<ResolvedBidiShapingSeam>; 2],
    source: &MarkdownLayoutTextAuthority,
    offset: usize,
) -> Result<Option<ResolvedBidiShapingSeam>> {
    for seam in seams.iter().flatten() {
        if seam.boundary(source)?.1 == offset {
            return Ok(Some(seam.clone()));
        }
    }
    Ok(None)
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
        || response.style.presentation != fonts.style.presentation
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
    if fonts.audit.is_some() {
        // 与“缺字修复”用途区分：首GCB仍选原Font是正常选择结果，不能据此提前跳过后继base恢复。
        return provide_audit_style(payload, response);
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

/// 审核原Ready窗口的真实字体策略；只保当前同Font段，在首次变化后交还未消费余文。
fn provide_audit_style(payload: &mut Payload, response: MarkdownLineStyleResponse) -> Result<()> {
    let fonts = payload.closed_fonts.as_mut().expect("原字体审核owner仍活");
    let audit = fonts.audit.as_ref().expect("审核响应只进入原审核用途");
    let preferred = response
        .preferred_registration
        .as_ref()
        .ok_or(MarkdownLineComposeError::SourceMismatch)?;
    if !preferred
        .authority()?
        .same_payload(&audit.preferred.authority()?)
    {
        return Err(MarkdownLineComposeError::SourceMismatch);
    } // 相同文字不能替代原语义base身份；每个实际试次仍属于同一原样式/注册。
    if fonts.run.is_none()
        && !response.font.permits(&fonts.original.authority())
        && matches!(audit.origin, FontAuditOrigin::Frontier)
    {
        // 后续安全part的首GCB可恰好恢复base；前一part已真实消费，这里没有可输出的旧Font首段。
        // 先归还未发布的旧Job，再退役原parent及其整part continuation；同cursor重新ItemStart，
        // 不能输出空run、拒绝合法变化或伪造已经消费到该part末端。原Source/Script/Breaks照常保留。
        let at_head = fonts.probe.as_ref().expect("原审核仍持GCB").range.start == payload.cursor;
        drop(payload.item.take());
        if !at_head {
            // 变化落在中心之前的左支持内，不能把它伪称为当前cursor的新Font起点。
            fonts.phase = Phase::UncertifiedSupport;
            payload.request.take();
            payload.font_probe_scope.take();
            payload.stage = Stage::ClosedFonts;
            return Ok(());
        }
        drop(payload.frontier.take());
        payload.closed_fonts = None;
        payload.request.take();
        payload.font_probe_scope.take();
        payload.stage = Stage::ItemStart;
        return Ok(());
    }
    let range = fonts.probe.take().expect("原完整GCB试次仍在").range;
    match &mut fonts.run {
        None => {
            if !response.font.permits(&fonts.original.authority()) {
                return Err(MarkdownLineComposeError::SourceMismatch);
            } // 首GCB已由发起响应选择过；同Source/语义未变时不能悄悄改写原首Font事实。
            fonts.run = Some(Run {
                range,
                font: response.font,
                registration: response.registration,
            });
            fonts.following();
        }
        Some(run) if response.font.permits(&run.font.authority()) => {
            run.range.end = range.end;
            fonts.following();
        }
        Some(_) => {
            if fonts.run.as_ref().expect("原连续Font段仍在").range.start != payload.cursor {
                // 有上下文计划的左支持不能裸裁掉；缺少同计划重形中心票时明确保持未认证，
                // 不发布已知使用错误支持字体的旧Job，也不把预算末端签为新Font自然边界。
                drop(payload.item.take());
                fonts.phase = Phase::UncertifiedSupport;
                payload.request.take();
                payload.font_probe_scope.take();
                payload.stage = Stage::ClosedFonts;
                return Ok(());
            }
            fonts.emit = fonts.run.take();
            fonts.frontier_head = true;
            fonts.phase = Phase::Emit;
            // 原Job包含已发现字体选择不同的后文，先真实归还再建立自然首段，绝不同时保两张主字形图。
            // 后继Font必须在首段child归还后从真实b回ItemStart取得自己计划；旧part末端不给它闭项权。
            drop(payload.item.take());
            // 旧Frontier的child弱身份现在只反映旧Job已归还；continuation仍记录整part原末端。
            // 它绝不传给resume；后续AwaitChild只等本次自然首段再真正释放后整体退役原parent。
        }
    }
    payload.request.take();
    payload.font_probe_scope.take();
    payload.stage = Stage::ClosedFonts;
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
        Phase::ReuseOriginal => {
            let item = payload
                .item
                .as_mut()
                .expect("未变化审核仍持唯一原Ready Job");
            let audit = fonts.audit.as_ref().expect("复用只属于原字体审核");
            audit.preferred.lease()?;
            if item.facts.source_utf16 != fonts.facts.source_utf16
                || !fonts.original.permits(&item.font)
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            let count = item.job.profile()?.glyph_count;
            item.job.glyph(authority, &item.font, count)?;
            step.answer = MarkdownLineItemAnswer::ItemReady(item.facts.clone());
            payload.closed_fonts = None;
            payload.stage = Stage::Ready;
        } // Frontier原parent/child弱票及continuation未改动；自然项也复用原完整Job，不重复shape。
        Phase::UncertifiedSupport => {
            step.answer =
                MarkdownLineItemAnswer::NeedsFrontier(MarkdownLineFrontierReason::UncertifiedWord);
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
        // 这次同 Font 探测没有取得自然右界，不能发布预算截断段，也不能覆盖原计划更精确的拒绝。
        // 原 Frontier 从未交出 child；只销毁本次试字 owner，回到同一 Source/SCX/词域续体。
        payload
            .frontier
            .as_mut()
            .expect("首 Font 探测仍归原 Frontier")
            .decline_font_head();
        let retired = payload.closed_fonts.take();
        payload.stage = Stage::Frontier;
        drop(retired); // 归还本次 Font 段/试次 scope；旧字体请求不能延长或重启已拒探测。
        return Ok(step);
    }
    if probe.range.is_empty() {
        step.metadata_units += 1;
        match payload.breaks.grapheme_query(
            authority,
            probe.range.start,
            MarkdownGraphemeQuery::Next,
        )? {
            MarkdownGraphemeAnswer::Pending => return Ok(step),
            MarkdownGraphemeAnswer::Known(Some(end)) if end.utf16 > probe.range.start => {
                let limit = fonts
                    .audit
                    .as_ref()
                    .map_or(fonts.facts.source_utf16.end, |audit| audit.range.end);
                if end.utf16 > limit
                    && seam_at(&fonts.shaping_seams, &fonts.source, limit)?.is_none()
                {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
                probe.range.end = end.utf16.min(limit); // 只有原 UBA 自然端点可截交本地 shaping 段。
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
                probe.endpoints[index] = Some(CertifiedShapingEndpoint::Grapheme {
                    proof: endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                    seam: seam_at(&fonts.shaping_seams, &fonts.source, offset)?,
                });
            }
            MarkdownGraphemeAnswer::Known(None) => {
                probe.endpoints[index] = Some(CertifiedShapingEndpoint::Bidi(
                    seam_at(&fonts.shaping_seams, &fonts.source, offset)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                ));
            }
            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
        }
    }
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        range: probe.range.clone(),
        scope: StyleScope::Text(TextStyleScope {
            paragraph: fonts.facts.paragraph_utf16.clone(),
            script: fonts.facts.script,
            level: fonts.facts.level,
        }),
        shaping_seams: [
            seam_at(&fonts.shaping_seams, &fonts.source, probe.range.start)?,
            seam_at(&fonts.shaping_seams, &fonts.source, probe.range.end)?,
        ],
        continuation_font: Some(fonts.original.authority()),
        continuation_bytes: Some(
            probe.endpoints[0]
                .as_ref()
                .expect("原闭项 GCB 起点已认证")
                .boundary(authority)?
                .byte
                ..probe.endpoints[1]
                    .as_ref()
                    .expect("原闭项 GCB 终点已认证")
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
                fonts.endpoints[index] = Some(CertifiedShapingEndpoint::Grapheme {
                    proof: endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                    seam: fonts.seam_at(offset)?,
                });
            }
            MarkdownGraphemeAnswer::Known(None) => {
                fonts.endpoints[index] = Some(CertifiedShapingEndpoint::Bidi(
                    fonts
                        .seam_at(offset)?
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                ));
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
    endpoints[0].require_pair(&endpoints[1], authority)?;
    // 普通闭项仍受原256 UTF16上限约束。已封存Frontier窗口另有原256 scalar/1024 byte
    // 证明，非BMP不能被误按UTF16单位扩成两倍工作；此用途只构造过真实part，并仍限制在其内。
    // 这里只为已实测字体变化之前的旧Font首段保留该原预算，不把安全窗口末端授给后继新Font。
    if fonts.source != *authority
        || first.utf16 != payload.cursor
        || first.utf16 != run.range.start
        || last.utf16 != run.range.end
        || first.utf16 < fonts.facts.source_utf16.start
        || last.utf16
            > fonts
                .audit
                .as_ref()
                .map_or(fonts.facts.source_utf16.end, |audit| audit.range.end)
        || first.utf16 >= last.utf16
        || (last.utf16 - first.utf16 > MAX_CLOSED_UTF16
            && !fonts
                .audit
                .as_ref()
                .is_some_and(|audit| matches!(audit.origin, FontAuditOrigin::Frontier)))
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
    let shaping_seams = fonts.seams_for(&run.range)?;
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
        shaping_seams,
    });
    payload.stage = Stage::ClosedInput;
    Ok(step) // 这里只交权；原邻接context和中心正文在后继独立bytes/scalars量子实际读取。
}

#[cfg(test)]
mod initial_tests;
