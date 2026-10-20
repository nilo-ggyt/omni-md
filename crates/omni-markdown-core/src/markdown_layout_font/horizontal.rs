//! 同一实际平台Font的完整水平推进快照；只覆盖水平metrics，不修改cmap、GSUB或定位算法。

use harfrust::{FontFuncs, GlyphId, ShaperFont};

use super::{MarkdownLayoutFontError as Error, Result, positioning};

/// 固定glyph域纯值归原Font payload独占；没有Source、Native强引用、闭包或全局缓存。
pub(super) struct HorizontalAdvances {
    scale: i32,
    values: Box<[i32]>,
}

impl HorizontalAdvances {
    /// 平台数据已按原glyph顺序读取；解析出的maxp域仍是数量权威，调用方不能自行缩短。
    /// 原双精度point推进只在此转为与GPOS/trak相同的细尺度，公开26.6继续由原出口统一量化。
    pub(super) fn new(points: f64, glyphs: u32, values: Box<[f64]>) -> Result<Self> {
        if glyphs == 0 || glyphs > 65_536 || values.len() != glyphs as usize {
            return Err(Error::InvalidHorizontalAdvances);
        }
        let (scale, divisor) = positioning::bound_scale(points)?;
        let mut converted = Vec::new();
        converted
            .try_reserve_exact(values.len())
            .map_err(|_| Error::HorizontalAdvanceAllocation)?;
        for value in values {
            let raw = (value * 64.0 * f64::from(divisor)).round();
            if !value.is_finite() || raw < f64::from(i32::MIN) || raw > f64::from(i32::MAX) {
                return Err(Error::InvalidHorizontalAdvances);
            } // 错误立即丢弃整个候选；不保留半张表，也不以hmtx或零值补缺项。
            converted.push(raw as i32);
        }
        Ok(Self {
            scale,
            values: converted.into_boxed_slice(),
        })
    }

    /// 同一Font的snapshot只属于原绑定字号的实际尺度；相同face另一字号须重新捕获。
    pub(super) fn require_scale(&self, scale: i32) -> Result<()> {
        if scale != self.scale {
            return Err(Error::InvalidPointSize);
        }
        Ok(())
    }
}

impl FontFuncs for HorizontalAdvances {
    /// HarfRust在原已解析Font的glyph域内查询；非法glyph不具备平台快照索引权限。
    /// 只覆盖这一真实业务度量，批查询沿上游默认方法逐项调用，不额外持可变状态。
    fn glyph_h_advance(&self, _font: &ShaperFont<'_, '_>, glyph: GlyphId) -> i32 {
        self.values[glyph.to_u32() as usize]
    }
}
