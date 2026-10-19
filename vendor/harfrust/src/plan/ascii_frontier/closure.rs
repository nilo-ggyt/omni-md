//! 单输入查表的保守非缺字闭包；cmap覆盖不能代替实际可达替换输出证明。
use super::*;
use read_fonts::{
    FontData, FontRead,
    tables::gsub::{
        AlternateSubstFormat1, MultipleSubstFormat1, SingleSubstFormat1, SingleSubstFormat2,
    },
};

/// 固定字形编号域的实际查表事实，不是当前Source或软行权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AsciiGlyphClosure {
    /// 保守包含feature开/关和所有alternate分支的可达glyph数。
    pub reachable_glyphs: usize,
    /// 实际读取的nominal、coverage与输出记录总数，不包含正文。
    pub checked_records: usize,
}

#[derive(Clone)]
pub(super) struct GlyphSet(pub(super) [u64; 1024]);
impl GlyphSet {
    pub(super) fn insert(&mut self, glyph: u16) {
        self.0[usize::from(glyph) / 64] |= 1_u64 << (glyph % 64);
    }
    pub(super) fn contains(&self, glyph: u16) -> bool {
        self.0[usize::from(glyph) / 64] & (1_u64 << (glyph % 64)) != 0
    }
    pub(super) fn next(&self, from: u32) -> Option<u16> {
        if from > u32::from(u16::MAX) {
            return None;
        }
        let mut word = from as usize / 64;
        let mut bits = self.0[word] & (u64::MAX << (from % 64));
        loop {
            if bits != 0 {
                return Some((word * 64 + bits.trailing_zeros() as usize) as u16);
            }
            word += 1;
            if word == self.0.len() {
                return None;
            }
            bits = self.0[word];
        } // 只查固定65536编号域，不按正文长度创建glyph表。
    }
}

struct Output {
    glyph: u16,
    index: usize,
    next: usize,
}

/// 同一计划/字体的有限游标；两个固定8KiB bitset不会随输入文本或实际glyph数增加。
///
/// 调用方须在所有step中保持原计划与字体不变。这个元数据游标本身不签Source权限，
/// Core另以原Font/plan owner和取消位约束其整个生命期。
pub struct AsciiGlyphClosureJob {
    current: GlyphSet,
    next: GlyphSet,
    scalar: u32,
    stage: usize,
    lookup: usize,
    subtable: usize,
    glyph: u32,
    lookup_started: bool,
    output: Option<Output>,
    records: usize,
    complete: Option<AsciiGlyphClosure>,
    failed: bool,
}
impl ShapePlan {
    /// 构造前先核原单输入计划；不在这里提前扫描95字符或输出表。
    ///
    /// # Errors
    /// 原计划含未证明依赖时拒绝，不通过改features获得能力。
    pub fn ascii_nonmissing_closure_job(
        &self,
        font: &Font,
    ) -> Result<AsciiGlyphClosureJob, AsciiFrontierRejection> {
        self.ascii_frontier_capability(font)?;
        Ok(AsciiGlyphClosureJob {
            current: GlyphSet([0; 1024]),
            next: GlyphSet([0; 1024]),
            scalar: 0x20,
            stage: 0,
            lookup: 0,
            subtable: 0,
            glyph: 0,
            lookup_started: false,
            output: None,
            records: 0,
            complete: None,
            failed: false,
        })
    }
}
impl AsciiGlyphClosureJob {
    /// 失败也保留真实已访问记录数；观察者不把Err当作0工作量或完整闭包。
    pub fn checked_records(&self) -> usize {
        self.records
    }

    /// 至多处理给定数量的真实记录；Some只在同计划全部lookup结束后产生。
    ///
    /// # Errors
    /// 零预算、可达空/0/越界输出、坏表或超过4096记录永久撤权。每lookup只读取开始时
    /// current集合；next保留旧glyph覆盖feature关闭/忽略路径，所有alternate均保守加入。
    pub fn step(
        &mut self,
        plan: &ShapePlan,
        font: &Font,
        budget: usize,
    ) -> Result<Option<AsciiGlyphClosure>, AsciiFrontierRejection> {
        if self.failed {
            return Err(AsciiFrontierRejection::LookupUnavailable);
        }
        let result = self.advance(plan, font, budget);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn charge(&mut self) -> Result<(), AsciiFrontierRejection> {
        self.records += 1;
        if self.records > 4096 {
            return Err(AsciiFrontierRejection::MetadataBudget);
        }
        Ok(())
    }
    fn advance(
        &mut self,
        plan: &ShapePlan,
        font: &Font,
        budget: usize,
    ) -> Result<Option<AsciiGlyphClosure>, AsciiFrontierRejection> {
        if budget == 0 {
            return Err(AsciiFrontierRejection::MetadataBudget);
        }
        if self.complete.is_some() {
            return Ok(self.complete);
        }
        let start = self.records;
        let shaper = ShaperFont::new(font);
        let layout = shaper.layout();
        while self.records - start < budget {
            if self.scalar <= 0x7e {
                self.charge()?;
                let glyph = shaper
                    .default_nominal_glyph(self.scalar)
                    .ok_or(AsciiFrontierRejection::MissingGlyph)?
                    .to_u32();
                if glyph == 0 || glyph >= font.num_glyphs() || glyph > u32::from(u16::MAX) {
                    return Err(AsciiFrontierRejection::MissingGlyph);
                }
                self.current.insert(glyph as u16);
                self.scalar += 1;
                continue; // 单个实际cmap查询独立计费；尚未查到的ASCII不宣称被覆盖。
            }
            if self.stage == plan.ot_map.stages(LayoutTableKind::Gsub).len() {
                let facts = AsciiGlyphClosure {
                    reachable_glyphs: self
                        .current
                        .0
                        .iter()
                        .map(|word| word.count_ones() as usize)
                        .sum(),
                    checked_records: self.records,
                };
                self.complete = Some(facts);
                return Ok(Some(facts));
            }
            let lookups = plan.ot_map.stage_lookups(LayoutTableKind::Gsub, self.stage);
            let Some(lookup) = lookups.get(self.lookup) else {
                self.stage += 1;
                self.lookup = 0;
                continue; // 空stage仍受原capability的256 lookup/1024 subtable上限。
            };
            if !self.lookup_started {
                self.next = self.current.clone();
                self.lookup_started = true;
            }
            let (bytes, actual) = layout
                .ot
                .table_data_and_lookup(LayoutTableKind::Gsub, lookup.index)
                .ok_or(AsciiFrontierRejection::LookupUnavailable)?;
            let Some(subtable) = actual.subtables.get(self.subtable) else {
                self.current = self.next.clone();
                self.lookup += 1;
                self.subtable = 0;
                self.glyph = 0;
                self.lookup_started = false;
                continue; // 此lookup的新输出只交下一lookup，不能在同一次查表中自反馈。
            };
            let data = FontData::new(
                bytes
                    .get(subtable.offset as usize..)
                    .ok_or(AsciiFrontierRejection::LookupUnavailable)?,
            );
            macro_rules! table {
                ($ty:ty) => {
                    <$ty>::read(data).map_err(|_| AsciiFrontierRejection::LookupUnavailable)?
                };
            }
            if let Some(output) = &mut self.output {
                let glyph = match subtable.kind {
                    SubtableKind::SingleSubst1 => {
                        let table = table!(SingleSubstFormat1);
                        Some((i32::from(output.glyph) + i32::from(table.delta_glyph_id())) as u16)
                    }
                    SubtableKind::SingleSubst2 => table!(SingleSubstFormat2)
                        .substitute_glyph_ids()
                        .get(output.index)
                        .map(|value| value.get().to_u16()),
                    SubtableKind::MultipleSubst1 => table!(MultipleSubstFormat1)
                        .sequences()
                        .get(output.index)
                        .map_err(|_| AsciiFrontierRejection::LookupUnavailable)?
                        .substitute_glyph_ids()
                        .get(output.next)
                        .map(|value| value.get().to_u16()),
                    SubtableKind::AlternateSubst1 => table!(AlternateSubstFormat1)
                        .alternate_sets()
                        .get(output.index)
                        .map_err(|_| AsciiFrontierRejection::LookupUnavailable)?
                        .alternate_glyph_ids()
                        .get(output.next)
                        .map(|value| value.get().to_u16()),
                    _ => return Err(AsciiFrontierRejection::CrossInputSubtable),
                };
                // 单输出没有第二项；数组须至少有一项。实际输出查询连同空结束检查都计费。
                let is_single = matches!(
                    subtable.kind,
                    SubtableKind::SingleSubst1 | SubtableKind::SingleSubst2
                );
                let nth = output.next;
                self.charge()?;
                if let Some(glyph) = glyph.filter(|_| !is_single || nth == 0) {
                    if glyph == 0 || u32::from(glyph) >= font.num_glyphs() {
                        return Err(AsciiFrontierRejection::MissingGlyph);
                    }
                    self.next.insert(glyph);
                    self.output
                        .as_mut()
                        .ok_or(AsciiFrontierRejection::LookupUnavailable)?
                        .next += 1;
                } else if nth == 0 {
                    return Err(AsciiFrontierRejection::MissingGlyph); // 可达删除不能当作非缺字。
                } else {
                    self.output = None;
                }
                continue;
            }
            let Some(glyph) = self.current.next(self.glyph) else {
                self.subtable += 1;
                self.glyph = 0;
                continue;
            };
            self.glyph = u32::from(glyph) + 1;
            self.charge()?;
            let index = match subtable.kind {
                SubtableKind::SingleSubst1 => table!(SingleSubstFormat1).coverage(),
                SubtableKind::SingleSubst2 => table!(SingleSubstFormat2).coverage(),
                SubtableKind::MultipleSubst1 => table!(MultipleSubstFormat1).coverage(),
                SubtableKind::AlternateSubst1 => table!(AlternateSubstFormat1).coverage(),
                _ => return Err(AsciiFrontierRejection::CrossInputSubtable),
            }
            .map_err(|_| AsciiFrontierRejection::LookupUnavailable)?
            .get(crate::GlyphId::new(u32::from(glyph)));
            if let Some(index) = index {
                self.output = Some(Output {
                    glyph,
                    index: usize::from(index),
                    next: 0,
                });
            } // 使用真实coverage.get返回的index，不假设坏表的覆盖序号必连续。
        }
        Ok(None)
    }
}
