//! 实际活动计划的单字素独立性元数据；不修改GSUB、tracking或字形结果。
use super::{AsciiFrontierRejection as Rejection, ShapePlan};
use crate::{
    Direction, Font, ShaperFont,
    normalize::NormalizationMode,
    ot::{layout::LayoutTableKind, lookup::SubtableKind},
    unicode::{CharExt, GeneralCategory},
};

/// 完成游标只交付固定事实；原Source/GCB/Font owner仍须由上层封闭认证。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SimpleGcbFrontierCapability {
    /// 原计划会按原grapheme算法施加tracking，不重算或关闭它。
    pub tracking: bool,
    /// 所有被实际检查的stage、lookup与子表记录数。
    pub checked_records: usize,
}

/// 仅固定游标；一个step最多读取budget条真实活动元数据，没有正文或glyph数组。
pub struct SimpleGcbFrontierJob {
    stage: usize,
    lookup: usize,
    subtable: usize,
    stage_checked: bool,
    lookup_checked: bool,
    records: usize,
    complete: Option<SimpleGcbFrontierCapability>,
    failed: bool,
}

impl ShapePlan {
    /// 创建尚未认证的原计划游标；真实单scalar字素与normalizer输入由调用方另证。
    ///
    /// # Errors
    /// 自定义文本回调、跨输入位置/AAT、fraction或非默认normalizer均不由此窄证书认证。
    pub fn simple_gcb_frontier_job(&self) -> Result<SimpleGcbFrontierJob, Rejection> {
        if self.direction != Direction::LeftToRight {
            return Err(Rejection::Direction);
        }
        let shaper = self.shaper;
        if shaper.preprocess_text.is_some()
            || shaper.postprocess_glyphs.is_some()
            || shaper.setup_masks.is_some()
            || shaper.reorder_marks.is_some()
            || shaper.decompose.is_some()
            || shaper.compose.is_some()
            || !matches!(
                shaper.normalization_preference,
                NormalizationMode::Auto
                    | NormalizationMode::ComposedDiacritics
                    | NormalizationMode::None
            )
        {
            return Err(Rejection::Shaper);
        }
        if self.apply_gpos || self.apply_morx || self.apply_kerx || self.apply_kern || self.has_vert
        {
            return Err(Rejection::Positioning);
        }
        if self.has_frac {
            return Err(Rejection::LookupBehavior); // 此分支不借当前字素猜未读尾部的自动fraction掩码。
        }
        Ok(SimpleGcbFrontierJob {
            stage: 0,
            lookup: 0,
            subtable: 0,
            stage_checked: false,
            lookup_checked: false,
            records: 0,
            complete: None,
            failed: false,
        })
    }
}

impl SimpleGcbFrontierJob {
    /// 已消费的真实记录，包括失败前工作；不能从这些公开计数构造Source权限。
    pub fn checked_records(&self) -> usize {
        self.records
    }

    /// 每条stage、lookup、子表和最终tracking存在性分别计费，quota1也可暂停。
    ///
    /// # Errors
    /// 任一真实活动lookup不是单输入1→1、含反向/随机/音节行为或缓存不可读时永久失败。
    pub fn step(
        &mut self,
        plan: &ShapePlan,
        font: &Font,
        budget: usize,
    ) -> Result<Option<SimpleGcbFrontierCapability>, Rejection> {
        if self.failed {
            return Err(Rejection::LookupUnavailable);
        }
        let result = self.advance(plan, font, budget);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn advance(
        &mut self,
        plan: &ShapePlan,
        font: &Font,
        budget: usize,
    ) -> Result<Option<SimpleGcbFrontierCapability>, Rejection> {
        if budget == 0 {
            return Err(Rejection::MetadataBudget);
        }
        if self.complete.is_some() {
            return Ok(self.complete);
        }
        let shaping = ShaperFont::new(font);
        let layout = shaping.layout();
        let begin = self.records;
        while self.records - begin < budget {
            let Some(stage) = plan.ot_map.stages(LayoutTableKind::Gsub).get(self.stage) else {
                self.records += 1;
                if plan.apply_trak && layout.aat.trak.is_none() {
                    return Err(Rejection::LookupUnavailable);
                }
                let capability = SimpleGcbFrontierCapability {
                    tracking: plan.apply_trak,
                    checked_records: self.records,
                };
                self.complete = Some(capability);
                return Ok(Some(capability));
            };
            if !self.stage_checked {
                self.records += 1;
                if stage.pause_func.is_some() {
                    return Err(Rejection::StageCallback);
                }
                self.stage_checked = true;
                continue;
            }
            let Some(lookup) = plan
                .ot_map
                .stage_lookups(LayoutTableKind::Gsub, self.stage)
                .get(self.lookup)
            else {
                self.stage += 1;
                self.lookup = 0;
                self.subtable = 0;
                self.stage_checked = false;
                self.lookup_checked = false;
                continue;
            };
            let (_, actual) = layout
                .ot
                .table_data_and_lookup(LayoutTableKind::Gsub, lookup.index)
                .ok_or(Rejection::LookupUnavailable)?;
            if !self.lookup_checked {
                self.records += 1;
                if lookup.random || lookup.per_syllable || actual.is_reversed || !actual.is_subst {
                    return Err(Rejection::LookupBehavior);
                }
                self.lookup_checked = true;
                continue;
            }
            let Some(sub) = actual.subtables.get(self.subtable) else {
                self.lookup += 1;
                self.subtable = 0;
                self.lookup_checked = false;
                continue;
            };
            self.records += 1;
            if !matches!(
                sub.kind,
                SubtableKind::SingleSubst1
                    | SubtableKind::SingleSubst2
                    | SubtableKind::AlternateSubst1
            ) {
                return Err(Rejection::CrossInputSubtable);
            } // Alternate只选其中一个glyph；Multiple/ligature/context都不能偷当1→1。
            self.subtable += 1;
        }
        Ok(None)
    }
}

/// 核原normalizer的真实nominal短路输入；这里只交付一个glyph事实，不宣称shape无缺字。
///
/// # Errors
/// mark、hidden/default-ignorable、非普通空格的控制字符或缺少真实nominal映射均保未知。
pub fn simple_gcb_nominal(font: &Font, scalar: char) -> Result<u32, Rejection> {
    let point = u32::from(scalar);
    let category = point.general_category();
    if point.is_default_ignorable()
        || !(category.is_letter()
            || category.in_range_inclusive(
                GeneralCategory::DECIMAL_NUMBER,
                GeneralCategory::OPEN_PUNCTUATION,
            )
            || category == GeneralCategory::SPACE_SEPARATOR)
        || category.is_mark()
        || point.combining_class() != 0
    {
        return Err(Rejection::LookupBehavior);
    } // 数字仅在已排除fraction的计划内成立；emoji/IVS及控制格式字符不在此域。
    let shaping = ShaperFont::new(font);
    let glyph = shaping
        .default_nominal_glyph(point)
        .ok_or(Rejection::MissingGlyph)?
        .to_u32();
    if glyph == 0 || glyph >= font.num_glyphs() {
        return Err(Rejection::MissingGlyph);
    }
    Ok(glyph)
}

/// 真实shape后仍核最终glyph属性；输入的Unicode类别不能替代GSUB后的GDEF事实。
pub fn simple_gcb_output_is_base(font: &Font, glyph: u32) -> bool {
    let Ok(glyph) = u16::try_from(glyph) else {
        return false;
    };
    glyph != 0
        && u32::from(glyph) < font.num_glyphs()
        && ShaperFont::new(font).layout().ot.glyph_props(glyph.into()) & 0x0c == 0
}
