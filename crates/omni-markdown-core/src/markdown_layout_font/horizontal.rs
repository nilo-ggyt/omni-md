//! 同一实际平台Font的完整水平推进快照；只覆盖水平metrics，不修改cmap、GSUB或定位算法。

use harfrust::{Direction, Font, FontFuncs, GlyphId, ShaperFont};

use super::{MarkdownLayoutFontError as Error, Result, positioning};

/// 固定glyph域纯值归原Font payload独占；没有Source、Native强引用、闭包或全局缓存。
pub(super) struct HorizontalAdvances {
    scale: i32,
    values: Box<[i32]>,
}

impl HorizontalAdvances {
    /// 平台数据已按原glyph顺序读取；解析出的maxp域仍是数量权威，调用方不能自行缩短。
    /// 原双精度point推进只在此转为与GPOS/trak相同的细尺度，公开26.6继续由原出口统一量化。
    pub(super) fn new(font: &Font, points: f64, glyphs: u32, values: Box<[f64]>) -> Result<Self> {
        if glyphs == 0 || glyphs > 65_536 || values.len() != glyphs as usize {
            return Err(Error::InvalidHorizontalAdvances);
        }
        let (scale, divisor) = positioning::bound_scale(points)?;
        // Native 的完整名义推进已反映该实际字号/轴实例自己的默认 tracking 策略。
        // 按 HarfBuzz CoreText font bridge 的合同，在名义入口扣原 Font 真正会加的值；
        // 后续原定位仍按 GCB 加回它，既不关闭 kern/trak，也不按 glyph 或字体名称补差。
        // point_size 使用 shape 同一 f32 编码，尺度仍绑定原双精度字号，避免两次舍入相减。
        let tracking = ShaperFont::new(font)
            .with_scale(scale)
            .default_tracking_advance(Direction::LeftToRight, Some(points as f32));
        let mut converted = Vec::new();
        converted
            .try_reserve_exact(values.len())
            .map_err(|_| Error::HorizontalAdvanceAllocation)?;
        for value in values {
            let raw = (value * 64.0 * f64::from(divisor)).round();
            if !value.is_finite() || raw < f64::from(i32::MIN) || raw > f64::from(i32::MAX) {
                return Err(Error::InvalidHorizontalAdvances);
            } // 错误立即丢弃整个候选；不保留半张表，也不以hmtx或零值补缺项。
            // 真实负值不夹成零；原前沿会用同一回调和实际 tracking 证明能否提前发布。
            // 超出坐标范围拒绝整张候选表，不能让饱和运算改变 Native 名义度量。
            converted.push(
                (raw as i32)
                    .checked_sub(tracking)
                    .ok_or(Error::InvalidHorizontalAdvances)?,
            );
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
