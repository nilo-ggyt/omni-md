//! 对真实可达域取保守positioned advance下界；不修改实际字形或关闭kern/trak。
//!
//! 原PairPos每lookup至多应用一个value1，且value2已证为空。整数与variation是原顺序
//! 的两次saturating_add；每个实际record对同glyph下界求值后取最小，单调性保证只会低估。
//! trak在其后按原字号和每ASCII字素执行。下界为负只拒证，不能夹成0。
mod tracking;
mod variation;
use super::*;
use crate::GlyphId;
use tracking::Tracking;
use variation::Variation;

const MAX_REACHABLE: usize = 256;
#[derive(Clone, Copy)]
enum Target {
    Glyph(usize),
    Class(u16),
}
struct ApplyRecord {
    target: Target,
    integer: i32,
    delta: i32,
    cursor: usize,
}

/// 完整positioned下界事实；raw值仍在原Scale单位，Core只在最终出口做原量化。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AsciiPositionedAdvance {
    /// 包括全部可达glyph、GPOS与tracking后的保守原始下界。
    pub minimum_raw: i32,
    /// 实际访问的glyph/PairValue/region/axis/trak记录总量。
    pub checked_records: usize,
}

/// 固定glyph/class编号域与一个variation/trak游标，不存按正文增长的glyph或pair数组。
pub struct AsciiPositionedAdvanceJob {
    glyphs: GlyphSet,
    classes: [GlyphSet; 2],
    points: Option<f32>,
    mode: usize,
    stage: usize,
    lookup: usize,
    subtable: usize,
    first: u32,
    second: u32,
    started: bool,
    header: bool,
    mapped: bool,
    pair_set: Option<(usize, usize, usize)>,
    value: Option<Variation>,
    apply: Option<ApplyRecord>,
    glyph_ids: [u16; MAX_REACHABLE],
    advances: [i32; MAX_REACHABLE],
    next_advances: [i32; MAX_REACHABLE],
    class_ids: [u16; MAX_REACHABLE],
    class_heads: [u16; MAX_REACHABLE],
    class_next: [u16; MAX_REACHABLE],
    class_count: usize,
    count: usize,
    set_glyph: usize,
    fold: usize,
    tracking_value: i32,
    minimum: i32,
    tracking: Tracking,
    records: usize,
    glyph_queries: usize,
    complete: Option<AsciiPositionedAdvance>,
    failed: bool,
}
impl AsciiLocalFrontierCertificate {
    /// 仅完整局部闭包可开始定位下界作业；仍须沿同Font/实际计划/scale/字号消费。
    pub fn positioned_advance_job(&self, points: Option<f32>) -> AsciiPositionedAdvanceJob {
        AsciiPositionedAdvanceJob {
            glyphs: self.glyphs.clone(),
            classes: [GlyphSet([0; 1024]), GlyphSet([0; 1024])],
            points,
            mode: 0,
            stage: 0,
            lookup: 0,
            subtable: 0,
            first: 0,
            second: 0,
            started: false,
            header: false,
            mapped: false,
            pair_set: None,
            value: None,
            apply: None,
            glyph_ids: [0; MAX_REACHABLE],
            advances: [0; MAX_REACHABLE],
            next_advances: [0; MAX_REACHABLE],
            class_ids: [0; MAX_REACHABLE],
            class_heads: [u16::MAX; MAX_REACHABLE],
            class_next: [u16::MAX; MAX_REACHABLE],
            class_count: 0,
            count: 0,
            set_glyph: 0,
            fold: 0,
            tracking_value: 0,
            minimum: i32::MAX,
            tracking: Tracking::new(),
            records: 0,
            glyph_queries: 0,
            complete: None,
            failed: false,
        }
    }
}
impl AsciiPositionedAdvanceJob {
    /// 错误也保留此前真实工作量，不把失败报告成0。
    pub fn checked_records(&self) -> usize {
        self.records
    }
    /// 与PairValue/region等元数据分开报告的实际glyph advance查询数。
    pub fn queried_glyphs(&self) -> usize {
        self.glyph_queries
    }
    /// 每次最多budget条真实记录；region内的每个axis与trak每个size/track都可暂停。
    ///
    /// # Errors
    /// 零预算、坏表、超固定总预算或不满足原局部证书类型时永久拒绝。调用方不可换原Font、
    /// 计划、scale或points重用；Core以原弱身份/完整options及同取消位约束该义务。
    pub fn step(
        &mut self,
        plan: &ShapePlan,
        font: &ShaperFont<'_, '_>,
        budget: usize,
    ) -> Result<Option<AsciiPositionedAdvance>, Rejection> {
        if self.failed {
            return Err(Rejection::LookupUnavailable);
        }
        let result = self.advance(plan, font, budget);
        if result.is_err() {
            self.failed = true
        }
        result
    }
    fn charge(&mut self) -> Result<(), Rejection> {
        self.records = self
            .records
            .checked_add(1)
            .ok_or(Rejection::MetadataBudget)?;
        if self.records > 1_048_576 {
            return Err(Rejection::MetadataBudget);
        }
        Ok(())
    }
    fn next_subtable(&mut self) {
        self.subtable += 1;
        self.header = false;
        self.first = 0;
        self.second = 0;
        self.mapped = false;
        self.pair_set = None;
        self.classes = [GlyphSet([0; 1024]), GlyphSet([0; 1024])];
    }
    fn value_record(
        &mut self,
        read: &Read<'_>,
        data_base: usize,
        offset: usize,
        format: u16,
        scale: crate::Scale,
        has_variations: bool,
        target: Target,
    ) -> Result<(), Rejection> {
        if format & !0xff != 0 {
            return Err(Rejection::Positioning);
        }
        let integer = if format & 4 != 0 {
            let at = offset + (format & 3).count_ones() as usize * 2;
            let raw = i32::from(read.u16(at)? as i16);
            scale.scale_x(raw)
        } else {
            0
        };
        let cursor = match target {
            Target::Glyph(index) => index,
            Target::Class(class) => {
                let slot = self.class_ids[..self.class_count]
                    .binary_search(&class)
                    .map_err(|_| Rejection::LookupUnavailable)?;
                usize::from(self.class_heads[slot])
            }
        };
        self.apply = Some(ApplyRecord {
            target,
            integer,
            delta: 0,
            cursor,
        });
        if format & 0x40 != 0 && has_variations {
            let at = offset
                + (format & 0x0f).count_ones() as usize * 2
                + (format & 0x30).count_ones() as usize * 2;
            let relative = usize::from(read.u16(at)?);
            if relative != 0 {
                let record = data_base
                    .checked_add(relative)
                    .ok_or(Rejection::LookupUnavailable)?;
                if read.u16(record + 4)? == 0x8000 {
                    self.value = Some(Variation::new(read.u16(record)?, read.u16(record + 2)?));
                }
            }
        } // 普通device不是原value::apply支持的VarIdx；保持其真实0，不另造ppem行为。
        Ok(())
    }
    fn class_member(&mut self, class: u16, index: usize) -> Result<(), Rejection> {
        let slot = match self.class_ids[..self.class_count].binary_search(&class) {
            Ok(slot) => slot,
            Err(slot) => {
                if self.class_count == MAX_REACHABLE {
                    return Err(Rejection::MetadataBudget);
                }
                self.class_ids.copy_within(slot..self.class_count, slot + 1);
                self.class_heads
                    .copy_within(slot..self.class_count, slot + 1);
                self.class_ids[slot] = class;
                self.class_heads[slot] = u16::MAX;
                self.class_count += 1;
                slot
            }
        };
        // 这个固定≤256项小目录只索引已认证glyph；链表让每PairValue只更新实际class成员，
        // 不在每个record内重扫完整域，也不创建按正文增长的Vec或font强owner。
        self.class_next[index] = self.class_heads[slot];
        self.class_heads[slot] = index as u16;
        Ok(())
    }
    fn advance(
        &mut self,
        plan: &ShapePlan,
        shaper: &ShaperFont<'_, '_>,
        budget: usize,
    ) -> Result<Option<AsciiPositionedAdvance>, Rejection> {
        if budget == 0 {
            return Err(Rejection::MetadataBudget);
        }
        if self.complete.is_some() {
            return Ok(self.complete);
        }
        let before = self.records;
        // 调用方交付同一实际执行Font与尺度；平台不可变metrics也必须进入这一原始下界，
        // 不能内部重造默认Font后对hmtx证明，却在实际shape中使用不同的水平推进。
        let layout = shaper.layout();
        let scale = shaper.scale;
        while self.records - before < budget {
            if self.mode == 0 {
                if let Some(glyph) = self.glyphs.next(self.first) {
                    self.charge()?;
                    self.glyph_queries += 1;
                    if self.count == MAX_REACHABLE {
                        return Err(Rejection::MetadataBudget);
                    }
                    self.glyph_ids[self.count] = glyph;
                    self.advances[self.count] = shaper.glyph_h_advance(GlyphId::from(glyph));
                    self.count += 1;
                    self.first = u32::from(glyph) + 1;
                    continue;
                }
                if self.count == 0 {
                    return Err(Rejection::MissingGlyph);
                }
                self.mode = if plan.apply_gpos { 1 } else { 2 };
                self.first = 0;
                continue;
            }
            if self.mode == 2 {
                self.charge()?;
                let tracking = if plan.apply_trak {
                    self.tracking.step(layout, scale, self.points)?
                } else {
                    Some(0)
                };
                if let Some(value) = tracking {
                    self.tracking_value = value;
                    self.mode = 3;
                }
                continue;
            }
            if self.mode == 3 {
                if self.fold < self.count {
                    self.charge()?;
                    self.minimum = self
                        .minimum
                        .min(self.advances[self.fold].saturating_add(self.tracking_value));
                    self.fold += 1;
                } else {
                    let facts = AsciiPositionedAdvance {
                        minimum_raw: self.minimum,
                        checked_records: self.records,
                    };
                    self.complete = Some(facts);
                    return Ok(Some(facts));
                }
                continue;
            }
            if let Some(value) = &mut self.value {
                self.records = self
                    .records
                    .checked_add(1)
                    .ok_or(Rejection::MetadataBudget)?;
                if self.records > 1_048_576 {
                    return Err(Rejection::MetadataBudget);
                }
                if let Some(delta) = value.step(layout)? {
                    self.apply
                        .as_mut()
                        .ok_or(Rejection::LookupUnavailable)?
                        .delta = scale.scale_x_f(delta);
                    self.value = None;
                }
                continue;
            }
            if let Some(record) = &mut self.apply {
                if record.cursor == usize::from(u16::MAX) {
                    self.apply = None;
                    continue;
                }
                let index = record.cursor;
                // 一个已认证glyph的下界cell就是本量子记录；不在quota1内暗扫全部域。
                self.records = self
                    .records
                    .checked_add(1)
                    .ok_or(Rejection::MetadataBudget)?;
                if self.records > 1_048_576 {
                    return Err(Rejection::MetadataBudget);
                }
                let candidate = self.advances[index]
                    .saturating_add(record.integer)
                    .saturating_add(record.delta);
                self.next_advances[index] = self.next_advances[index].min(candidate);
                record.cursor = match record.target {
                    Target::Glyph(_) => usize::from(u16::MAX),
                    Target::Class(_) => usize::from(self.class_next[index]),
                };
                continue;
            }
            if self.stage == plan.ot_map.stages(LayoutTableKind::Gpos).len() {
                self.mode = 2;
                continue;
            }
            let lookups = plan.ot_map.stage_lookups(LayoutTableKind::Gpos, self.stage);
            let Some(lookup) = lookups.get(self.lookup) else {
                self.stage += 1;
                self.lookup = 0;
                continue;
            };
            if !self.started {
                self.charge()?;
                self.next_advances = self.advances; // 保留本lookup不应用的真实路径；不是把调整量夹成0。
                self.started = true;
                continue;
            }
            let (bytes, actual) = layout
                .ot
                .table_data_and_lookup(LayoutTableKind::Gpos, lookup.index)
                .ok_or(Rejection::LookupUnavailable)?;
            let Some(sub) = actual.subtables.get(self.subtable) else {
                self.advances = self.next_advances;
                self.lookup += 1;
                self.started = false;
                self.subtable = 0;
                self.header = false;
                self.first = 0;
                self.second = 0;
                continue;
            };
            let read = Read(bytes);
            let base = sub.offset as usize;
            let mark_noop = matches!(
                sub.kind,
                SubtableKind::MarkBasePos1 | SubtableKind::MarkLigPos1 | SubtableKind::MarkMarkPos1
            );
            if !self.header {
                self.charge()?;
                if mark_noop {
                    self.header = true;
                    continue; // 不能仅信局部证书计数；仍对同一完整可达域逐条验证实际firstCoverage。
                }
                if !matches!(sub.kind, SubtableKind::PairPos1 | SubtableKind::PairPos2)
                    || read.u16(base + 6)? != 0
                    || read.u16(base + 4)? & !0xff != 0
                {
                    return Err(Rejection::Positioning);
                }
                self.classes = [GlyphSet([0; 1024]), GlyphSet([0; 1024])];
                self.class_count = 0;
                self.class_heads = [u16::MAX; MAX_REACHABLE];
                self.class_next = [u16::MAX; MAX_REACHABLE];
                self.mapped = false;
                self.header = true;
                continue;
            }
            if mark_noop {
                let Some(glyph) = self.glyphs.next(self.first) else {
                    self.next_subtable();
                    continue; // 完整域确实不交才沿原advance下界进入后继表，不增加或丢弃任何位置值。
                };
                self.charge()?;
                self.first = u32::from(glyph) + 1;
                if read
                    .coverage(read.offset(base, base + 2)?, glyph)?
                    .is_some()
                {
                    return Err(Rejection::Positioning);
                }
                continue; // 真不交的这一个glyph只花本条记录；quota1不暗扫余下集合。
            }
            let format = read.u16(base + 4)?;
            let record_bytes = (format.count_ones() as usize) * 2;
            if sub.kind == SubtableKind::PairPos1 {
                if let Some((set, count, index)) = self.pair_set {
                    if index == count {
                        self.pair_set = None;
                        continue;
                    }
                    self.charge()?;
                    let at = set + 2 + index * (2 + record_bytes);
                    let second = read.u16(at)?;
                    self.pair_set = Some((set, count, index + 1));
                    if self.glyphs.contains(second) {
                        self.value_record(
                            &read,
                            set,
                            at + 2,
                            format,
                            scale,
                            layout.ot.var_store.is_some(),
                            Target::Glyph(self.set_glyph),
                        )?;
                    }
                    continue;
                }
                let Some(first) = self.glyphs.next(self.first) else {
                    self.next_subtable();
                    continue;
                };
                self.charge()?;
                self.first = u32::from(first) + 1;
                self.set_glyph = self.glyph_ids[..self.count]
                    .binary_search(&first)
                    .map_err(|_| Rejection::LookupUnavailable)?;
                let Some(index) = read.coverage(read.offset(base, base + 2)?, first)? else {
                    continue;
                };
                if index >= usize::from(read.u16(base + 8)?) {
                    return Err(Rejection::LookupUnavailable);
                }
                let set = read.offset(base, base + 10 + index * 2)?;
                self.pair_set = Some((set, usize::from(read.u16(set)?), 0));
                continue;
            }
            if !self.mapped {
                if let Some(glyph) = self.glyphs.next(self.first) {
                    self.charge()?;
                    self.first = u32::from(glyph) + 1;
                    if read
                        .coverage(read.offset(base, base + 2)?, glyph)?
                        .is_some()
                    {
                        let class = read.class(read.offset(base, base + 8)?, glyph)?;
                        if class >= read.u16(base + 12)? {
                            return Err(Rejection::LookupUnavailable);
                        }
                        self.classes[0].insert(class);
                        let index = self.glyph_ids[..self.count]
                            .binary_search(&glyph)
                            .map_err(|_| Rejection::LookupUnavailable)?;
                        self.class_member(class, index)?;
                    }
                    let class = read.class(read.offset(base, base + 10)?, glyph)?;
                    if class >= read.u16(base + 14)? {
                        return Err(Rejection::LookupUnavailable);
                    }
                    self.classes[1].insert(class);
                    continue;
                }
                self.first = 0;
                self.second = 0;
                self.mapped = true;
                continue;
            }
            let Some(first) = self.classes[0].next(self.first) else {
                self.next_subtable();
                continue;
            };
            let Some(second) = self.classes[1].next(self.second) else {
                self.first = u32::from(first) + 1;
                self.second = 0;
                continue;
            };
            self.charge()?;
            self.second = u32::from(second) + 1;
            let row = usize::from(first) * usize::from(read.u16(base + 14)?) + usize::from(second);
            let at = (base + 16)
                .checked_add(
                    row.checked_mul(record_bytes)
                        .ok_or(Rejection::LookupUnavailable)?,
                )
                .ok_or(Rejection::LookupUnavailable)?;
            self.value_record(
                &read,
                base,
                at,
                format,
                scale,
                layout.ot.var_store.is_some(),
                Target::Class(first),
            )?;
        }
        Ok(None)
    }
}
