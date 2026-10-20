//! 真实活动计划的局部依赖证书。它不改查表、位置计算或unsafe标记算法。
//!
//! 这里只认证一标量一glyph的printable ASCII域：每个可达替换保持1→1，context的
//! back谓词对自身输出保持不变，PairPos没有second记录。因此有限支持域能覆盖真实
//! 查表的读域及unsafe标记写域；Core仍须另证完整源item、GCB和实际字体/选项身份。

mod positioned;
mod read;
use super::{closure::GlyphSet, AsciiFrontierRejection as Rejection};
use crate::{
    ot::{layout::LayoutTableKind, lookup::SubtableKind},
    Direction, Font, ShapePlan, ShaperFont, Tag,
};
pub use positioned::{AsciiPositionedAdvance, AsciiPositionedAdvanceJob};
use read::Read;

/// 完整记录游标结束后的元数据事实；本值单独不授予任何源范围权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AsciiLocalFrontierCapability {
    /// 两侧各自需要的真实标量支持长度，包含unsafe标记可能回写的域。
    pub support_scalars: usize,
    /// 已检查的保守可达glyph数；不以cmap覆盖冒充实际整形无缺字。
    pub reachable_glyphs: usize,
    /// cmap、coverage、rule/component、位置header与属性检查的实际记录数。
    pub checked_records: usize,
}

/// 只能由完整真实闭包作业消费产生；公开计数或任意glyph编号不能构造该域。
pub struct AsciiLocalFrontierCertificate {
    capability: AsciiLocalFrontierCapability,
    glyphs: GlyphSet,
}
impl AsciiLocalFrontierCertificate {
    /// 只读固定事实，不移出或重建其私有可达域。
    pub fn capability(&self) -> AsciiLocalFrontierCapability {
        self.capability
    }
}

#[derive(Clone, Copy)]
struct LigatureCursor {
    table: usize,
    component: usize,
    possible: bool,
}

#[derive(Clone, Copy)]
/// 原context3单ahead谓词对完整可达域的不交查询；逐glyph收费，未完成不能跳过真实替换规则。
struct ContextCoverage {
    glyph: u16,
    coverage: usize,
    probe: u32,
}

/// 固定两个8KiB glyph编号集合和标量游标；不存在按正文长度增长的工作表。
pub struct AsciiLocalFrontierJob {
    current: GlyphSet,
    next: GlyphSet,
    scalar: u32,
    scalar_end: u32,
    table: usize,
    stage: usize,
    lookup: usize,
    subtable: usize,
    glyph: u32,
    rule: usize,
    started: bool,
    header: bool,
    span: usize,
    support: usize,
    ligature: Option<LigatureCursor>,
    context_coverage: Option<ContextCoverage>,
    records: usize,
    complete: Option<AsciiLocalFrontierCapability>,
    failed: bool,
}

impl ShapePlan {
    /// 创建尚未认证的逐记录作业。构造只检查固定上界的活动stage/lookup描述。
    ///
    /// # Errors
    /// AAT重排、传统kern、可变回调、fraction显式启用、反向或超出固定计划上界均拒绝。
    /// GPOS/trak不是无条件拒绝：随后作业必须证明真实活动类型的有限依赖。
    pub fn ascii_local_frontier_job(
        &self,
        font: &Font,
    ) -> Result<AsciiLocalFrontierJob, Rejection> {
        self.ascii_local_frontier_job_for_range(font, 0x20..=0x7e)
    }

    /// 对调用方已完整证明的保守printable ASCII域检查同一个真实计划；不从当前前缀猜未读尾部。
    ///
    /// # Errors
    /// 空/越界域与原独立作业相同的计划限制均拒绝；本构造不授予任何Source范围权限。
    pub fn ascii_local_frontier_job_for_range(
        &self,
        font: &Font,
        scalars: core::ops::RangeInclusive<u8>,
    ) -> Result<AsciiLocalFrontierJob, Rejection> {
        let (first, last) = (*scalars.start(), *scalars.end());
        if first < 0x20 || last > 0x7e || first > last {
            return Err(Rejection::LookupBehavior);
        } // 源域证明由Core同owner负责；Font侧仍独立拒绝不属于既有ASCII能力域的数字输入。
        if self.direction != Direction::LeftToRight {
            return Err(Rejection::Direction);
        }
        if self.shaper.preprocess_text.is_some()
            || self.shaper.postprocess_glyphs.is_some()
            || self.shaper.setup_masks.is_some()
            || self.shaper.reorder_marks.is_some()
            || self.shaper.decompose.is_some()
            || self.shaper.compose.is_some()
        {
            return Err(Rejection::Shaper);
        }
        if self.apply_morx || self.apply_kerx || self.apply_kern || self.has_vert {
            return Err(Rejection::Positioning);
        }
        // 默认fraction的自动掩码仅由U+2044触发；完整ASCII源证书排除该字符。用户显式
        // 非零feature可能直接设掩码，这里不能借“没有斜线”省掉它的活动查表。
        if self.user_features.iter().any(|f| {
            f.value != 0
                && [Tag::new(b"frac"), Tag::new(b"numr"), Tag::new(b"dnom")].contains(&f.tag)
        }) {
            return Err(Rejection::LookupBehavior);
        }
        let shaper = ShaperFont::new(font);
        let layout = shaper.layout();
        let mut lookups = 0;
        let mut subtables = 0;
        for kind in [LayoutTableKind::Gsub, LayoutTableKind::Gpos] {
            for (stage, entry) in self.ot_map.stages(kind).iter().enumerate() {
                if entry.pause_func.is_some() {
                    return Err(Rejection::StageCallback);
                }
                for lookup in self.ot_map.stage_lookups(kind, stage) {
                    lookups += 1;
                    if lookups > 256 || lookup.random || lookup.per_syllable {
                        return Err(Rejection::MetadataBudget);
                    }
                    let (_, actual) = layout
                        .ot
                        .table_data_and_lookup(kind, lookup.index)
                        .ok_or(Rejection::LookupUnavailable)?;
                    subtables += actual.subtables.len();
                    if subtables > 1024 || actual.is_reversed {
                        return Err(Rejection::MetadataBudget);
                    }
                }
            }
        }
        Ok(AsciiLocalFrontierJob {
            current: GlyphSet([0; 1024]),
            next: GlyphSet([0; 1024]),
            scalar: u32::from(first),
            scalar_end: u32::from(last),
            table: 0,
            stage: 0,
            lookup: 0,
            subtable: 0,
            glyph: 0,
            rule: 0,
            started: false,
            header: false,
            span: 0,
            support: 0,
            ligature: None,
            context_coverage: None,
            records: 0,
            complete: None,
            failed: false,
        })
    }
}

impl AsciiLocalFrontierJob {
    /// 消费真实完整游标，保留一个固定glyph集合供后继定位证明使用。
    ///
    /// # Errors
    /// 未完成或失败不能从公开capability计数补签一个可达域。
    pub fn into_certificate(self) -> Result<AsciiLocalFrontierCertificate, Rejection> {
        if self.failed {
            return Err(Rejection::LookupUnavailable);
        }
        Ok(AsciiLocalFrontierCertificate {
            capability: self.complete.ok_or(Rejection::LookupUnavailable)?,
            glyphs: self.current,
        })
    }
    /// 包括失败前已消费的真实记录；失败不伪装为零成本。
    pub fn checked_records(&self) -> usize {
        self.records
    }

    /// 最多消费budget条有限记录，quota=1也能暂停；未结束不会交付能力。
    ///
    /// # Errors
    /// 格式、可达多输入/删除/缺字、忽略链、context自反馈或固定元数据总上界不满足，
    /// 作业永久拒绝。调用方保持原Font和ShapePlan owner，不得更换同名字体续用游标。
    pub fn step(
        &mut self,
        plan: &ShapePlan,
        font: &Font,
        budget: usize,
    ) -> Result<Option<AsciiLocalFrontierCapability>, Rejection> {
        if self.failed {
            return Err(Rejection::LookupUnavailable);
        }
        let result = self.advance(plan, font, budget);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn charge(&mut self) -> Result<(), Rejection> {
        self.records = self
            .records
            .checked_add(1)
            .ok_or(Rejection::MetadataBudget)?;
        if self.records > 262_144 {
            return Err(Rejection::MetadataBudget);
        }
        Ok(())
    }
    fn output(&mut self, font: &Font, glyph: u16) -> Result<(), Rejection> {
        if glyph == 0 || u32::from(glyph) >= font.num_glyphs() {
            return Err(Rejection::MissingGlyph);
        }
        // 输出不能成为会被以后/本lookup跳过的mark或ligature；否则一个逻辑邻居可能
        // 需要穿过任意长链。本窄证书保留base/unknown，其余继续完整原算法。
        let shaper = ShaperFont::new(font);
        if shaper.layout().ot.glyph_props(glyph.into()) & 0x0c != 0 {
            return Err(Rejection::LookupBehavior);
        }
        self.next.insert(glyph);
        Ok(())
    }
    fn advance(
        &mut self,
        plan: &ShapePlan,
        font: &Font,
        budget: usize,
    ) -> Result<Option<AsciiLocalFrontierCapability>, Rejection> {
        if budget == 0 {
            return Err(Rejection::MetadataBudget);
        }
        if self.complete.is_some() {
            return Ok(self.complete);
        }
        let begin = self.records;
        let shaper = ShaperFont::new(font);
        let layout = shaper.layout();
        while self.records - begin < budget {
            if self.scalar <= self.scalar_end {
                self.charge()?;
                let glyph = shaper
                    .default_nominal_glyph(self.scalar)
                    .ok_or(Rejection::MissingGlyph)?
                    .to_u32();
                if glyph == 0 || glyph >= font.num_glyphs() || glyph > u32::from(u16::MAX) {
                    return Err(Rejection::MissingGlyph);
                }
                if layout.ot.glyph_props((glyph as u16).into()) & 0x0c != 0 {
                    return Err(Rejection::LookupBehavior);
                }
                self.current.insert(glyph as u16);
                self.scalar += 1;
                continue;
            }
            if self.table == 2 {
                self.charge()?; // tracking保持原每grapheme算法，仅核实际table存在，不改其插值/舍入。
                if plan.apply_trak && layout.aat.trak.is_none() {
                    return Err(Rejection::LookupUnavailable);
                }
                let result = AsciiLocalFrontierCapability {
                    support_scalars: self.support,
                    reachable_glyphs: self.current.0.iter().map(|v| v.count_ones() as usize).sum(),
                    checked_records: self.records,
                };
                self.complete = Some(result);
                return Ok(Some(result));
            }
            let kind = if self.table == 0 {
                LayoutTableKind::Gsub
            } else {
                LayoutTableKind::Gpos
            };
            if self.stage == plan.ot_map.stages(kind).len() {
                self.table += 1;
                self.stage = 0;
                self.lookup = 0;
                continue;
            }
            let lookups = plan.ot_map.stage_lookups(kind, self.stage);
            let Some(lookup) = lookups.get(self.lookup) else {
                self.stage += 1;
                self.lookup = 0;
                continue;
            };
            if !self.started {
                self.charge()?;
                // 仅在完整printable ASCII源条件下，自动fraction没有U+2044触发者。
                // root与其他feature共用mask时仍会检查，不能按lookup编号硬跳。
                let fractional = plan.frac_mask | plan.numr_mask | plan.dnom_mask;
                if kind == LayoutTableKind::Gsub
                    && lookup.mask & !fractional == 0
                    && lookup.mask & plan.ot_map.get_global_mask() == 0
                {
                    self.lookup += 1;
                    continue;
                }
                self.next = self.current.clone();
                self.started = true;
                self.span = 0;
                continue;
            }
            let (bytes, actual) = layout
                .ot
                .table_data_and_lookup(kind, lookup.index)
                .ok_or(Rejection::LookupUnavailable)?;
            let Some(sub) = actual.subtables.get(self.subtable) else {
                // 每lookup读写域叠加到既有依赖；两倍span覆盖中心由左右邻居触发的
                // unsafe标记写入。自身back谓词不变已逐实际替换核验，故无同lookup递归。
                self.support = self
                    .support
                    .checked_add(self.span.saturating_mul(2))
                    .ok_or(Rejection::MetadataBudget)?;
                if self.support > 64 {
                    return Err(Rejection::MetadataBudget);
                }
                self.current = self.next.clone();
                self.lookup += 1;
                self.subtable = 0;
                self.glyph = 0;
                self.started = false;
                self.header = false;
                continue;
            };
            let mark_noop = kind == LayoutTableKind::Gpos
                && matches!(
                    sub.kind,
                    SubtableKind::MarkBasePos1
                        | SubtableKind::MarkLigPos1
                        | SubtableKind::MarkMarkPos1
                );
            if actual.props & !8 != 0 && !mark_noop {
                return Err(Rejection::LookupBehavior);
            } // 只有firstCoverage逐glyph证实不交的mark候选可不消费过滤标志；任何命中仍拒绝。
            let read = Read(bytes);
            let base = sub.offset as usize;
            if !self.header {
                self.charge()?;
                self.header = true;
                if kind == LayoutTableKind::Gpos {
                    if mark_noop {
                        continue; // 已知格式的firstCoverage在后续原逐glyph量子验证，不把格式本身当no-op。
                    }
                    if !matches!(sub.kind, SubtableKind::PairPos1 | SubtableKind::PairPos2)
                        || read.u16(base + 6)? != 0
                    {
                        return Err(Rejection::Positioning);
                    }
                    // value2为空才不会在成功pair后跳过second，消除跨任意pair链的扫描相位。
                    // 真实value1/device/variation仍交原算法，证书不重复计算位置。
                    self.span = self.span.max(1);
                } else if !matches!(
                    sub.kind,
                    SubtableKind::SingleSubst1
                        | SubtableKind::SingleSubst2
                        | SubtableKind::MultipleSubst1
                        | SubtableKind::AlternateSubst1
                        | SubtableKind::LigatureSubst1
                        | SubtableKind::ChainedContextFormat2
                        | SubtableKind::ChainedContextFormat3
                ) {
                    // 未支持格式不能假设offset2就是coverage；先核真实kind，避免从
                    // 偶然空的错误offset查询推导“输入不可达”并错误签发依赖证书。
                    return Err(Rejection::CrossInputSubtable);
                }
                continue;
            }
            if let Some(mut context) = self.context_coverage.take() {
                if let Some(probe) = self.current.next(context.probe) {
                    self.charge()?;
                    context.probe = u32::from(probe) + 1;
                    if read.coverage(context.coverage, probe)?.is_some() {
                        // 这个必要谓词真实可达时仍走原child/feedback限制，不把可能匹配当成安全替换。
                        self.context3(
                            plan,
                            font,
                            &read,
                            base,
                            context.glyph,
                            actual.subtables.len(),
                        )?;
                        self.glyph = u32::from(context.glyph) + 1;
                        self.rule = 0;
                    } else {
                        self.context_coverage = Some(context);
                    }
                    continue;
                }
                self.glyph = u32::from(context.glyph) + 1;
                self.rule = 0;
                continue; // 完整闭包确实不交：替换无效，但下面装配时记录的真实context unsafe写域仍保留。
            }
            if let Some(mut lig) = self.ligature {
                self.charge()?;
                let count = usize::from(read.u16(lig.table + 2)?);
                if lig.component < count {
                    let component = read.u16(lig.table + 2 + lig.component * 2)?;
                    lig.possible &= self.current.contains(component);
                    lig.component += 1;
                    self.ligature = Some(lig);
                    continue;
                }
                if lig.possible {
                    return Err(Rejection::CrossInputSubtable);
                }
                self.ligature = None;
                self.rule += 1;
                continue; // 即使不匹配也保留上面的unsafe写域。
            }
            let Some(glyph) = self.current.next(self.glyph) else {
                self.subtable += 1;
                self.glyph = 0;
                self.rule = 0;
                self.header = false;
                continue;
            };
            self.charge()?;
            if kind == LayoutTableKind::Gpos {
                if mark_noop
                    && read
                        .coverage(read.offset(base, base + 2)?, glyph)?
                        .is_some()
                {
                    return Err(Rejection::Positioning);
                } // 真实mark firstCoverage命中时仍无局部能力；不按GDEF类别或正文语言直接跳过。
                  // 每个实际可达glyph都核GDEF；不把“源不是mark”替代替换后glyph属性。
                if layout.ot.glyph_props(glyph.into()) & 0x0c != 0 {
                    return Err(Rejection::LookupBehavior);
                }
                self.glyph = u32::from(glyph) + 1;
                continue;
            }
            let first = if sub.kind == SubtableKind::ChainedContextFormat3 {
                let back = usize::from(read.u16(base + 2)?);
                if back > 1 {
                    return Err(Rejection::CrossInputSubtable);
                }
                let input = base + 4 + 2 * back;
                if read.u16(input)? != 1 {
                    return Err(Rejection::CrossInputSubtable);
                }
                read.offset(base, input + 2)?
            } else {
                read.offset(base, base + 2)?
            };
            let Some(index) = read.coverage(first, glyph)? else {
                self.glyph = u32::from(glyph) + 1;
                self.rule = 0;
                continue;
            };
            match sub.kind {
                SubtableKind::SingleSubst1 | SubtableKind::SingleSubst2 => {
                    self.output(
                        font,
                        read.single(base, glyph)?
                            .ok_or(Rejection::LookupUnavailable)?,
                    )?;
                }
                SubtableKind::MultipleSubst1 | SubtableKind::AlternateSubst1 => {
                    let sequence = read.offset(base, base + 6 + index * 2)?;
                    if read.u16(sequence)? != 1 {
                        return Err(Rejection::CrossInputSubtable);
                    }
                    self.output(font, read.u16(sequence + 2)?)?;
                }
                SubtableKind::LigatureSubst1 => {
                    let set = read.offset(base, base + 6 + index * 2)?;
                    if self.rule < usize::from(read.u16(set)?) {
                        let table = read.offset(set, set + 2 + self.rule * 2)?;
                        let count = usize::from(read.u16(table + 2)?);
                        if count == 0 || count > 16 {
                            return Err(Rejection::CrossInputSubtable);
                        }
                        self.span = self.span.max(count.saturating_sub(1));
                        self.ligature = Some(LigatureCursor {
                            table,
                            component: 1,
                            possible: true,
                        });
                        continue;
                    }
                }
                SubtableKind::ChainedContextFormat2 => {
                    let class = usize::from(read.class(read.offset(base, base + 6)?, glyph)?);
                    let count = usize::from(read.u16(base + 10)?);
                    if class < count {
                        let offset = read.u16(base + 12 + class * 2)?;
                        if offset != 0 {
                            let set = base + usize::from(offset);
                            if self.rule < usize::from(read.u16(set)?) {
                                let rule = read.offset(set, set + 2 + self.rule * 2)?;
                                self.context2(
                                    plan,
                                    font,
                                    &read,
                                    base,
                                    rule,
                                    glyph,
                                    actual.subtables.len(),
                                )?;
                                self.rule += 1;
                                continue;
                            }
                        }
                    }
                }
                SubtableKind::ChainedContextFormat3 => {
                    let back = usize::from(read.u16(base + 2)?);
                    let input = base + 4 + 2 * back;
                    let ahead = usize::from(read.u16(input + 4)?);
                    if ahead > 1 {
                        return Err(Rejection::CrossInputSubtable);
                    }
                    self.span = self.span.max(back + ahead);
                    let coverage = if ahead != 0 {
                        Some(read.offset(base, input + 6)?)
                    } else {
                        None // back可能已受同lookup前项替换影响，继续原反馈校验，不借prelookup域跳过。
                    };
                    if let Some(coverage) = coverage {
                        self.context_coverage = Some(ContextCoverage {
                            glyph,
                            coverage,
                            probe: 0,
                        });
                        continue; // 原单一谓词游标在下一记录预算继续，不在一个quota1里暗扫完整glyph集合。
                    }
                    self.context3(plan, font, &read, base, glyph, actual.subtables.len())?;
                }
                _ => return Err(Rejection::CrossInputSubtable),
            }
            self.glyph = u32::from(glyph) + 1;
            self.rule = 0;
        }
        Ok(None)
    }
    fn child_single(
        &self,
        plan: &ShapePlan,
        font: &Font,
        read: &Read<'_>,
        records: usize,
        count: u16,
        glyph: u16,
    ) -> Result<u16, Rejection> {
        if count == 0 {
            return Ok(glyph);
        }
        if count != 1 || read.u16(records)? != 0 {
            return Err(Rejection::CrossInputSubtable);
        }
        let index = read.u16(records + 2)?;
        let shaper = ShaperFont::new(font);
        let (bytes, child) = shaper
            .layout()
            .ot
            .table_data_and_lookup(LayoutTableKind::Gsub, index)
            .ok_or(Rejection::LookupUnavailable)?;
        if child.subtables.len() != 1
            || child.props != 0
            || child.is_reversed
            || !child.is_subst
            || !matches!(
                child.subtables[0].kind,
                SubtableKind::SingleSubst1 | SubtableKind::SingleSubst2
            )
        {
            return Err(Rejection::CrossInputSubtable);
        }
        let _ = plan; // 实际嵌套lookup仅由当前已认证root的原record引用，不从外部索引获得。
        Ok(Read(bytes)
            .single(child.subtables[0].offset as usize, glyph)?
            .unwrap_or(glyph))
    }
    fn context2(
        &mut self,
        plan: &ShapePlan,
        font: &Font,
        read: &Read<'_>,
        base: usize,
        rule: usize,
        glyph: u16,
        siblings: usize,
    ) -> Result<(), Rejection> {
        let back = usize::from(read.u16(rule)?);
        if back > 1 {
            return Err(Rejection::CrossInputSubtable);
        }
        let input = rule + 2 + 2 * back;
        if read.u16(input)? != 1 {
            return Err(Rejection::CrossInputSubtable);
        }
        let ahead = usize::from(read.u16(input + 2)?);
        if ahead > 1 {
            return Err(Rejection::CrossInputSubtable);
        }
        let count_at = input + 4 + 2 * ahead;
        let output =
            self.child_single(plan, font, read, count_at + 2, read.u16(count_at)?, glyph)?;
        if output != glyph
            && (siblings != 1
                || read.class(read.offset(base, base + 4)?, glyph)?
                    != read.class(read.offset(base, base + 4)?, output)?)
        {
            return Err(Rejection::LookupBehavior);
        }
        // format2同table的其他规则也可读取back类；即使本条backCount=0，输出仍须
        // 保持这个共享ClassDef不变，防止后一位置沿另条规则形成无界自反馈。
        self.span = self.span.max(back + ahead);
        self.output(font, output)
    }
    fn context3(
        &mut self,
        plan: &ShapePlan,
        font: &Font,
        read: &Read<'_>,
        base: usize,
        glyph: u16,
        siblings: usize,
    ) -> Result<(), Rejection> {
        let back = usize::from(read.u16(base + 2)?);
        let input = base + 4 + 2 * back;
        if back > 1 || read.u16(input)? != 1 {
            return Err(Rejection::CrossInputSubtable);
        }
        let ahead = usize::from(read.u16(input + 4)?);
        if ahead > 1 {
            return Err(Rejection::CrossInputSubtable);
        }
        let count_at = input + 6 + 2 * ahead;
        let output =
            self.child_single(plan, font, read, count_at + 2, read.u16(count_at)?, glyph)?;
        if output != glyph
            && (siblings != 1
                || (back != 0 && {
                    let coverage = read.offset(base, base + 4)?;
                    read.coverage(coverage, glyph)?.is_some()
                        != read.coverage(coverage, output)?.is_some()
                }))
        {
            return Err(Rejection::LookupBehavior);
        }
        self.span = self.span.max(back + ahead);
        self.output(font, output)
    }
}
