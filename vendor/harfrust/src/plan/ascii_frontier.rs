//! 只读检查实际已编译计划，不执行或修改字形算法；源范围权限由调用方另行认证。

use super::ShapePlan;
use crate::{ot::layout::LayoutTableKind, ot::lookup::SubtableKind, Direction, Font, ShaperFont};

mod closure;
mod local;
pub use closure::{AsciiGlyphClosure, AsciiGlyphClosureJob};
pub use local::{
    AsciiLocalFrontierCapability, AsciiLocalFrontierCertificate, AsciiLocalFrontierJob,
    AsciiPositionedAdvance, AsciiPositionedAdvanceJob,
};

/// 未认证只是本窄优化不适用；调用方仍须保留原字体与完整整形功能。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AsciiFrontierRejection {
    /// 本窄证书尚未证明这个方向。
    Direction,
    /// 脚本shaper含可变文本、掩码或规范化回调。
    Shaper,
    /// 有真实跨glyph位置、AAT或tracking阶段。
    Positioning,
    /// 查表之间有可以改写buffer的回调。
    StageCallback,
    /// 查表需要随机、音节或反向行为。
    LookupBehavior,
    /// 实际编译索引未取得可核查的缓存。
    LookupUnavailable,
    /// 实际子表读取不止一个输入glyph或未被本证书证明。
    CrossInputSubtable,
    /// 计划元数据超过本次证书的固定检查预算。
    MetadataBudget,
    /// 至少一个printable ASCII无真实nominal glyph或可达替换产生空/0输出。
    MissingGlyph,
}

/// 实际计划的有限元数据事实。它不是Source、字素、缺字或软行就绪证明。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AsciiFrontierCapability {
    /// 按实际执行stage顺序检查的查表次数。
    pub lookups: usize,
    /// 已解析extension后的实际单输入子表次数。
    pub subtables: usize,
    /// fraction只在源中真实闭合的非Decimal端点处独立，不能从此布尔值省略源检查。
    pub requires_closed_decimal_seam: bool,
}

impl ShapePlan {
    /// 检查同一实际Font/轴位所编译计划是否只含单输入GSUB。
    ///
    /// # Errors
    /// GPOS/AAT/trak、跨输入查表、可变回调和超预算元数据均明确拒绝。调用方必须使用
    /// 同Font、方向、脚本、语言和features执行本计划；仍须证明真实ASCII及两端GCB。
    /// 本方法不从字体名称推导能力，也不把UNSAFE_TO_CONCAT缺位当作未读尾部证明。
    pub fn ascii_frontier_capability(
        &self,
        font: &Font,
    ) -> Result<AsciiFrontierCapability, AsciiFrontierRejection> {
        use AsciiFrontierRejection as Rejection;
        if self.direction != Direction::LeftToRight {
            return Err(Rejection::Direction); // 首切片只认证水平正向，不猜反向输出区间。
        }
        if self.shaper.preprocess_text.is_some()
            || self.shaper.postprocess_glyphs.is_some()
            || self.shaper.setup_masks.is_some()
            || self.shaper.reorder_marks.is_some()
            || self.shaper.decompose.is_some()
            || self.shaper.compose.is_some()
        {
            // 只接受已核无文本重排/掩码回调的默认shaper；复杂脚本继续原完整算法。
            return Err(Rejection::Shaper);
        }
        if self.apply_gpos
            || self.apply_morx
            || self.apply_kerx
            || self.apply_kern
            || self.apply_trak
        {
            return Err(Rejection::Positioning); // 不把单输入替换证明扩展成位置依赖证明。
        }
        let shaper = ShaperFont::new(font);
        let layout = shaper.layout();
        let mut capability = AsciiFrontierCapability {
            lookups: 0,
            subtables: 0,
            requires_closed_decimal_seam: self.has_frac,
        };
        for (stage, entry) in self.ot_map.stages(LayoutTableKind::Gsub).iter().enumerate() {
            if entry.pause_func.is_some() {
                return Err(Rejection::StageCallback); // 暂停回调可能重写任意glyph，不能忽略。
            }
            for lookup in self.ot_map.stage_lookups(LayoutTableKind::Gsub, stage) {
                capability.lookups += 1;
                if capability.lookups > 256 {
                    return Err(Rejection::MetadataBudget); // 元数据本身也不能形成无界扫描。
                }
                if lookup.random || lookup.per_syllable {
                    return Err(Rejection::LookupBehavior);
                }
                let (_, actual) = layout
                    .ot
                    .table_data_and_lookup(LayoutTableKind::Gsub, lookup.index)
                    .ok_or(Rejection::LookupUnavailable)?;
                if !actual.is_subst || actual.is_reversed {
                    return Err(Rejection::LookupBehavior);
                }
                for subtable in &actual.subtables {
                    capability.subtables += 1;
                    if capability.subtables > 1024 {
                        return Err(Rejection::MetadataBudget);
                    }
                    if !matches!(
                        subtable.kind,
                        SubtableKind::SingleSubst1
                            | SubtableKind::SingleSubst2
                            | SubtableKind::MultipleSubst1
                            | SubtableKind::AlternateSubst1
                    ) {
                        // 缓存kind已解析extension；context、ligature及未知类型不作乐观推断。
                        return Err(Rejection::CrossInputSubtable);
                    }
                }
            }
        }
        // apply_fallback_kern当前明确为STUB；fallback_mark只处理Unicode mark/hidden/
        // ignorable链。调用方逐字节证明printable ASCII后，这两条不引入跨源依赖。
        Ok(capability)
    }
}
